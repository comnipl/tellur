//! Reuse- and cost-aware implementation of
//! [`tellur_core::render_context::RenderContext`].
//!
//! [`CachingRenderContext`] memoizes [`RasterImage`] representations keyed by
//! `(TypeId, content_hash, size, target, residency)`. The cache is bounded in
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
use tellur_core::raster::{PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution};
use tellur_core::render_context::{CachePolicy, GpuPreference, GpuRasterBackend, RenderContext};

use crate::cache::{
    AdmissionPlanning, AdmissionPreparation, AdmissionRejectReason, CommitWithError, EntryMeta,
    FrequencyCache, Lookup, MissTicket, RemovedEntry,
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
const GPU_CACHE_GROW_SPARE_RENDER_STREAK: u8 = 64;
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
    residency: RasterResidency,
}

impl CacheKey {
    fn of(
        c: &dyn RasterComponent,
        type_id: TypeId,
        size: Vec2,
        target: Resolution,
        residency: RasterResidency,
    ) -> Self {
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
            residency,
        }
    }

    fn with_residency(self, residency: RasterResidency) -> Self {
        Self { residency, ..self }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepresentationAdmission {
    Resident,
    /// A normal frequency warm-up miss; another request should retry the
    /// desired representation before retaining an intermediate source.
    Deferred,
    /// A budget, pressure, or replacement-policy rejection for which keeping
    /// a freshly-produced opposite representation can avoid repeated renders.
    Rejected,
}

impl From<RasterResidency> for RenderCacheClass {
    fn from(residency: RasterResidency) -> Self {
        match residency {
            RasterResidency::Cpu => Self::Cpu,
            RasterResidency::Gpu => Self::Gpu,
        }
    }
}

/// Per-concrete-type hit/miss tally.
///
/// Surfaced as part of [`CacheMetrics::per_type`] so callers can spot
/// which component types are actually benefiting from memoization and
/// which requested representations keep missing every frame (typically a sign that something
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
    /// Requested representations that were absent from the cache.
    ///
    /// A miss may reuse and convert the opposite CPU/GPU representation
    /// without invoking the component's `render` method again.
    pub misses: u64,
    /// Total bytes currently held by CPU and GPU cache entries.
    pub bytes_cached: usize,
    /// Bytes currently held by cached CPU render entries.
    pub cpu_cache_bytes: usize,
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
    /// Bytes released through policy-selected eviction.
    pub bytes_evicted: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because system memory pressure was over threshold.
    pub pressure_skips: u64,
    /// Misses where the freshly-produced image was not admitted
    /// because a single image exceeded the configured cap.
    pub oversize_skips: u64,
    /// Requested representations not admitted because a cache/VRAM budget was
    /// exhausted or the GPU representation could not be materialized.
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
            "GPU    preference={:?}, attempted={}, available={}, ops={} (composite {}, shadow {}, outline {}, rasterize {}, fill {}, temporal_avg {}, readback {}, vram_failures {})",
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
        )?;
        writeln!(
            f,
            "VRAM   used {} / {}, render_cache {} / {} (max {})",
            format_bytes(self.vram_used_bytes as u64),
            format_bytes(self.vram_budget_bytes as u64),
            format_bytes(self.gpu_cache_bytes as u64),
            format_bytes(self.gpu_cache_cap_bytes as u64),
            format_bytes(self.gpu_cache_max_bytes as u64),
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

fn render_cost_nanos(duration: Duration) -> u64 {
    duration.as_nanos().clamp(1, u64::MAX as u128) as u64
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
/// [`tellur_core::timeline_component::ResolvedTimeline::frame`]; the cache
/// persists across frames, admitting a subtree after its second observation and
/// retaining the entries expected to save the most render time per byte.
pub struct CachingRenderContext {
    cache: FrequencyCache<CacheKey, RenderCacheClass, CachedRasterImage>,
    gpu_cache_max_bytes: usize,
    gpu_cache_spare_renders: u8,
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
        let mut cache = FrequencyCache::new();
        drop(cache.set_capacity(RenderCacheClass::Cpu, cache_ram_capacity(cap_bytes)));
        drop(cache.set_capacity(RenderCacheClass::Gpu, gpu_cache_cap_bytes));
        Self {
            cache,
            gpu_cache_max_bytes,
            gpu_cache_spare_renders: 0,
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
        if !self.gpu_preference.prefers_gpu() {
            return None;
        }
        // GPU primitives used by timeline compositing and temporal effects can
        // run outside `ctx.render`. Reconcile their allocation failures before
        // handing the backend out again so a cache-hit-only workload still
        // releases scratch and shrinks the frequency-managed GPU cache.
        self.reconcile_gpu_vram_failure();
        if self.gpu.is_some() {
            return self.gpu.as_mut();
        }
        if self.gpu_init_attempted {
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
            cpu_cache_bytes: self.cache.class_weight(RenderCacheClass::Cpu),
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

    fn admit_representation(
        &mut self,
        ticket: MissTicket<CacheKey>,
        image: &RasterImage,
        residency: RasterResidency,
        observed_cost: u64,
    ) -> RepresentationAdmission {
        debug_assert_eq!(image.residency(), residency);
        let bytes = Self::image_bytes(image);
        let class = RenderCacheClass::from(residency);

        match self
            .cache
            .plan_admission(ticket, EntryMeta::with_cost(class, bytes, observed_cost))
        {
            AdmissionPlanning::Rejected(AdmissionRejectReason::AlreadyResident) => {
                RepresentationAdmission::Resident
            }
            AdmissionPlanning::Rejected(AdmissionRejectReason::BelowFrequency { .. }) => {
                RepresentationAdmission::Deferred
            }
            AdmissionPlanning::Rejected(AdmissionRejectReason::Overweight { .. }) => {
                match class {
                    RenderCacheClass::Cpu => self.oversize_skips += 1,
                    RenderCacheClass::Gpu => self.budget_skips += 1,
                }
                RepresentationAdmission::Rejected
            }
            // Strategy-specific rejections leave the freshly-rendered value
            // uncached here.
            AdmissionPlanning::Rejected(_) => RepresentationAdmission::Rejected,
            AdmissionPlanning::Planned(planned) => {
                // Host memory pressure applies only to CPU pixel buffers. GPU
                // entries own VRAM reservations and are bounded independently.
                if class == RenderCacheClass::Cpu && self.under_memory_pressure() {
                    self.shed_under_pressure();
                    self.pressure_skips += 1;
                    return RepresentationAdmission::Rejected;
                }

                match self.cache.prepare_admission(planned) {
                    AdmissionPreparation::Rejected(reason) => {
                        debug_assert!(
                            false,
                            "planned admission became invalid before preparation: {reason:?}"
                        );
                        RepresentationAdmission::Rejected
                    }
                    AdmissionPreparation::Ready { admission, evicted } => {
                        self.account_evicted(evicted);
                        match self.cache.commit_with(admission, || {
                            Self::cache_entry(image.clone(), bytes).ok_or(())
                        }) {
                            Ok(()) => RepresentationAdmission::Resident,
                            Err(CommitWithError::Create(())) => {
                                self.budget_skips += 1;
                                RepresentationAdmission::Rejected
                            }
                            Err(CommitWithError::Policy(reason)) => {
                                debug_assert!(
                                    false,
                                    "admission plan became invalid before commit: {reason:?}"
                                );
                                self.budget_skips += 1;
                                RepresentationAdmission::Rejected
                            }
                        }
                    }
                }
            }
        }
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
        if self.reconcile_gpu_vram_failure() {
            return;
        }

        // CPU-only work says nothing about whether the GPU has recovered from
        // pressure. Only start the cooldown while a backend is active and GPU
        // work remains enabled.
        if !self.gpu_preference.prefers_gpu() || self.gpu.is_none() {
            self.gpu_cache_spare_renders = 0;
            return;
        }

        let limit = configured_vram_bytes();
        let used = vram_used_bytes();
        let gpu_cache_cap_bytes = self.cache.class_capacity(RenderCacheClass::Gpu);
        if gpu_cache_cap_bytes >= self.gpu_cache_max_bytes
            || used.saturating_mul(4) >= limit.saturating_mul(3)
        {
            self.gpu_cache_spare_renders = 0;
            return;
        }

        self.gpu_cache_spare_renders = self.gpu_cache_spare_renders.saturating_add(1);
        if self.gpu_cache_spare_renders >= GPU_CACHE_GROW_SPARE_RENDER_STREAK {
            let step = (limit / GPU_CACHE_GROW_FRACTION_DIVISOR).max(MIB);
            let next = gpu_cache_cap_bytes
                .saturating_add(step)
                .min(self.gpu_cache_max_bytes);
            let removed = self.cache.set_capacity(RenderCacheClass::Gpu, next);
            debug_assert!(removed.is_empty(), "growing a cache cannot evict entries");
            self.account_evicted(removed);
            self.gpu_cache_spare_renders = 0;
        }
    }

    /// Reconciles allocation failures from GPU operations that may have run
    /// outside the component-cache miss path.
    fn reconcile_gpu_vram_failure(&mut self) -> bool {
        let failures = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.stats().vram_reserve_failures)
            .unwrap_or(0);
        if failures <= self.gpu_vram_failures_seen {
            return false;
        }

        self.gpu_vram_failures_seen = failures;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.release_cached_resources();
        }
        self.shrink_gpu_cache_budget();
        true
    }

    fn shrink_gpu_cache_budget(&mut self) {
        self.shrink_gpu_cache_budget_for_pressure(0);
    }

    fn shrink_gpu_cache_budget_for_pressure(&mut self, needed: usize) {
        self.gpu_cache_spare_renders = 0;
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

    fn ensure_residency(&mut self, image: RasterImage, residency: RasterResidency) -> RasterImage {
        match (residency, image) {
            (RasterResidency::Cpu, RasterImage::Cpu(image)) => RasterImage::Cpu(image),
            (RasterResidency::Cpu, image @ RasterImage::Gpu(_)) => {
                RasterImage::Cpu(self.readback(image))
            }
            (RasterResidency::Gpu, image @ RasterImage::Gpu(_)) => image,
            (RasterResidency::Gpu, RasterImage::Cpu(image)) => {
                if image.format != PixelFormat::Rgba8 || self.gpu_backend_mut().is_none() {
                    return RasterImage::Cpu(image);
                }

                if let Some(uploaded) = self.gpu.as_mut().and_then(|gpu| gpu.upload(&image)) {
                    return uploaded;
                }

                // Uploads are now owned by the main component cache. If a
                // transient upload cannot reserve VRAM, free renderer scratch
                // and frequency-selected GPU residents before retrying once.
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.release_cached_resources();
                }
                self.gpu_vram_failures_seen = self
                    .gpu
                    .as_ref()
                    .map(|gpu| gpu.stats().vram_reserve_failures)
                    .unwrap_or(self.gpu_vram_failures_seen);
                self.evict_gpu_cache_until_vram_available(image.pixels.len());

                self.gpu
                    .as_mut()
                    .and_then(|gpu| gpu.upload(&image))
                    .unwrap_or(RasterImage::Cpu(image))
            }
        }
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
                self.gpu_vram_failures_seen = self
                    .gpu
                    .as_ref()
                    .map(|gpu| gpu.stats().vram_reserve_failures)
                    .unwrap_or(self.gpu_vram_failures_seen);
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
        residency: RasterResidency,
    ) -> RasterImage {
        // A GPU request is best-effort. Once initialization has definitively
        // failed (or GPU work is disabled), use the CPU representation key so
        // fallback rendering remains cacheable instead of missing forever.
        let residency = match residency {
            RasterResidency::Gpu if self.gpu_backend_mut().is_none() => RasterResidency::Cpu,
            residency => residency,
        };

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
            CachePolicy::Memoize => Some(CacheKey::of(component, type_id, size, target, residency)),
        };

        // `counted` gates the hit/miss tally so transparent passes (which
        // are neither) don't masquerade as cache misses; `was_hit` only
        // matters when `counted`.
        let (img, was_hit, counted) = match key {
            None => {
                let image = component.render(size, target, residency, self);
                (self.ensure_residency(image, residency), false, false)
            }
            Some(key) => match self.cache.lookup(key, |entry| entry.image.clone()) {
                Lookup::Hit(img) => (img, true, true),
                Lookup::Miss(ticket) => {
                    // Reuse the opposite representation as a conversion source
                    // when it is already resident. Its hit is real policy
                    // evidence, while an absent alternative records no ghost
                    // miss because that representation was not requested.
                    let render_start = Instant::now();
                    let other_residency = match residency {
                        RasterResidency::Cpu => RasterResidency::Gpu,
                        RasterResidency::Gpu => RasterResidency::Cpu,
                    };
                    let other_key = key.with_residency(other_residency);
                    let (native, source_was_resident) = match self
                        .cache
                        .get_if_resident(&other_key, |entry| entry.image.clone())
                    {
                        Some(image) => (image, true),
                        None => (component.render(size, target, residency, self), false),
                    };
                    // A fallback source entry only avoids producing `native`;
                    // it does not avoid the upload/readback that follows. Keep
                    // its admission cost separate from the requested
                    // representation's conversion-inclusive cost.
                    let native_observed_cost = render_cost_nanos(render_start.elapsed());
                    let fresh_opposite_source = (!source_was_resident
                        && native.residency() != residency)
                        .then(|| native.clone());
                    let img = self.ensure_residency(native, residency);
                    self.adjust_gpu_cache_budget_after_render();
                    let observed_cost = render_cost_nanos(render_start.elapsed());

                    // A GPU request may fall back when the backend cannot
                    // materialize it. Never register that CPU fallback under a
                    // GPU key; a later request must remain free to retry GPU.
                    let desired_admission = if img.residency() != residency {
                        if residency == RasterResidency::Gpu {
                            self.budget_skips = self.budget_skips.saturating_add(1);
                        }
                        RepresentationAdmission::Rejected
                    } else {
                        self.admit_representation(ticket, &img, residency, observed_cost)
                    };

                    // A fresh opposite-residency source remains useful when
                    // the requested representation either cannot be
                    // materialized or is rejected by budget/replacement policy.
                    // Grow the source under its own key so a later request can
                    // convert it without rerendering the component.
                    if desired_admission == RepresentationAdmission::Rejected {
                        if let Some(fallback) = fresh_opposite_source.as_ref() {
                            let fallback_residency = fallback.residency();
                            let fallback_key = key.with_residency(fallback_residency);
                            if let Lookup::Miss(fallback_ticket) =
                                self.cache.lookup(fallback_key, |_| ())
                            {
                                self.admit_representation(
                                    fallback_ticket,
                                    fallback,
                                    fallback_residency,
                                    native_observed_cost,
                                );
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tellur_core::color::Color;
    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::layer::Layer;
    use tellur_core::placement::RasterPlacement;
    use tellur_core::raster::{
        GpuSurface, Opacity, PixelFormat, RasterComponent, RasterImage, RasterResidency, Resolution,
    };
    use tellur_core::render_context::{CachePolicy, GpuPreference, PassThrough, RenderContext};
    use tellur_core::shapes::Rectangle;
    use tellur_core::vector::Paint;

    use super::{CacheKey, CachedRasterImage, CachingRenderContext, RenderCacheClass};
    use crate::cache::{AdmissionPlanning, AdmissionPreparation, EntryMeta, Lookup};
    use crate::rasterize::Rasterizable;

    #[test]
    fn render_cost_conversion_is_nonzero_and_saturating() {
        assert_eq!(super::render_cost_nanos(Duration::ZERO), 1);
        assert_eq!(super::render_cost_nanos(Duration::from_nanos(7)), 7);
        assert_eq!(
            super::render_cost_nanos(Duration::new(u64::MAX, 999_999_999)),
            u64::MAX
        );
    }

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

    #[test]
    fn full_opacity_is_transparent_to_cache() {
        let component = Opacity {
            opacity: 1.0,
            child: Box::new(SolidRaster { id: 1 }),
        };
        assert_eq!(component.cache_policy(), CachePolicy::Transparent);
        let translucent = Opacity {
            opacity: 0.5,
            child: Box::new(SolidRaster { id: 1 }),
        };
        assert_eq!(translucent.cache_policy(), CachePolicy::Memoize);

        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        let _ = cache.render(&component, size, target, RasterResidency::Cpu);
        let admitted = cache.render(&component, size, target, RasterResidency::Cpu);
        let hit = cache.render(&component, size, target, RasterResidency::Cpu);

        assert!(admitted.shares_storage(&hit));
        assert_eq!(cache.metrics().bytes_cached, 4);
    }

    // A transparent `Positioned` must not change pixels: routing its child
    // through the context (so the child owns the cache slot) has to produce
    // exactly what an uncached pass produces on the first miss, the second
    // render that admits the value, and the first actual hit.
    #[test]
    fn passthrough_and_cache_agree_through_positioned() {
        let size = Vec2(40.0, 30.0);
        let target = Resolution::new(40, 30);

        let mut pass = PassThrough;
        let a = {
            let img = scene().render(size, target, RasterResidency::Cpu, &mut pass);
            pass.readback(img)
        };

        let mut cache = CachingRenderContext::new().with_gpu_preference(GpuPreference::Disabled);
        let first = {
            let img = scene().render(size, target, RasterResidency::Cpu, &mut cache);
            cache.readback(img)
        };
        let second = {
            let img = scene().render(size, target, RasterResidency::Cpu, &mut cache);
            cache.readback(img)
        };
        let third = {
            let img = scene().render(size, target, RasterResidency::Cpu, &mut cache);
            cache.readback(img)
        };

        assert_eq!(a.pixels, first.pixels);
        assert_eq!(a.pixels, second.pixels);
        assert_eq!(a.pixels, third.pixels);
    }

    #[derive(Clone, PartialEq, Hash)]
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
            _residency: RasterResidency,
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

    #[tellur_core::component(raster)]
    fn AvailablePositionedRaster(#[available] _available: Vec2) -> impl RasterComponent {
        SolidRaster { id: 9 }.place_at(Vec2::ZERO)
    }

    #[test]
    fn available_raster_component_only_caches_the_rendered_child_surface() {
        let component = AvailablePositionedRaster::builder().build();
        assert_eq!(component.cache_policy(), CachePolicy::Transparent);

        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        let first = cache.render(&component, size, target, RasterResidency::Cpu);
        let admitted = cache.render(&component, size, target, RasterResidency::Cpu);
        let hit = cache.render(&component, size, target, RasterResidency::Cpu);

        assert!(!first.shares_storage(&admitted));
        assert!(admitted.shares_storage(&hit));
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.cpu_cache_bytes, 4);
        assert_eq!(metrics.bytes_cached, 4);
    }

    #[derive(Clone)]
    struct CountingRaster {
        id: u8,
        renders: Arc<AtomicUsize>,
    }

    impl PartialEq for CountingRaster {
        fn eq(&self, other: &Self) -> bool {
            // The counter is test instrumentation, not component content; its
            // mutation must not change the cache key between renders.
            self.id == other.id
        }
    }

    impl std::hash::Hash for CountingRaster {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.id.hash(state);
        }
    }

    impl RasterComponent for CountingRaster {
        fn layout(&self, constraints: Constraints) -> Vec2 {
            constraints.constrain(Vec2(1.0, 1.0))
        }

        fn render(
            &self,
            _size: Vec2,
            target: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            self.renders.fetch_add(1, Ordering::Relaxed);
            let pixels = (target.width as usize) * (target.height as usize);
            let mut buf = Vec::with_capacity(pixels * 4);
            for _ in 0..pixels {
                buf.extend_from_slice(&[self.id, 0, 0, 255]);
            }
            RasterImage::cpu(target.width, target.height, PixelFormat::Rgba8, buf)
        }
    }

    #[test]
    fn frequency_admission_caches_second_miss() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let one = SolidRaster { id: 1 };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        render_and_readback(&mut cache, &one, size, target);
        let first = cache.metrics();
        assert_eq!(first.hits, 0);
        assert_eq!(first.misses, 1);
        assert_eq!(first.bytes_cached, 0);

        render_and_readback(&mut cache, &one, size, target);
        let admitted = cache.metrics();
        assert_eq!(admitted.hits, 0);
        assert_eq!(admitted.misses, 2);
        assert_eq!(admitted.bytes_cached, 4);

        render_and_readback(&mut cache, &one, size, target);
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.bytes_cached, 4);
    }

    #[test]
    fn disabled_gpu_request_uses_the_cpu_cache_key() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let component = SolidRaster { id: 1 };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        for _ in 0..3 {
            let image = cache.render(&component, size, target, RasterResidency::Gpu);
            assert_eq!(image.residency(), RasterResidency::Cpu);
        }

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.cpu_cache_bytes, 4);
        assert_eq!(metrics.gpu_cache_bytes, 0);
        assert!(!metrics.gpu_init_attempted);
    }

    #[test]
    fn cpu_only_misses_do_not_regrow_the_gpu_cache_budget() {
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        let initial_cap = cache.cache.class_capacity(RenderCacheClass::Gpu);

        for id in 0..super::GPU_CACHE_GROW_SPARE_RENDER_STREAK {
            let _ = cache.render(
                &SolidRaster { id },
                Vec2(1.0, 1.0),
                Resolution::new(1, 1),
                RasterResidency::Cpu,
            );
        }

        assert_eq!(cache.gpu_cache_spare_renders, 0);
        assert_eq!(
            cache.cache.class_capacity(RenderCacheClass::Gpu),
            initial_cap
        );
    }

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn uploaded_gpu_representation_is_frequency_cached() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let component = SolidRaster { id: 1 };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::PreferGpu);

        let first = cache.render(&component, size, target, RasterResidency::Gpu);
        let second = cache.render(&component, size, target, RasterResidency::Gpu);
        let third = cache.render(&component, size, target, RasterResidency::Gpu);

        assert_eq!(first.residency(), RasterResidency::Gpu);
        assert_eq!(second.residency(), RasterResidency::Gpu);
        assert_eq!(third.residency(), RasterResidency::Gpu);
        assert!(
            !first.shares_storage(&second),
            "the first observation should not bypass frequency admission"
        );
        assert!(
            second.shares_storage(&third),
            "the third request should reuse the Main Component Cache GPU entry"
        );

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 2);
        assert_eq!(metrics.cpu_cache_bytes, 0);
        assert_eq!(metrics.gpu_cache_bytes, 4);
    }

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn rejected_gpu_admission_frequency_caches_the_fresh_cpu_source() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let renders = Arc::new(AtomicUsize::new(0));
        let component = CountingRaster {
            id: 1,
            renders: Arc::clone(&renders),
        };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::PreferGpu);
        drop(cache.cache.set_capacity(RenderCacheClass::Gpu, 0));

        let first = cache.render(&component, size, target, RasterResidency::Gpu);
        assert_eq!(first.residency(), RasterResidency::Gpu);
        assert_eq!(renders.load(Ordering::Relaxed), 1);
        assert_eq!(cache.metrics().cpu_cache_bytes, 0);

        let second = cache.render(&component, size, target, RasterResidency::Gpu);
        assert_eq!(second.residency(), RasterResidency::Gpu);
        assert_eq!(renders.load(Ordering::Relaxed), 2);
        assert_eq!(cache.metrics().cpu_cache_bytes, 4);

        let third = cache.render(&component, size, target, RasterResidency::Gpu);
        assert_eq!(third.residency(), RasterResidency::Gpu);
        assert_eq!(
            renders.load(Ordering::Relaxed),
            2,
            "the third request should upload the resident CPU source without rerendering"
        );

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 3);
        assert_eq!(metrics.cpu_cache_bytes, 4);
        assert_eq!(metrics.gpu_cache_bytes, 0);
    }

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn cpu_resident_is_uploaded_into_a_frequency_cached_gpu_variant_without_rerender() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let renders = Arc::new(AtomicUsize::new(0));
        let component = CountingRaster {
            id: 1,
            renders: Arc::clone(&renders),
        };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::PreferGpu);

        let _ = cache.render(&component, size, target, RasterResidency::Cpu);
        let _ = cache.render(&component, size, target, RasterResidency::Cpu);
        assert_eq!(renders.load(Ordering::Relaxed), 2);
        assert_eq!(cache.metrics().cpu_cache_bytes, 4);

        let first_gpu = cache.render(&component, size, target, RasterResidency::Gpu);
        let second_gpu = cache.render(&component, size, target, RasterResidency::Gpu);
        let third_gpu = cache.render(&component, size, target, RasterResidency::Gpu);

        assert_eq!(renders.load(Ordering::Relaxed), 2);
        assert_eq!(first_gpu.residency(), RasterResidency::Gpu);
        assert_eq!(second_gpu.residency(), RasterResidency::Gpu);
        assert_eq!(third_gpu.residency(), RasterResidency::Gpu);
        assert!(!first_gpu.shares_storage(&second_gpu));
        assert!(second_gpu.shares_storage(&third_gpu));

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 4);
        assert_eq!(metrics.cpu_cache_bytes, 4);
        assert_eq!(metrics.gpu_cache_bytes, 4);
    }

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn gpu_resident_is_read_back_into_a_frequency_cached_cpu_variant_without_rerender() {
        let size = Vec2(1.0, 1.0);
        let target = Resolution::new(1, 1);
        let renders = Arc::new(AtomicUsize::new(0));
        let component = CountingRaster {
            id: 1,
            renders: Arc::clone(&renders),
        };
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::PreferGpu);

        let _ = cache.render(&component, size, target, RasterResidency::Gpu);
        let _ = cache.render(&component, size, target, RasterResidency::Gpu);
        assert_eq!(renders.load(Ordering::Relaxed), 2);
        assert_eq!(cache.metrics().gpu_cache_bytes, 4);

        let first_cpu = cache.render(&component, size, target, RasterResidency::Cpu);
        let second_cpu = cache.render(&component, size, target, RasterResidency::Cpu);
        let third_cpu = cache.render(&component, size, target, RasterResidency::Cpu);

        assert_eq!(renders.load(Ordering::Relaxed), 2);
        assert_eq!(first_cpu.residency(), RasterResidency::Cpu);
        assert_eq!(second_cpu.residency(), RasterResidency::Cpu);
        assert_eq!(third_cpu.residency(), RasterResidency::Cpu);
        assert!(!first_cpu.shares_storage(&second_cpu));
        assert!(second_cpu.shares_storage(&third_cpu));

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 1);
        assert_eq!(metrics.misses, 4);
        assert_eq!(metrics.cpu_cache_bytes, 4);
        assert_eq!(metrics.gpu_cache_bytes, 4);
        assert_eq!(metrics.gpu.readbacks, 2);
    }

    #[test]
    fn residency_variants_have_independent_frequency_history() {
        let cpu_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 1,
            size_x_bits: 1.0_f32.to_bits(),
            size_y_bits: 1.0_f32.to_bits(),
            target_width: 1,
            target_height: 1,
            residency: RasterResidency::Cpu,
        };
        let gpu_key = cpu_key.with_residency(RasterResidency::Gpu);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        let cpu_ticket = match cache.cache.lookup(cpu_key, |_| ()) {
            Lookup::Hit(()) => panic!("CPU variant should start absent"),
            Lookup::Miss(ticket) => ticket,
        };
        assert!(matches!(
            cache.cache.plan_admission(
                cpu_ticket,
                EntryMeta::with_cost(RenderCacheClass::Cpu, 4, 10),
            ),
            AdmissionPlanning::Rejected(_)
        ));

        // Looking for an optional conversion source must not manufacture a
        // request for the other representation.
        assert!(cache.cache.get_if_resident(&gpu_key, |_| ()).is_none());

        let cpu_ticket = match cache.cache.lookup(cpu_key, |_| ()) {
            Lookup::Hit(()) => panic!("CPU variant should still be a ghost"),
            Lookup::Miss(ticket) => ticket,
        };
        assert!(matches!(
            cache.cache.plan_admission(
                cpu_ticket,
                EntryMeta::with_cost(RenderCacheClass::Cpu, 4, 10),
            ),
            AdmissionPlanning::Planned(_)
        ));

        let gpu_ticket = match cache.cache.lookup(gpu_key, |_| ()) {
            Lookup::Hit(()) => panic!("GPU variant should start absent"),
            Lookup::Miss(ticket) => ticket,
        };
        assert!(matches!(
            cache.cache.plan_admission(
                gpu_ticket,
                EntryMeta::with_cost(RenderCacheClass::Gpu, 4, 10),
            ),
            AdmissionPlanning::Rejected(_)
        ));
    }

    #[test]
    fn failed_gpu_materialization_can_grow_a_cpu_fallback_entry() {
        let cpu_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 7,
            size_x_bits: 1.0_f32.to_bits(),
            size_y_bits: 1.0_f32.to_bits(),
            target_width: 1,
            target_height: 1,
            residency: RasterResidency::Cpu,
        };
        let image = RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![7, 0, 0, 255]);
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);

        for _ in 0..2 {
            let ticket = match cache.cache.lookup(cpu_key, |_| ()) {
                Lookup::Hit(()) => panic!("CPU fallback admitted before its second observation"),
                Lookup::Miss(ticket) => ticket,
            };
            cache.admit_representation(ticket, &image, RasterResidency::Cpu, 10);
        }

        let resident = cache
            .cache
            .get_if_resident(&cpu_key, |entry| entry.image.clone())
            .expect("CPU fallback should become resident frequency-aware");
        assert_eq!(resident.residency(), RasterResidency::Cpu);
        assert_eq!(cache.metrics().cpu_cache_bytes, 4);
    }

    #[test]
    fn cpu_admission_eviction_updates_cache_metrics() {
        let mut cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(cache.cache.set_capacity(RenderCacheClass::Cpu, 4));

        let first_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 1,
            size_x_bits: 1,
            size_y_bits: 1,
            target_width: 1,
            target_height: 1,
            residency: RasterResidency::Cpu,
        };
        let second_key = CacheKey {
            content_hash: 2,
            ..first_key
        };
        let first = RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![1, 0, 0, 255]);
        let second = RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![2, 0, 0, 255]);
        insert_test_entry(&mut cache, first_key, first, RenderCacheClass::Cpu, 4, 10);
        insert_test_entry(&mut cache, second_key, second, RenderCacheClass::Cpu, 4, 20);

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 0);
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
        render_and_readback(&mut cache, &SolidRaster { id: 1 }, size, target);

        cache.clear();

        assert_eq!(cache.current_bytes(), 0);
        assert_eq!(cache.capacity_bytes(), 4);
        assert_eq!(cache.metrics().bytes_evicted, 0);
    }

    #[test]
    fn overweight_cpu_images_update_oversize_metric() {
        let size = Vec2(1.0, 1.0);

        let mut cpu_cache = CachingRenderContext::with_capacity_bytes(1024)
            .with_gpu_preference(GpuPreference::Disabled);
        drop(cpu_cache.cache.set_capacity(RenderCacheClass::Cpu, 4));
        let _ = cpu_cache.render(
            &SolidRaster { id: 1 },
            size,
            Resolution::new(2, 2),
            RasterResidency::Cpu,
        );
        let cpu_metrics = cpu_cache.metrics();
        assert_eq!(cpu_metrics.oversize_skips, 1);
        assert_eq!(cpu_metrics.budget_skips, 0);
        assert_eq!(cpu_metrics.bytes_cached, 0);
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
            residency: RasterResidency::Cpu,
        };
        let gpu_key = CacheKey {
            type_id: TypeId::of::<SolidRaster>(),
            content_hash: 2,
            size_x_bits: 1,
            size_y_bits: 1,
            target_width: 10,
            target_height: 10,
            residency: RasterResidency::Gpu,
        };
        drop(cache.cache.set_capacity(RenderCacheClass::Cpu, 4));
        drop(cache.cache.set_capacity(RenderCacheClass::Gpu, 400));
        insert_test_entry(&mut cache, cpu_key, cpu, RenderCacheClass::Cpu, 4, 10);
        insert_test_entry(&mut cache, gpu_key, gpu, RenderCacheClass::Gpu, 400, 10);

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
                residency: RasterResidency::Gpu,
            };
            insert_test_entry(
                &mut cache,
                key,
                image,
                RenderCacheClass::Gpu,
                entry_bytes,
                10,
            );
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
        observed_cost: u64,
    ) {
        let mut image = Some(image);
        for observation in 1..=2 {
            let ticket = match cache.cache.lookup(key, |_| ()) {
                Lookup::Hit(()) => panic!("test cache key was already resident"),
                Lookup::Miss(ticket) => ticket,
            };
            match cache
                .cache
                .plan_admission(ticket, EntryMeta::with_cost(class, bytes, observed_cost))
            {
                AdmissionPlanning::Rejected(_) if observation == 1 => {}
                AdmissionPlanning::Planned(planned) if observation == 2 => {
                    match cache.cache.prepare_admission(planned) {
                        AdmissionPreparation::Ready { admission, evicted } => {
                            cache.account_evicted(evicted);
                            cache
                                .cache
                                .commit_with(admission, || {
                                    Ok::<_, ()>(CachedRasterImage {
                                        image: image
                                            .take()
                                            .expect("test image must be inserted once"),
                                        _ram_reservation: None,
                                    })
                                })
                                .expect("test cache admission must commit");
                            return;
                        }
                        AdmissionPreparation::Rejected(reason) => {
                            panic!("test admission preparation rejected: {reason:?}")
                        }
                    }
                }
                AdmissionPlanning::Rejected(reason) => {
                    panic!("test admission {observation} rejected unexpectedly: {reason:?}")
                }
                AdmissionPlanning::Planned(_) => {
                    panic!("test admission became ready before its second observation")
                }
            }
        }
        panic!("test cache admission did not commit")
    }

    fn render_and_readback(
        cache: &mut CachingRenderContext,
        component: &dyn RasterComponent,
        size: Vec2,
        target: Resolution,
    ) {
        let image = cache.render(component, size, target, RasterResidency::Cpu);
        let _ = cache.readback(image);
    }
}
