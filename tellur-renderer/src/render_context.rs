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

use crate::cache::{
    AdmissionPreparation, AdmissionRejectReason, CommitWithError, EntryMeta, ImmediateCache,
    Lookup, RemovedEntry,
};
use crate::gpu::{GpuRenderStats, GpuRenderer};

/// Default cache size in bytes (1 GiB) when constructed with
/// [`CachingRenderContext::new`].
pub const DEFAULT_CAPACITY_BYTES: usize = 1024 * 1024 * 1024;

/// System-memory utilization fraction above which the cache stops
/// admitting new entries and starts shedding existing ones.
pub const MEMORY_PRESSURE_THRESHOLD: f32 = 0.90;

const GPU_CACHE_INITIAL_FRACTION_DIVISOR: usize = 4;
const GPU_CACHE_MAX_FRACTION_DIVISOR: usize = 2;
const GPU_CACHE_SHRINK_NUMERATOR: usize = 3;
const GPU_CACHE_SHRINK_DENOMINATOR: usize = 4;
const GPU_CACHE_GROW_SUCCESS_STREAK: u8 = 64;
const GPU_CACHE_GROW_FRACTION_DIVISOR: usize = 64;
const MIB: usize = 1024 * 1024;

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
    _ram_reservation: Option<BudgetReservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderCacheClass {
    Cpu,
    Gpu,
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
    /// Bytes currently held by cached GPU render entries.
    pub gpu_cache_bytes: usize,
    /// Current Tellur-managed VRAM allowance for cached GPU render entries.
    pub gpu_cache_cap_bytes: usize,
    /// Maximum Tellur-managed VRAM allowance the GPU render cache can grow back to.
    pub gpu_cache_max_bytes: usize,
    /// Process-wide Tellur-managed VRAM currently reserved.
    pub vram_used_bytes: usize,
    /// Configured process-wide Tellur-managed VRAM budget.
    pub vram_budget_bytes: usize,
    /// Bytes released through LRU eviction (capacity-driven).
    pub bytes_evicted: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because system memory pressure was over threshold.
    pub pressure_skips: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because a single image exceeded the configured cap.
    pub oversize_skips: u64,
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
            "Cache  {} hits / {} misses ({:.1}% hit) — {} cached, {} evicted, {} pressure skips, {} oversize skips, {} budget skips",
            self.hits,
            self.misses,
            self.hit_rate() * 100.0,
            format_bytes(self.bytes_cached as u64),
            format_bytes(self.bytes_evicted),
            self.pressure_skips,
            self.oversize_skips,
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
        writeln!(
            f,
            "VRAM   used {} / {}, render_cache {} / {} (max {}), upload_cache {} / {} ({} entries, max {})",
            format_bytes(self.vram_used_bytes as u64),
            format_bytes(self.vram_budget_bytes as u64),
            format_bytes(self.gpu_cache_bytes as u64),
            format_bytes(self.gpu_cache_cap_bytes as u64),
            format_bytes(self.gpu_cache_max_bytes as u64),
            format_bytes(self.gpu.upload_cache_bytes as u64),
            format_bytes(self.gpu.upload_cache_cap_bytes as u64),
            self.gpu.upload_cache_entries,
            format_bytes(self.gpu.upload_cache_max_bytes as u64),
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
    cache: ImmediateCache<CacheKey, RenderCacheClass, CachedRasterImage>,
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
    budget_skips: u64,
    per_type: HashMap<TypeId, (TypeStats, &'static str)>,
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
        let mut cache = ImmediateCache::new();
        drop(cache.set_capacity(RenderCacheClass::Cpu, cache_ram_capacity(cap_bytes)));
        drop(cache.set_capacity(RenderCacheClass::Gpu, gpu_cache_cap_bytes));
        Self {
            cache,
            gpu_cache_max_bytes,
            gpu_cache_spare_successes: 0,
            gpu_vram_failures_seen: 0,
            system: System::new(),
            hits: 0,
            misses: 0,
            bytes_evicted: 0,
            pressure_skips: 0,
            oversize_skips: 0,
            budget_skips: 0,
            per_type: HashMap::new(),
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
        self.cache.total_weight()
    }

    /// Configured maximum capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.cache.class_capacity(RenderCacheClass::Cpu)
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
            bytes_cached: self.cache.total_weight(),
            gpu_cache_bytes: self.cache.class_weight(RenderCacheClass::Gpu),
            gpu_cache_cap_bytes: self.cache.class_capacity(RenderCacheClass::Gpu),
            gpu_cache_max_bytes: self.gpu_cache_max_bytes,
            vram_used_bytes: vram_used_bytes(),
            vram_budget_bytes: configured_vram_bytes(),
            bytes_evicted: self.bytes_evicted,
            pressure_skips: self.pressure_skips,
            oversize_skips: self.oversize_skips,
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
        self.budget_skips = 0;
        self.per_type.clear();
        self.total_render_time = Duration::ZERO;
    }

    /// Drop all cached entries.
    pub fn clear(&mut self) {
        self.cache.clear();
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
            _ram_reservation: ram_reservation,
        })
    }

    fn account_evicted(
        &mut self,
        entries: impl IntoIterator<Item = RemovedEntry<CacheKey, RenderCacheClass, CachedRasterImage>>,
    ) {
        for entry in entries {
            let RemovedEntry { key, meta, value } = entry;
            self.bytes_evicted = self.bytes_evicted.saturating_add(meta.weight as u64);
            let _ = key;
            drop(value);
        }
    }

    fn account_one_evicted(
        &mut self,
        entry: RemovedEntry<CacheKey, RenderCacheClass, CachedRasterImage>,
    ) {
        self.account_evicted(std::iter::once(entry));
    }

    /// Evict entries until system memory pressure subsides or the cache
    /// is empty.
    fn shed_under_pressure(&mut self) {
        while self.under_memory_pressure() {
            let Some(entry) = self.cache.evict_one(RenderCacheClass::Cpu) else {
                break;
            };
            self.account_one_evicted(entry);
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
        let gpu_cache_cap_bytes = self.cache.class_capacity(RenderCacheClass::Gpu);
        if gpu_cache_cap_bytes >= self.gpu_cache_max_bytes
            || used.saturating_mul(4) >= limit.saturating_mul(3)
        {
            self.gpu_cache_spare_successes = 0;
            return;
        }

        self.gpu_cache_spare_successes = self.gpu_cache_spare_successes.saturating_add(1);
        if self.gpu_cache_spare_successes >= GPU_CACHE_GROW_SUCCESS_STREAK {
            let step = (limit / GPU_CACHE_GROW_FRACTION_DIVISOR).max(MIB);
            let next = gpu_cache_cap_bytes
                .saturating_add(step)
                .min(self.gpu_cache_max_bytes);
            let removed = self.cache.set_capacity(RenderCacheClass::Gpu, next);
            debug_assert!(removed.is_empty(), "growing a cache cannot evict entries");
            self.account_evicted(removed);
            self.gpu_cache_spare_successes = 0;
        }
    }

    fn shrink_gpu_cache_budget(&mut self) {
        self.shrink_gpu_cache_budget_for_pressure(0);
    }

    fn shrink_gpu_cache_budget_for_pressure(&mut self, needed: usize) {
        self.gpu_cache_spare_successes = 0;
        let old = self.cache.class_capacity(RenderCacheClass::Gpu);
        let mut next =
            old.saturating_mul(GPU_CACHE_SHRINK_NUMERATOR) / GPU_CACHE_SHRINK_DENOMINATOR;
        if old > 0 && next == old {
            next = old - 1;
        }
        let non_cache_vram =
            vram_used_bytes().saturating_sub(self.cache.class_weight(RenderCacheClass::Gpu));
        let pressure_cap =
            configured_vram_bytes().saturating_sub(non_cache_vram.saturating_add(needed));
        let removed = self
            .cache
            .set_capacity(RenderCacheClass::Gpu, next.min(pressure_cap));
        self.account_evicted(removed);
    }

    fn evict_gpu_cache_until_vram_available(&mut self, needed: usize) {
        self.shrink_gpu_cache_budget_for_pressure(needed);
        if vram_used_bytes().saturating_add(needed) <= configured_vram_bytes() {
            return;
        }

        while vram_used_bytes().saturating_add(needed) > configured_vram_bytes() {
            let Some(entry) = self.cache.evict_one(RenderCacheClass::Gpu) else {
                break;
            };
            self.account_one_evicted(entry);
        }
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

    fn readback(&mut self, image: RasterImage) -> tellur_core::raster::CpuRasterImage {
        match image {
            RasterImage::Cpu(image) => image,
            image @ RasterImage::Gpu(_) => {
                let readback_bytes = Self::image_bytes(&image);
                let backend = match &image {
                    RasterImage::Gpu(surface) => surface.backend(),
                    RasterImage::Cpu(_) => unreachable!(),
                };
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(image) = gpu.readback(image.clone()) {
                        return image;
                    }
                }

                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.release_cached_resources();
                }
                self.evict_gpu_cache_until_vram_available(readback_bytes);
                if let Some(gpu) = self.gpu.as_mut() {
                    if let Some(image) = gpu.readback(image) {
                        return image;
                    }
                }

                panic!(
                    "render context could not read back GPU image for backend '{backend}' after freeing {readback_bytes} bytes for readback",
                )
            }
        }
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
            Some(key) => match self.cache.lookup(key, |entry| entry.image.clone()) {
                Lookup::Hit(img) => (img, true, true),
                Lookup::Miss(ticket) => {
                    // Miss path: produce the image, then decide whether to
                    // admit it. Nested `ctx.render` calls happen inside
                    // `component.render`, which is why timing is wrapped
                    // around the whole block.
                    let img = component.render(size, target, self);
                    self.adjust_gpu_cache_budget_after_render();
                    let bytes = Self::image_bytes(&img);
                    let class = match &img {
                        RasterImage::Cpu(_) => RenderCacheClass::Cpu,
                        RasterImage::Gpu(_) => RenderCacheClass::Gpu,
                    };

                    match self
                        .cache
                        .prepare_admission(ticket, EntryMeta::new(class, bytes))
                    {
                        AdmissionPreparation::Rejected(AdmissionRejectReason::Overweight {
                            ..
                        }) => match class {
                            RenderCacheClass::Cpu => self.oversize_skips += 1,
                            RenderCacheClass::Gpu => self.budget_skips += 1,
                        },
                        // A nested render may have populated the same key after
                        // this call received its owned miss ticket. Keep that
                        // resident. Future strategy-specific rejections also
                        // leave the freshly-rendered value uncached here.
                        AdmissionPreparation::Rejected(_) => {}
                        AdmissionPreparation::Ready { admission, evicted } => {
                            self.account_evicted(evicted);

                            if self.under_memory_pressure() {
                                self.shed_under_pressure();
                                self.pressure_skips += 1;
                            } else {
                                match self.cache.commit_with(admission, || {
                                    Self::cache_entry(img.clone(), bytes).ok_or(())
                                }) {
                                    Ok(()) => {}
                                    Err(CommitWithError::Create(())) => self.budget_skips += 1,
                                    Err(CommitWithError::Policy(reason)) => {
                                        debug_assert!(
                                            false,
                                            "admission plan became invalid before commit: {reason:?}"
                                        );
                                        self.budget_skips += 1;
                                    }
                                }
                            }
                        }
                    }
                    (img, false, true)
                }
            },
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

    use super::{CacheKey, CachedRasterImage, CachingRenderContext, RenderCacheClass};
    use crate::cache::{AdmissionPreparation, EntryMeta, Lookup};
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

    #[derive(PartialEq, Hash)]
    struct GpuRaster {
        id: u8,
    }

    impl RasterComponent for GpuRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(1.0, 1.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::Gpu(GpuSurface::new(
                target.width,
                target.height,
                PixelFormat::Rgba8,
                "test",
                Arc::new(()),
            ))
        }
    }

    #[test]
    fn immediate_admission_caches_first_miss() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let one = SolidRaster { id: 1 };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        render_and_readback(&mut cache, &one, size, target);
        render_and_readback(&mut cache, &one, size, target);

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 1);
        assert_eq!(metrics.bytes_cached, 4);
    }

    #[test]
    fn cpu_capacity_eviction_updates_cache_metrics() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(cache.cache.set_capacity(RenderCacheClass::Cpu, 4));

        render_and_readback(&mut cache, &SolidRaster { id: 1 }, size, target);
        render_and_readback(&mut cache, &SolidRaster { id: 2 }, size, target);

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.bytes_cached, 4);
        assert_eq!(metrics.bytes_evicted, 4);
    }

    #[test]
    fn clear_drops_entries_without_counting_an_eviction() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(cache.cache.set_capacity(RenderCacheClass::Cpu, 4));
        render_and_readback(&mut cache, &SolidRaster { id: 1 }, size, target);

        cache.clear();

        assert_eq!(cache.current_bytes(), 0);
        assert_eq!(cache.capacity_bytes(), 4);
        assert_eq!(cache.metrics().bytes_evicted, 0);
    }

    #[test]
    fn overweight_images_map_to_cpu_and_gpu_skip_metrics() {
        let size = Vec2(1.0, 1.0);

        let mut cpu_cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(cpu_cache.cache.set_capacity(RenderCacheClass::Cpu, 4));
        let _ = cpu_cache.render(&SolidRaster { id: 1 }, size, Resolution::new(2, 2));
        let cpu_metrics = cpu_cache.metrics();
        assert_eq!(cpu_metrics.oversize_skips, 1);
        assert_eq!(cpu_metrics.budget_skips, 0);
        assert_eq!(cpu_metrics.bytes_cached, 0);

        let mut gpu_cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(gpu_cache.cache.set_capacity(RenderCacheClass::Gpu, 4));
        let _ = gpu_cache.render(&GpuRaster { id: 1 }, size, Resolution::new(1, 1));
        let _ = gpu_cache.render(&GpuRaster { id: 2 }, size, Resolution::new(2, 2));
        let gpu_metrics = gpu_cache.metrics();
        assert_eq!(gpu_metrics.oversize_skips, 0);
        assert_eq!(gpu_metrics.budget_skips, 1);
        assert_eq!(gpu_metrics.gpu_cache_bytes, 4);
        assert_eq!(gpu_metrics.bytes_evicted, 0);
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
        drop(cache.cache.set_capacity(RenderCacheClass::Cpu, 4));
        drop(cache.cache.set_capacity(RenderCacheClass::Gpu, 400));
        insert_test_entry(&mut cache, cpu_key, cpu, RenderCacheClass::Cpu, 4);
        insert_test_entry(&mut cache, gpu_key, gpu, RenderCacheClass::Gpu, 400);

        let removed = cache.cache.reclaim_to_fit(RenderCacheClass::Gpu, 1);
        cache.account_evicted(removed);

        assert_eq!(cache.cache.len(), 1);
        assert_eq!(cache.cache.total_weight(), 4);
        assert_eq!(cache.cache.class_weight(RenderCacheClass::Gpu), 0);
        assert_eq!(cache.bytes_evicted, 400);
    }

    #[test]
    fn gpu_cache_budget_shrinks_after_vram_pressure() {
        let mut cache = CachingRenderContext::with_capacity_bytes(1024 * 1024)
            .with_gpu_preference(GpuPreference::PreferGpu);
        drop(cache.cache.set_capacity(RenderCacheClass::Gpu, 1024));

        cache.shrink_gpu_cache_budget();

        assert_eq!(cache.cache.class_capacity(RenderCacheClass::Gpu), 768);
    }

    #[test]
    fn evict_gpu_cache_until_vram_available_stops_after_needed_bytes() {
        use tellur_core::cache_budget::{configured_vram_bytes, try_reserve_vram};

        struct FakeGpuHandle {
            _reservation: tellur_core::cache_budget::BudgetReservation,
        }

        let limit = configured_vram_bytes();
        if limit < 16 * 1024 {
            return;
        }
        let entry_bytes = limit / 16;
        let needed = entry_bytes + entry_bytes / 2;
        let background_bytes = limit - (entry_bytes * 3) - (entry_bytes / 2);
        let _background =
            try_reserve_vram(background_bytes).expect("background reservation should fit");

        let mut cache = CachingRenderContext::with_capacity_bytes(1024 * 1024)
            .with_gpu_preference(GpuPreference::PreferGpu);
        drop(
            cache
                .cache
                .set_capacity(RenderCacheClass::Gpu, entry_bytes * 3),
        );
        for content_hash in 1..=3 {
            let reservation =
                try_reserve_vram(entry_bytes).expect("cache entry reservation should fit");
            let image = RasterImage::Gpu(GpuSurface::new(
                1,
                1,
                PixelFormat::Rgba8,
                "test",
                Arc::new(FakeGpuHandle {
                    _reservation: reservation,
                }),
            ));
            let key = CacheKey {
                type_id: TypeId::of::<SolidRaster>(),
                content_hash,
                size_x_bits: 1,
                size_y_bits: 1,
                target_width: 1,
                target_height: 1,
            };
            insert_test_entry(&mut cache, key, image, RenderCacheClass::Gpu, entry_bytes);
        }

        cache.evict_gpu_cache_until_vram_available(needed);

        assert_eq!(cache.cache.len(), 2);
        assert_eq!(cache.cache.total_weight(), entry_bytes * 2);
        assert_eq!(
            cache.cache.class_weight(RenderCacheClass::Gpu),
            entry_bytes * 2
        );
        assert_eq!(cache.bytes_evicted, entry_bytes as u64);
    }

    fn insert_test_entry(
        cache: &mut CachingRenderContext,
        key: CacheKey,
        image: RasterImage,
        class: RenderCacheClass,
        bytes: usize,
    ) {
        let ticket = match cache.cache.lookup(key, |_| ()) {
            Lookup::Hit(()) => panic!("test cache key was already resident"),
            Lookup::Miss(ticket) => ticket,
        };
        let (admission, evicted) = match cache
            .cache
            .prepare_admission(ticket, EntryMeta::new(class, bytes))
        {
            AdmissionPreparation::Ready { admission, evicted } => (admission, evicted),
            AdmissionPreparation::Rejected(reason) => {
                panic!("test admission rejected: {reason:?}")
            }
        };
        assert!(evicted.is_empty());
        cache
            .cache
            .commit_with(admission, || {
                Ok::<_, ()>(CachedRasterImage {
                    image,
                    _ram_reservation: None,
                })
            })
            .expect("test cache admission must commit");
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
