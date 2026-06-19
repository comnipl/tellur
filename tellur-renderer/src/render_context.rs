//! LRU-backed implementation of [`tellur_core::render_context::RenderContext`].
//!
//! [`CachingRenderContext`] memoizes [`RasterImage`] outputs keyed by
//! `(TypeId, content_hash, size, target)`. The cache is bounded in
//! **bytes** (image pixel buffers), with a soft second limit driven by
//! system memory pressure: if the host's used-memory ratio climbs above
//! the threshold (default 90%), entries are evicted aggressively and
//! new entries are not inserted until pressure drops back.
//!
//! Pixel data lives in `Bytes` (Arc-backed), so cloning a cached
//! `RasterImage` does not copy the buffer — cache hits are cheap. The
//! Arc'd backing also means that downstream consumers (encoder, save,
//! further compositing) hold the same buffer the cache holds.

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hasher;
use std::time::{Duration, Instant};

use lru::LruCache;
use rustc_hash::FxHasher;
use sysinfo::System;
use tellur_core::cache_budget::{
    cache_ram_capacity, configured_vram_bytes, try_reserve_cache_ram, vram_used_bytes,
    BudgetReservation,
};
use tellur_core::dyn_compare::{DynEq, DynHash};
use tellur_core::geometry::Vec2;
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::{CachePolicy, GpuPreference, GpuRasterBackend, RenderContext};

use crate::gpu::{GpuRenderStats, GpuRenderer};

/// Default cache size in bytes (1 GiB) when constructed with
/// [`CachingRenderContext::new`].
pub const DEFAULT_CAPACITY_BYTES: usize = 1024 * 1024 * 1024;

/// System-memory utilization fraction above which the cache stops
/// admitting new entries and starts shedding existing ones.
pub const MEMORY_PRESSURE_THRESHOLD: f32 = 0.90;

const DEFAULT_PROBATION_ENTRIES: usize = 4096;
const VOLATILE_LARGE_IMAGE_BYTES: usize = 1024 * 1024;
const VOLATILE_MIN_MISSES: u64 = 8;
const VOLATILE_MAX_HIT_RATE: f64 = 0.75;
const GPU_CACHE_INITIAL_FRACTION_DIVISOR: usize = 4;
const GPU_CACHE_MAX_FRACTION_DIVISOR: usize = 2;
const GPU_CACHE_SHRINK_NUMERATOR: usize = 3;
const GPU_CACHE_SHRINK_DENOMINATOR: usize = 4;
const GPU_CACHE_GROW_SUCCESS_STREAK: u8 = 64;
const GPU_CACHE_GROW_FRACTION_DIVISOR: usize = 64;
const MIB: usize = 1024 * 1024;

/// Controls when a freshly-rendered image becomes a cache entry.
///
/// `Immediate` is the export-oriented default: every miss is admitted while
/// capacity allows. `SecondUse` admits only keys that appear at least twice.
/// `SkipVolatileLarge` starts with immediate admission, but stops admitting
/// large images for component types that build up many misses with a weak hit
/// rate. That keeps live previews from filling the cache with per-frame
/// full-screen animation states while still letting static large entries warm
/// immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CacheAdmissionPolicy {
    #[default]
    Immediate,
    SecondUse,
    SkipVolatileLarge,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    type_id: TypeId,
    content_hash: u64,
    // f32 fields are stored as their bit patterns so the key is `Eq`/`Hash`
    // (which `f32` is not, due to NaN inequality).
    size_x_bits: u32,
    size_y_bits: u32,
    target_width: u32,
    target_height: u32,
}

impl CacheKey {
    fn of(c: &dyn RasterComponent, type_id: TypeId, size: Vec2, target: Resolution) -> Self {
        // The cache key stores `type_id` separately, so only hash the
        // component's own fields here. Cache keys are internal and not exposed
        // to untrusted input, so a fast non-cryptographic hasher is appropriate
        // for this hot path.
        let mut hasher = FxHasher::default();
        DynHash::dyn_hash(c, &mut hasher);
        let content_hash = hasher.finish();
        Self {
            type_id,
            content_hash,
            size_x_bits: size.0.to_bits(),
            size_y_bits: size.1.to_bits(),
            target_width: target.width,
            target_height: target.height,
        }
    }
}

struct CachedRasterImage {
    image: RasterImage,
    bytes: usize,
    _ram_reservation: Option<BudgetReservation>,
}

impl CachedRasterImage {
    fn is_gpu(&self) -> bool {
        matches!(self.image, RasterImage::Gpu(_))
    }
}

/// Per-concrete-type hit/miss tally.
///
/// Surfaced as part of [`CacheMetrics::per_type`] so callers can spot
/// which component types are actually benefiting from memoization and
/// which keep missing every frame (typically a sign that something
/// "downstream" of the type — a `paint_bounds`-derived `Resolution`, a
/// timestamped field — is varying each call).
#[derive(Debug, Clone, Copy, Default)]
pub struct TypeStats {
    pub hits: u64,
    pub misses: u64,
    /// Wall-clock time spent inside `ctx.render` for this type,
    /// **including** all nested child renders called through the
    /// context. Dominated by whichever level of the tree the time was
    /// "really" spent at.
    pub inclusive_time: Duration,
    /// `inclusive_time` minus the time spent in nested `ctx.render`
    /// calls. Approximates the time genuinely consumed by this type's
    /// own `render` body plus cache bookkeeping for it — a good proxy
    /// for "is this layer the bottleneck?".
    pub self_time: Duration,
}

impl TypeStats {
    pub fn total(&self) -> u64 {
        self.hits + self.misses
    }

    pub fn hit_rate(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            0.0
        } else {
            self.hits as f64 / t as f64
        }
    }
}

/// Aggregate counters tracking what the cache is doing.
///
/// All numbers are cumulative since context construction (or the last
/// [`CachingRenderContext::clear_metrics`] call). Useful for confirming
/// that memoization is actually firing on a given timeline, and for
/// understanding why it isn't when it isn't.
#[derive(Debug, Clone, Default)]
pub struct CacheMetrics {
    /// Calls to [`RenderContext::render`] that returned a cached image.
    pub hits: u64,
    /// Calls that had to invoke the component's `render` method.
    pub misses: u64,
    /// Bytes currently held by the cache.
    pub bytes_cached: usize,
    /// Bytes released through LRU eviction (capacity-driven).
    pub bytes_evicted: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because system memory pressure was over threshold.
    pub pressure_skips: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because a single image exceeded the configured cap.
    pub oversize_skips: u64,
    /// Misses where the freshly-produced image was not admitted by the
    /// configured admission policy.
    pub admission_skips: u64,
    /// Misses where the freshly-produced image was not admitted because the
    /// process-wide cache RAM budget was exhausted.
    pub budget_skips: u64,
    /// Current GPU policy for this context.
    pub gpu_preference: GpuPreference,
    /// Whether the context has tried to create a GPU backend.
    pub gpu_init_attempted: bool,
    /// Whether a GPU backend is currently active.
    pub gpu_available: bool,
    /// GPU operation counters accumulated by the active backend.
    pub gpu: GpuRenderStats,
    /// Breakdown by the concrete `RasterComponent` type that was queried,
    /// keyed by display name (`std::any::type_name`).
    pub per_type: HashMap<&'static str, TypeStats>,
}

impl CacheMetrics {
    /// Hit rate as a fraction in `[0, 1]`. Returns `0.0` when no calls
    /// have been made yet.
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Multi-line display for `CacheMetrics`. Renders the totals on one row
/// and a per-type breakdown sorted by total call count (descending), so
/// the noisiest types lead. Suitable for `eprintln!("{}", metrics)` or
/// a log line at the end of an export.
impl fmt::Display for CacheMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Cache  {} hits / {} misses ({:.1}% hit) — {} cached, {} evicted, {} pressure skips, {} oversize skips, {} admission skips, {} budget skips",
            self.hits,
            self.misses,
            self.hit_rate() * 100.0,
            format_bytes(self.bytes_cached as u64),
            format_bytes(self.bytes_evicted),
            self.pressure_skips,
            self.oversize_skips,
            self.admission_skips,
            self.budget_skips,
        )?;
        writeln!(
            f,
            "GPU    preference={:?}, attempted={}, available={}, ops={} (composite {}, shadow {}, outline {}, rasterize {}, fill {}, temporal_avg {}, readback {}, vram_failures {}, cache_evictions {})",
            self.gpu_preference,
            self.gpu_init_attempted,
            self.gpu_available,
            self.gpu.total_ops(),
            self.gpu.composites,
            self.gpu.drop_shadows,
            self.gpu.outlines,
            self.gpu.rasterizes,
            self.gpu.fills,
            self.gpu.temporal_averages,
            self.gpu.readbacks,
            self.gpu.vram_reserve_failures,
            self.gpu.vram_cache_evictions,
        )?;
        if !self.per_type.is_empty() {
            writeln!(f, "Cache by type (sorted by self_time, descending):")?;
            // Sort by self_time so the type that's actually burning
            // CPU shows up first; that's almost always the question
            // the user is trying to answer when they look at this.
            let mut rows: Vec<(&&'static str, &TypeStats)> = self.per_type.iter().collect();
            rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.self_time));
            let name_w = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
            for (name, s) in rows {
                writeln!(
                    f,
                    "  {name:<name_w$}  {hits:>5} hits / {misses:>5} misses ({rate:>5.1}%)  self {self_t:>9}  incl {inc:>9}",
                    name = name,
                    name_w = name_w,
                    hits = s.hits,
                    misses = s.misses,
                    rate = s.hit_rate() * 100.0,
                    self_t = format_duration(s.self_time),
                    inc = format_duration(s.inclusive_time),
                )?;
            }
        }
        Ok(())
    }
}

fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros >= 1_000_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else if micros >= 1_000 {
        format!("{:.2}ms", micros as f64 / 1_000.0)
    } else {
        format!("{micros}µs")
    }
}

fn format_bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bf = b as f64;
    if bf >= GIB {
        format!("{:.2} GiB", bf / GIB)
    } else if bf >= MIB {
        format!("{:.2} MiB", bf / MIB)
    } else if bf >= KIB {
        format!("{:.2} KiB", bf / KIB)
    } else {
        format!("{b} B")
    }
}

fn pixel_stride(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Rgba8 => 4,
        PixelFormat::Rgba16Float => 8,
    }
}

/// A render context that memoizes `RasterImage` outputs.
///
/// Construct one per export / preview session and pass it into
/// [`tellur_core::timeline::Timeline::build`]; the cache persists across
/// frames so any time-invariant subtree only re-renders once.
pub struct CachingRenderContext {
    cache: LruCache<CacheKey, CachedRasterImage>,
    probation: LruCache<CacheKey, ()>,
    cur_bytes: usize,
    cpu_cache_bytes: usize,
    cap_bytes: usize,
    gpu_cache_bytes: usize,
    gpu_cache_cap_bytes: usize,
    gpu_cache_max_bytes: usize,
    gpu_cache_spare_successes: u8,
    gpu_vram_failures_seen: u64,
    system: System,
    // Aggregate counters; `per_type` is keyed by `TypeId` for cheap
    // updates inside `render`, then projected onto `&'static str` names
    // when the user calls `metrics()`.
    hits: u64,
    misses: u64,
    bytes_evicted: u64,
    pressure_skips: u64,
    oversize_skips: u64,
    admission_skips: u64,
    budget_skips: u64,
    per_type: HashMap<TypeId, (TypeStats, &'static str)>,
    admission_policy: CacheAdmissionPolicy,
    gpu_preference: GpuPreference,
    gpu: Option<GpuRenderer>,
    gpu_init_attempted: bool,
    gpu_init_error: Option<String>,
    motion_blur_enabled: bool,
    // Running total of every `ctx.render` call's inclusive duration.
    // A `render` invocation snapshots this on entry and re-reads it on
    // exit to derive how much time was spent inside nested child
    // renders; the difference between elapsed and that delta is the
    // current frame's "self time" contribution.
    total_render_time: Duration,
}

impl CachingRenderContext {
    /// Create a context with the default capacity ([`DEFAULT_CAPACITY_BYTES`]).
    pub fn new() -> Self {
        Self::with_capacity_bytes(DEFAULT_CAPACITY_BYTES)
    }

    /// Create a context with a custom byte capacity.
    pub fn with_capacity_bytes(cap_bytes: usize) -> Self {
        let (gpu_cache_cap_bytes, gpu_cache_max_bytes) = gpu_cache_limits();
        Self {
            cache: LruCache::unbounded(),
            probation: LruCache::unbounded(),
            cur_bytes: 0,
            cpu_cache_bytes: 0,
            cap_bytes: cache_ram_capacity(cap_bytes),
            gpu_cache_bytes: 0,
            gpu_cache_cap_bytes,
            gpu_cache_max_bytes,
            gpu_cache_spare_successes: 0,
            gpu_vram_failures_seen: 0,
            system: System::new(),
            hits: 0,
            misses: 0,
            bytes_evicted: 0,
            pressure_skips: 0,
            oversize_skips: 0,
            admission_skips: 0,
            budget_skips: 0,
            per_type: HashMap::new(),
            admission_policy: CacheAdmissionPolicy::Immediate,
            gpu_preference: GpuPreference::Auto,
            gpu: None,
            gpu_init_attempted: false,
            gpu_init_error: None,
            motion_blur_enabled: true,
            total_render_time: Duration::ZERO,
        }
    }

    pub fn with_gpu_preference(mut self, gpu_preference: GpuPreference) -> Self {
        self.gpu_preference = gpu_preference;
        self
    }

    pub fn with_cache_admission_policy(mut self, policy: CacheAdmissionPolicy) -> Self {
        self.admission_policy = policy;
        self
    }

    pub fn with_second_use_admission(self) -> Self {
        self.with_cache_admission_policy(CacheAdmissionPolicy::SecondUse)
    }

    pub fn with_volatile_large_admission(self) -> Self {
        self.with_cache_admission_policy(CacheAdmissionPolicy::SkipVolatileLarge)
    }

    pub fn set_gpu_preference(&mut self, gpu_preference: GpuPreference) {
        self.gpu_preference = gpu_preference;
    }

    /// Toggles the [`RenderContext::motion_blur_enabled`] policy signal.
    ///
    /// Cache-safe in either direction: temporal effects average their child
    /// frames at the IMAGE layer (never through a `ctx.render` cache slot),
    /// so flipping the toggle changes which sub-frames get rendered but
    /// never aliases a blurred result with an unblurred cache entry.
    pub fn set_motion_blur_enabled(&mut self, enabled: bool) {
        self.motion_blur_enabled = enabled;
    }

    fn gpu_backend_mut(&mut self) -> Option<&mut GpuRenderer> {
        if self.gpu.is_some() {
            return self.gpu.as_mut();
        }
        if !self.gpu_preference.prefers_gpu() || self.gpu_init_attempted {
            return None;
        }
        self.gpu_init_attempted = true;
        match GpuRenderer::new() {
            Ok(gpu) => {
                self.gpu_init_error = None;
                self.gpu = Some(gpu);
            }
            Err(err) => {
                self.gpu_init_error = Some(err);
            }
        }
        self.gpu.as_mut()
    }

    /// Current memory footprint of cached images, in bytes.
    pub fn current_bytes(&self) -> usize {
        self.cur_bytes
    }

    /// Configured maximum capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.cap_bytes
    }

    /// The last GPU initialization error, when backend creation was attempted
    /// but no renderer is available.
    pub fn gpu_init_error(&self) -> Option<&str> {
        self.gpu_init_error.as_deref()
    }

    /// A snapshot of the cumulative cache counters. The per-type table
    /// is projected from `TypeId` onto `&'static str` names at this
    /// point so the returned struct can be displayed or logged
    /// independently of the context.
    pub fn metrics(&self) -> CacheMetrics {
        let per_type = self
            .per_type
            .values()
            .map(|(stats, name)| (*name, *stats))
            .collect();
        CacheMetrics {
            hits: self.hits,
            misses: self.misses,
            bytes_cached: self.cur_bytes,
            bytes_evicted: self.bytes_evicted,
            pressure_skips: self.pressure_skips,
            oversize_skips: self.oversize_skips,
            admission_skips: self.admission_skips,
            budget_skips: self.budget_skips,
            gpu_preference: self.gpu_preference,
            gpu_init_attempted: self.gpu_init_attempted,
            gpu_available: self.gpu.is_some(),
            gpu: self
                .gpu
                .as_ref()
                .map(GpuRenderer::stats)
                .unwrap_or_default(),
            per_type,
        }
    }

    /// Reset the counters (does not flush the cache itself).
    pub fn clear_metrics(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.bytes_evicted = 0;
        self.pressure_skips = 0;
        self.oversize_skips = 0;
        self.admission_skips = 0;
        self.budget_skips = 0;
        self.per_type.clear();
        self.total_render_time = Duration::ZERO;
    }

    /// Drop all cached entries.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.probation.clear();
        self.cur_bytes = 0;
        self.cpu_cache_bytes = 0;
        self.gpu_cache_bytes = 0;
    }

    fn should_admit(&mut self, key: CacheKey, type_id: TypeId, bytes: usize) -> bool {
        match self.admission_policy {
            CacheAdmissionPolicy::Immediate => true,
            CacheAdmissionPolicy::SecondUse => {
                if self.probation.pop(&key).is_some() {
                    true
                } else {
                    self.probation.put(key, ());
                    while self.probation.len() > DEFAULT_PROBATION_ENTRIES {
                        let _ = self.probation.pop_lru();
                    }
                    self.admission_skips = self.admission_skips.saturating_add(1);
                    false
                }
            }
            CacheAdmissionPolicy::SkipVolatileLarge => {
                if bytes < VOLATILE_LARGE_IMAGE_BYTES {
                    return true;
                }
                let volatile = self
                    .per_type
                    .get(&type_id)
                    .map(|(stats, _)| {
                        stats.misses >= VOLATILE_MIN_MISSES
                            && stats.hit_rate() <= VOLATILE_MAX_HIT_RATE
                    })
                    .unwrap_or(false);
                if volatile {
                    self.admission_skips = self.admission_skips.saturating_add(1);
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Refresh and check system-wide memory utilization.
    fn under_memory_pressure(&mut self) -> bool {
        self.system.refresh_memory();
        let total = self.system.total_memory();
        if total == 0 {
            return false;
        }
        // Compare in u64 space to avoid f32 precision quirks at large RAM sizes.
        let used = self.system.used_memory();
        // used / total > 0.90  ⇔  used * 100 > total * 90
        used.saturating_mul(100) > total.saturating_mul(90)
    }

    fn image_bytes(image: &RasterImage) -> usize {
        match image {
            RasterImage::Cpu(image) => image.pixels.len(),
            RasterImage::Gpu(surface) => {
                (surface.width as usize) * (surface.height as usize) * pixel_stride(surface.format)
            }
        }
    }

    fn cache_entry(image: RasterImage, bytes: usize) -> Option<CachedRasterImage> {
        let ram_reservation = match &image {
            RasterImage::Cpu(_) => Some(try_reserve_cache_ram(bytes)?),
            RasterImage::Gpu(_) => None,
        };
        Some(CachedRasterImage {
            image,
            bytes,
            _ram_reservation: ram_reservation,
        })
    }

    fn account_evicted(&mut self, entry: &CachedRasterImage) {
        self.cur_bytes = self.cur_bytes.saturating_sub(entry.bytes);
        if entry.is_gpu() {
            self.gpu_cache_bytes = self.gpu_cache_bytes.saturating_sub(entry.bytes);
        } else {
            self.cpu_cache_bytes = self.cpu_cache_bytes.saturating_sub(entry.bytes);
        }
        self.bytes_evicted = self.bytes_evicted.saturating_add(entry.bytes as u64);
    }

    fn evict_gpu_cache_to_fit(&mut self, needed: usize) {
        if self.gpu_cache_bytes.saturating_add(needed) <= self.gpu_cache_cap_bytes {
            return;
        }

        let mut keep = Vec::with_capacity(self.cache.len());
        while self.gpu_cache_bytes.saturating_add(needed) > self.gpu_cache_cap_bytes {
            match self.cache.pop_lru() {
                Some((_, entry)) if entry.is_gpu() => self.account_evicted(&entry),
                Some((key, entry)) => keep.push((key, entry)),
                None => break,
            }
        }
        for (key, entry) in keep {
            self.cache.put(key, entry);
        }
    }

    /// Evict least-recently-used entries until `needed` more bytes fit
    /// under the configured cap.
    fn evict_cpu_cache_to_fit(&mut self, needed: usize) {
        if self.cpu_cache_bytes.saturating_add(needed) <= self.cap_bytes {
            return;
        }

        let mut keep = Vec::with_capacity(self.cache.len());
        while self.cpu_cache_bytes.saturating_add(needed) > self.cap_bytes {
            match self.cache.pop_lru() {
                Some((_, entry)) if !entry.is_gpu() => self.account_evicted(&entry),
                Some((key, entry)) => keep.push((key, entry)),
                None => break,
            }
        }
        for (key, entry) in keep {
            self.cache.put(key, entry);
        }
    }

    /// Evict entries until system memory pressure subsides or the cache
    /// is empty.
    fn shed_under_pressure(&mut self) {
        while self.under_memory_pressure() {
            let mut keep = Vec::with_capacity(self.cache.len());
            let mut evicted = false;
            match self.cache.pop_lru() {
                Some((_, entry)) if !entry.is_gpu() => {
                    self.account_evicted(&entry);
                    evicted = true;
                }
                Some((key, entry)) => keep.push((key, entry)),
                None => break,
            }
            while !evicted {
                match self.cache.pop_lru() {
                    Some((_, entry)) if !entry.is_gpu() => {
                        self.account_evicted(&entry);
                        evicted = true;
                    }
                    Some((key, entry)) => keep.push((key, entry)),
                    None => break,
                }
            }
            for (key, entry) in keep {
                self.cache.put(key, entry);
            }
            if !evicted {
                break;
            }
        }
    }

    fn adjust_gpu_cache_budget_after_render(&mut self) {
        let failures = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.stats().vram_reserve_failures)
            .unwrap_or(0);
        if failures > self.gpu_vram_failures_seen {
            self.gpu_vram_failures_seen = failures;
            self.shrink_gpu_cache_budget();
            return;
        }

        let limit = configured_vram_bytes();
        let used = vram_used_bytes();
        if self.gpu_cache_cap_bytes >= self.gpu_cache_max_bytes
            || used.saturating_mul(4) >= limit.saturating_mul(3)
        {
            self.gpu_cache_spare_successes = 0;
            return;
        }

        self.gpu_cache_spare_successes = self.gpu_cache_spare_successes.saturating_add(1);
        if self.gpu_cache_spare_successes >= GPU_CACHE_GROW_SUCCESS_STREAK {
            let step = (limit / GPU_CACHE_GROW_FRACTION_DIVISOR).max(MIB);
            self.gpu_cache_cap_bytes = self
                .gpu_cache_cap_bytes
                .saturating_add(step)
                .min(self.gpu_cache_max_bytes);
            self.gpu_cache_spare_successes = 0;
        }
    }

    fn shrink_gpu_cache_budget(&mut self) {
        self.gpu_cache_spare_successes = 0;
        let old = self.gpu_cache_cap_bytes;
        self.gpu_cache_cap_bytes =
            old.saturating_mul(GPU_CACHE_SHRINK_NUMERATOR) / GPU_CACHE_SHRINK_DENOMINATOR;
        if old > 0 && self.gpu_cache_cap_bytes == old {
            self.gpu_cache_cap_bytes = old - 1;
        }
        self.evict_gpu_cache_to_fit(0);
    }
}

fn gpu_cache_limits() -> (usize, usize) {
    let limit = configured_vram_bytes();
    let max = limit / GPU_CACHE_MAX_FRACTION_DIVISOR;
    let cap = (limit / GPU_CACHE_INITIAL_FRACTION_DIVISOR).min(max);
    (cap, max)
}

impl Default for CachingRenderContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderContext for CachingRenderContext {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn gpu_preference(&self) -> GpuPreference {
        self.gpu_preference
    }

    fn gpu_backend(&mut self) -> Option<&mut dyn GpuRasterBackend> {
        self.gpu_backend_mut()
            .map(|gpu| gpu as &mut dyn GpuRasterBackend)
    }

    fn motion_blur_enabled(&self) -> bool {
        self.motion_blur_enabled
    }

    fn render(
        &mut self,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) -> RasterImage {
        // Capture type identity up front so we can record stats per
        // concrete type even when we hit the cache and never touch
        // `component` again.
        let any_ref = component.as_any();
        let type_id = any_ref.type_id();
        let type_name = DynEq::type_name(component);

        // Timing setup. `child_acc_at_start` is the total inclusive
        // time recorded for *all* prior `render` calls; on exit the
        // delta tells us how much time was spent inside nested child
        // renders during *this* call, which we subtract from our own
        // elapsed to get self time.
        let start = Instant::now();
        let child_acc_at_start = self.total_render_time;

        // Transparent components (pure pass-through wrappers like
        // `Positioned`) get no cache slot of their own: they delegate
        // straight to a child through `ctx.render`, so caching them would
        // only duplicate the child's entry and double-count its bytes.
        // Timing is independent of this — the bookkeeping below runs either
        // way — so a transparent wrapper's (near-zero) self-cost stays
        // visible and the child's time is attributed to the child.
        let key = match component.cache_policy() {
            CachePolicy::Transparent => None,
            CachePolicy::Memoize => Some(CacheKey::of(component, type_id, size, target)),
        };

        // `counted` gates the hit/miss tally so transparent passes (which
        // are neither) don't masquerade as cache misses; `was_hit` only
        // matters when `counted`.
        let (img, was_hit, counted) = match key {
            None => (component.render(size, target, self), false, false),
            Some(key) => {
                if let Some(entry) = self.cache.get(&key) {
                    (entry.image.clone(), true, true)
                } else {
                    // Miss path: produce the image, then decide whether to
                    // admit it. Nested `ctx.render` calls happen inside
                    // `component.render`, which is why timing is wrapped
                    // around the whole block.
                    let img = component.render(size, target, self);
                    self.adjust_gpu_cache_budget_after_render();
                    let bytes = Self::image_bytes(&img);
                    let is_gpu = matches!(img, RasterImage::Gpu(_));

                    if !is_gpu && bytes > self.cap_bytes {
                        self.oversize_skips += 1;
                    } else if !self.should_admit(key, type_id, bytes) {
                    } else {
                        if is_gpu {
                            self.evict_gpu_cache_to_fit(bytes);
                        }
                        if is_gpu
                            && self.gpu_cache_bytes.saturating_add(bytes) > self.gpu_cache_cap_bytes
                        {
                            self.budget_skips += 1;
                        } else {
                            if !is_gpu {
                                self.evict_cpu_cache_to_fit(bytes);
                            }
                            if self.under_memory_pressure() {
                                self.shed_under_pressure();
                                self.pressure_skips += 1;
                            } else if let Some(entry) = Self::cache_entry(img.clone(), bytes) {
                                self.cache.put(key, entry);
                                self.cur_bytes += bytes;
                                if is_gpu {
                                    self.gpu_cache_bytes += bytes;
                                } else {
                                    self.cpu_cache_bytes += bytes;
                                }
                            } else {
                                self.budget_skips += 1;
                            }
                        }
                    }
                    (img, false, true)
                }
            }
        };

        let inclusive = start.elapsed();
        let child_inclusive = self.total_render_time.saturating_sub(child_acc_at_start);
        let self_time = inclusive.saturating_sub(child_inclusive);
        self.total_render_time += inclusive;

        let (stats, _) = self
            .per_type
            .entry(type_id)
            .or_insert_with(|| (TypeStats::default(), type_name));
        if counted {
            if was_hit {
                self.hits += 1;
                stats.hits += 1;
            } else {
                self.misses += 1;
                stats.misses += 1;
            }
        }
        stats.inclusive_time += inclusive;
        stats.self_time += self_time;

        img
    }
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;
    use std::sync::Arc;

    use tellur_core::color::Color;
    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::layer::Layer;
    use tellur_core::placement::RasterPlacement;
    use tellur_core::raster::{GpuSurface, PixelFormat, RasterComponent, RasterImage, Resolution};
    use tellur_core::render_context::{CachePolicy, GpuPreference, PassThrough, RenderContext};
    use tellur_core::shapes::Rectangle;
    use tellur_core::vector::Paint;

    use super::{CacheKey, CachedRasterImage, CachingRenderContext, VOLATILE_MIN_MISSES};
    use crate::rasterize::Rasterizable;

    fn scene() -> Layer {
        Layer::builder()
            .size(Vec2(40.0, 30.0))
            .child(
                Rectangle {
                    size: Vec2(20.0, 10.0),
                    fill: Paint::Solid(Color::rgba_u8(200, 40, 60, 255)).into(),
                    stroke: None,
                }
                .rasterize()
                .place_at(Vec2(5.0, 7.0)),
            )
            .build()
    }

    #[test]
    fn positioned_is_transparent_to_cache() {
        let positioned = Rectangle {
            size: Vec2(1.0, 1.0),
            fill: Paint::Solid(Color::rgb_u8(0, 0, 0)).into(),
            stroke: None,
        }
        .rasterize()
        .place_at(Vec2::ZERO);
        assert_eq!(positioned.cache_policy(), CachePolicy::Transparent);
    }

    // A transparent `Positioned` must not change pixels: routing its child
    // through the context (so the child owns the cache slot) has to produce
    // exactly what an uncached pass produces, on both the first (miss) and
    // second (hit) frame.
    #[test]
    fn passthrough_and_cache_agree_through_positioned() {
        let size = Vec2(40.0, 30.0);
        let target = Resolution::new(40, 30);

        let mut pass = PassThrough;
        let a = {
            let img = scene().render(size, target, &mut pass);
            pass.readback(img)
        };

        let mut cache = CachingRenderContext::new().with_gpu_preference(GpuPreference::Disabled);
        let first = {
            let img = scene().render(size, target, &mut cache);
            cache.readback(img)
        };
        let second = {
            let img = scene().render(size, target, &mut cache);
            cache.readback(img)
        };

        assert_eq!(a.pixels, first.pixels);
        assert_eq!(a.pixels, second.pixels);
    }

    #[derive(PartialEq, Hash)]
    struct SolidRaster {
        id: u8,
    }

    impl RasterComponent for SolidRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(1.0, 1.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            let pixels = (target.width as usize) * (target.height as usize);
            let mut buf = Vec::with_capacity(pixels * 4);
            for _ in 0..pixels {
                buf.extend_from_slice(&[self.id, 0, 0, 255]);
            }
            RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, buf)
        }
    }

    #[test]
    fn second_use_admission_skips_one_off_keys() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let one = SolidRaster { id: 1 };
        let two = SolidRaster { id: 2 };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled)
            .with_second_use_admission();

        render_and_readback(&mut cache, &one, size, target);
        render_and_readback(&mut cache, &two, size, target);
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.admission_skips, 2);
        assert_eq!(metrics.bytes_cached, 0);

        render_and_readback(&mut cache, &one, size, target);
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 3);
        assert_eq!(metrics.admission_skips, 2);
        assert!(metrics.bytes_cached > 0);

        render_and_readback(&mut cache, &one, size, target);
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 3);
        assert_eq!(metrics.admission_skips, 2);
    }

    #[test]
    fn volatile_large_admission_skips_repeated_large_misses() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(512, 512);
        let mut cache = CachingRenderContext::with_capacity_bytes(64 * 1024 * 1024)
            .with_gpu_preference(GpuPreference::Disabled)
            .with_volatile_large_admission();

        for id in 0..VOLATILE_MIN_MISSES {
            render_and_readback(&mut cache, &SolidRaster { id: id as u8 }, size, target);
        }
        let warmed = cache.metrics();
        assert_eq!(warmed.hits, 0);
        assert_eq!(warmed.misses, VOLATILE_MIN_MISSES);
        assert_eq!(warmed.admission_skips, 0);
        assert!(warmed.bytes_cached > 0);

        render_and_readback(&mut cache, &SolidRaster { id: 99 }, size, target);
        let skipped = cache.metrics();
        assert_eq!(skipped.hits, 0);
        assert_eq!(skipped.misses, VOLATILE_MIN_MISSES + 1);
        assert_eq!(skipped.admission_skips, 1);
        assert_eq!(skipped.bytes_cached, warmed.bytes_cached);
    }

    #[test]
    fn gpu_cache_cap_eviction_drops_only_gpu_entries() {
        let mut cache = CachingRenderContext::with_capacity_bytes(1024 * 1024)
            .with_gpu_preference(GpuPreference::PreferGpu);
        let cpu = RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![1, 2, 3, 4]);
        let gpu = RasterImage::Gpu(GpuSurface::new(
            10,
            10,
            PixelFormat::Rgba8,
            "test",
            Arc::new(()),
        ));
        let cpu_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 1,
            size_x_bits: 1,
            size_y_bits: 1,
            target_width: 1,
            target_height: 1,
        };
        let gpu_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 2,
            size_x_bits: 1,
            size_y_bits: 1,
            target_width: 10,
            target_height: 10,
        };
        cache.cache.put(
            cpu_key,
            CachedRasterImage {
                image: cpu,
                bytes: 4,
                _ram_reservation: None,
            },
        );
        cache.cache.put(
            gpu_key,
            CachedRasterImage {
                image: gpu,
                bytes: 400,
                _ram_reservation: None,
            },
        );
        cache.cur_bytes = 404;
        cache.gpu_cache_bytes = 400;
        cache.gpu_cache_cap_bytes = 128;

        cache.evict_gpu_cache_to_fit(1);

        assert_eq!(cache.cache.len(), 1);
        assert_eq!(cache.cur_bytes, 4);
        assert_eq!(cache.gpu_cache_bytes, 0);
        assert_eq!(cache.bytes_evicted, 400);
    }

    #[test]
    fn gpu_cache_budget_shrinks_after_vram_pressure() {
        let mut cache = CachingRenderContext::with_capacity_bytes(1024 * 1024)
            .with_gpu_preference(GpuPreference::PreferGpu);
        cache.gpu_cache_cap_bytes = 1024;

        cache.shrink_gpu_cache_budget();

        assert_eq!(cache.gpu_cache_cap_bytes, 768);
    }

    fn render_and_readback(
        cache: &mut CachingRenderContext,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) {
        let image = cache.render(component, size, target);
        let _ = cache.readback(image);
    }
}
