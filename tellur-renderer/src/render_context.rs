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
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use lru::LruCache;
use sysinfo::System;
use tellur_core::dyn_compare::DynEq;
use tellur_core::geometry::Vec2;
use tellur_core::raster::{RasterComponent, RasterImage, Resolution};
use tellur_core::render_context::RenderContext;

/// Default cache size in bytes (1 GiB) when constructed with
/// [`CachingRenderContext::new`].
pub const DEFAULT_CAPACITY_BYTES: usize = 1024 * 1024 * 1024;

/// System-memory utilization fraction above which the cache stops
/// admitting new entries and starts shedding existing ones.
pub const MEMORY_PRESSURE_THRESHOLD: f32 = 0.90;

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
    fn of(c: &dyn RasterComponent, size: Vec2, target: Resolution) -> Self {
        // `dyn RasterComponent` implements `Hash` by mixing the concrete
        // `TypeId` with the component's own content hash; reuse that
        // exact hash for the cache key's `content_hash` slot.
        let mut hasher = DefaultHasher::new();
        c.hash(&mut hasher);
        let content_hash = hasher.finish();
        Self {
            type_id: c.as_any().type_id(),
            content_hash,
            size_x_bits: size.0.to_bits(),
            size_y_bits: size.1.to_bits(),
            target_width: target.width,
            target_height: target.height,
        }
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
            "Cache  {} hits / {} misses ({:.1}% hit) — {} cached, {} evicted, {} pressure skips, {} oversize skips",
            self.hits,
            self.misses,
            self.hit_rate() * 100.0,
            format_bytes(self.bytes_cached as u64),
            format_bytes(self.bytes_evicted),
            self.pressure_skips,
            self.oversize_skips,
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

/// A render context that memoizes `RasterImage` outputs.
///
/// Construct one per export / preview session and pass it into
/// [`tellur_core::timeline::Timeline::build`]; the cache persists across
/// frames so any time-invariant subtree only re-renders once.
pub struct CachingRenderContext {
    cache: LruCache<CacheKey, RasterImage>,
    cur_bytes: usize,
    cap_bytes: usize,
    system: System,
    // Aggregate counters; `per_type` is keyed by `TypeId` for cheap
    // updates inside `render`, then projected onto `&'static str` names
    // when the user calls `metrics()`.
    hits: u64,
    misses: u64,
    bytes_evicted: u64,
    pressure_skips: u64,
    oversize_skips: u64,
    per_type: HashMap<TypeId, (TypeStats, &'static str)>,
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
        Self {
            cache: LruCache::unbounded(),
            cur_bytes: 0,
            cap_bytes,
            system: System::new(),
            hits: 0,
            misses: 0,
            bytes_evicted: 0,
            pressure_skips: 0,
            oversize_skips: 0,
            per_type: HashMap::new(),
            total_render_time: Duration::ZERO,
        }
    }

    /// Current memory footprint of cached images, in bytes.
    pub fn current_bytes(&self) -> usize {
        self.cur_bytes
    }

    /// Configured maximum capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.cap_bytes
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
        self.per_type.clear();
        self.total_render_time = Duration::ZERO;
    }

    /// Drop all cached entries.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.cur_bytes = 0;
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
        image.pixels.len()
    }

    /// Evict least-recently-used entries until `needed` more bytes fit
    /// under the configured cap.
    fn evict_to_fit(&mut self, needed: usize) {
        while self.cur_bytes + needed > self.cap_bytes {
            match self.cache.pop_lru() {
                Some((_, img)) => {
                    let b = Self::image_bytes(&img);
                    self.cur_bytes = self.cur_bytes.saturating_sub(b);
                    self.bytes_evicted = self.bytes_evicted.saturating_add(b as u64);
                }
                None => break,
            }
        }
    }

    /// Evict entries until system memory pressure subsides or the cache
    /// is empty.
    fn shed_under_pressure(&mut self) {
        while self.under_memory_pressure() {
            match self.cache.pop_lru() {
                Some((_, img)) => {
                    let b = Self::image_bytes(&img);
                    self.cur_bytes = self.cur_bytes.saturating_sub(b);
                    self.bytes_evicted = self.bytes_evicted.saturating_add(b as u64);
                }
                None => break,
            }
        }
    }
}

impl Default for CachingRenderContext {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderContext for CachingRenderContext {
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

        // hit_or_miss is the common-tail return point so timing
        // bookkeeping lives in exactly one place.
        let (img, was_hit) = {
            let key = CacheKey::of(component, size, target);
            if let Some(img) = self.cache.get(&key).cloned() {
                (img, true)
            } else {
                // Miss path: produce the image, then decide whether to
                // admit it. Nested `ctx.render` calls happen inside
                // `component.render`, which is why timing is wrapped
                // around the whole block.
                let img = component.render(size, target, self);
                let bytes = Self::image_bytes(&img);

                if bytes > self.cap_bytes {
                    self.oversize_skips += 1;
                } else {
                    self.evict_to_fit(bytes);
                    if self.under_memory_pressure() {
                        self.shed_under_pressure();
                        self.pressure_skips += 1;
                    } else {
                        self.cache.put(key, img.clone());
                        self.cur_bytes += bytes;
                    }
                }
                (img, false)
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
        if was_hit {
            self.hits += 1;
            stats.hits += 1;
        } else {
            self.misses += 1;
            stats.misses += 1;
        }
        stats.inclusive_time += inclusive;
        stats.self_time += self_time;

        img
    }
}
