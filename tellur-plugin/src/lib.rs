//! The timeline plugin ABI contract shared by authored projects and the
//! live-preview host.
//!
//! An authored timeline is compiled to a `cdylib` that exports a single entry
//! symbol ([`ENTRY_SYMBOL`]) returning a [`TimelineCollection`]; the host
//! `dlopen`s the library and calls it. This contract intentionally uses Rust
//! types across the dynamic-library boundary, so it is sound only when the host
//! and plugin are built from the same `tellur` version, toolchain, and resolved
//! boundary-crate versions — it is NOT a stable C ABI. The entry symbol is
//! versioned so a stale plugin fails to resolve cleanly instead of hitting a
//! vtable mismatch, and [`abi::ABI_FINGERPRINT_SYMBOL`] carries a finer-grained
//! fingerprint checked at load time.

use tellur_core::geometry::Vec2;
use tellur_core::raster::{RasterImage, RasterResidency, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline_component::{
    resolve, resolve_with_canvas, Arrangement, AudioBuffer, ResolveError, ResolvedTimeline,
    TimelineComponent,
};

// Re-exported under a hidden name so `export_timeline!` can reach `tellur-core`
// through `$crate` regardless of how the plugin author depends on it — directly
// on `tellur-core`, or only on the `tellur` facade (which pulls this crate in
// transitively). The macro must NOT hardcode `::tellur_core`, which only
// resolves when the author lists `tellur-core` as a direct dependency.
#[doc(hidden)]
pub use tellur_core as __core;

/// ABI version carried by the entry symbol.
///
/// Bumped to `v2` for the timeline subsystem migration: the collection now
/// carries the new [`TimelineComponent`] model and gained an `arrangement`
/// vtable slot. The host resolves this symbol with an unchecked `transmute_copy`,
/// so a stale 2-method `v1` `.so` would dlsym/transmute fine yet hit a
/// vtable-slot UB at call time. Renaming the symbol makes the lookup fail
/// cleanly on an old plugin instead.
///
/// Bumped to `v3` for motion blur: `RenderContext` (which the host passes
/// across the boundary as `&mut dyn`) gained the `motion_blur_enabled`
/// vtable slot and `GpuRasterBackend` gained `temporal_average`, so a `v2`
/// plugin calling through a `v3` host context would hit the same class of
/// vtable mismatch.
///
/// Bumped to `v4` for live-preview audio windows: `TimelineCollection` gained
/// `render_audio_window`, so stale `v3` plugins must fail at `dlsym` instead of
/// landing on the wrong vtable slot.
///
/// Bumped to `v5` for consumer-demanded raster residency:
/// `RenderContext::render` gained a residency argument and the trait gained
/// `ensure_residency`, `GpuRasterBackend` gained `upload`, and
/// `TimelineComponent::frame` and `TimelineCollection::build` gained residency
/// arguments. Stale `v4` plugins must fail before calling through any changed
/// trait object.
///
/// Bumped to `v6` for double-precision timeline seconds: `TimelineTime` and
/// the timeline component/collection duration, placement, arrangement, and
/// audio-window methods now carry `f64`. Stale `v5` plugins therefore have
/// incompatible value layouts and vtable signatures and must fail at lookup.
///
/// Bumped to `v7` because `RasterComponent` gained clone support through a new
/// supertrait, changing the vtable layout of raster trait objects passed to the
/// host render context. Stale `v6` plugins must fail before crossing that
/// boundary.
///
/// Bumped to `v8` because configurable stroke styles changed the layout of
/// `Stroke`, and therefore `VectorGraphic`, which crosses the live-plugin
/// boundary through `GpuRasterBackend::rasterize`. Stale `v7` plugins must fail
/// before passing an incompatible graphic layout to the host renderer.
///
/// Bumped to `v9` because `TimelineComponent` gained clone support through a
/// new supertrait, changing the vtable layout of timeline trait objects held by
/// live-preview plugins. Stale `v8` plugins must fail before the host loads an
/// incompatible component contract.
pub const ENTRY_SYMBOL: &[u8] = b"tellur_timeline_collection_v9\0";

pub mod abi;
pub use abi::{
    validate_plugin_fingerprint, AbiFingerprintFn, AbiMismatchError, ABI_FINGERPRINT,
    ABI_FINGERPRINT_SYMBOL,
};

/// Export the C ABI fingerprint symbol alongside timeline entry points.
#[doc(hidden)]
#[macro_export]
macro_rules! __tellur_export_abi_fingerprint {
    () => {
        #[no_mangle]
        pub extern "C" fn tellur_abi_fingerprint_v1() -> *const ::std::os::raw::c_char {
            $crate::abi::ABI_FINGERPRINT_C.as_ptr().cast()
        }
    };
}

/// The signature of the [`ENTRY_SYMBOL`] entry point a plugin exports.
pub type EntryFn = unsafe extern "Rust" fn() -> Box<dyn TimelineCollection>;

/// Metadata for one timeline exposed by a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineInfo {
    pub id: String,
    pub title: String,
    pub duration: f64,
    /// A resolve-time error for this timeline, if its tree failed to resolve
    /// (e.g. a media probe failure or a timeless root). Mirrors the
    /// `last_error` display the server already surfaces for plugin load
    /// failures; `None` when the timeline resolved cleanly.
    pub error: Option<String>,
}

/// A collection of timelines exported by one dynamic library.
pub trait TimelineCollection: Send {
    fn timelines(&self) -> Vec<TimelineInfo>;

    /// Builds a frame with the representation requested by its consumer.
    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage>;

    /// The resolved arrangement tree for `id`, for the live UI to introspect.
    ///
    /// Defaulted to `None` so existing collections can migrate in stages
    /// (audit B.4): a collection that has not yet built a resolved tree simply
    /// returns nothing here.
    fn arrangement(&self, _id: &str) -> Option<Arrangement> {
        None
    }

    /// The block-rendered audio mix-down for `id` at `rate` / `channels`, for the live
    /// preview to mux into its video stream. `None` means the collection
    /// contributes no audio (the preview then muxes a silent track for a
    /// consistent A/V stream structure). Defaulted so collections migrate in
    /// stages, mirroring [`arrangement`](Self::arrangement).
    fn render_audio(&self, _id: &str, _rate: u32, _channels: u16) -> Option<AudioBuffer> {
        None
    }

    /// Block-rendered audio mix-down for `[start, start + duration)`, used by the live
    /// preview when it encodes an individual cache segment. Custom collections
    /// must implement this explicitly to preview audio; falling back to the
    /// whole-track [`render_audio`](Self::render_audio) would reintroduce the
    /// per-segment full-timeline work this API avoids.
    fn render_audio_window(
        &self,
        _id: &str,
        _start: f64,
        _duration: f64,
        _rate: u32,
        _channels: u16,
    ) -> Option<AudioBuffer> {
        None
    }
}

/// Wraps a single resolved [`TimelineComponent`] as a one-entry collection.
///
/// The tree is resolved ONCE at collection construction (plugin-side — the host
/// only sees `Box<dyn TimelineCollection>`, audit B1) and the result is stored
/// as a [`Result`]: resolving probes media and can fail, but the entry fn
/// cannot return a `Result` across the dylib boundary and panicking there is
/// UB-adjacent (audit M5). So the error is stored and surfaced per query —
/// frame builds yield `None`, `timelines` carries the error string, and
/// `arrangement` yields `None`.
pub struct SingleTimeline {
    id: &'static str,
    title: &'static str,
    resolved: Result<ResolvedTimeline, ResolveError>,
}

/// Resolves `root` and wraps it as a one-entry [`TimelineCollection`].
///
/// `resolve` CONSUMES the tree (audit M1) and can fail; the result is kept
/// intact so the entry fn stays panic-free.
pub fn single_timeline<T: TimelineComponent + Send + 'static>(
    id: &'static str,
    title: &'static str,
    root: T,
) -> SingleTimeline {
    SingleTimeline {
        id,
        title,
        resolved: resolve(root),
    }
}

/// Like [`single_timeline`] but resolves against an explicit logical `canvas`.
pub fn single_timeline_with_canvas<T: TimelineComponent + Send + 'static>(
    id: &'static str,
    title: &'static str,
    root: T,
    canvas: Vec2,
) -> SingleTimeline {
    SingleTimeline {
        id,
        title,
        resolved: resolve_with_canvas(root, canvas),
    }
}

impl SingleTimeline {
    fn resolved(&self) -> Option<&ResolvedTimeline> {
        self.resolved.as_ref().ok()
    }
}

impl TimelineCollection for SingleTimeline {
    fn timelines(&self) -> Vec<TimelineInfo> {
        let (duration, error) = match &self.resolved {
            Ok(resolved) => (resolved.duration(), None),
            Err(e) => (0.0, Some(e.to_string())),
        };
        vec![TimelineInfo {
            id: self.id.to_owned(),
            title: self.title.to_owned(),
            duration,
            error,
        }]
    }

    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
        residency: RasterResidency,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        if id != self.id {
            return None;
        }
        self.resolved()?.frame(t, target, residency, ctx)
    }

    fn arrangement(&self, id: &str) -> Option<Arrangement> {
        if id != self.id {
            return None;
        }
        // Root offset 0: the resolved tree's start coincides with the global
        // axis, so the walk stamps absolute starts/ends from there.
        Some(self.resolved()?.source().arrangement(0.0))
    }

    fn render_audio(&self, id: &str, rate: u32, channels: u16) -> Option<AudioBuffer> {
        if id != self.id {
            return None;
        }
        // `render_audio` returns a buffer of the whole resolved length — silent
        // when the tree has no audio sources — so the preview always gets a
        // determinate track to mux.
        Some(self.resolved()?.render_audio(rate, channels))
    }

    fn render_audio_window(
        &self,
        id: &str,
        start: f64,
        duration: f64,
        rate: u32,
        channels: u16,
    ) -> Option<AudioBuffer> {
        if id != self.id {
            return None;
        }
        Some(
            self.resolved()?
                .render_audio_window(start, duration, rate, channels),
        )
    }
}

/// Exports a single [`TimelineComponent`] root from a `cdylib`.
///
/// ```ignore
/// #[tellur_core::component(timeline)]
/// fn Main() -> impl tellur_core::timeline_component::TimelineComponent { ... }
///
/// tellur_plugin::export_timeline!(
///     root = Main::builder().build(),
///     title = "Main",
/// );
/// ```
///
/// `title` is the human-readable label surfaced by [`TimelineInfo`]. The
/// machine-facing timeline id defaults to `"main"`; set `id = "..."` after
/// `title` when a different stable lookup key is required. Set
/// `canvas = (width, height)` last to resolve the root against an explicit
/// logical canvas.
#[macro_export]
macro_rules! export_timeline {
    (@__emit root = $root:expr, title = $title:expr, id = $id:expr) => {
        $crate::__tellur_export_abi_fingerprint!();
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v9(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            let __root = $root;
            ::std::boxed::Box::new($crate::single_timeline($id, $title, __root))
        }
    };
    (
        @__emit
        root = $root:expr,
        title = $title:expr,
        id = $id:expr,
        canvas = ($w:expr, $h:expr)
    ) => {
        $crate::__tellur_export_abi_fingerprint!();
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v9(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            let __root = $root;
            ::std::boxed::Box::new($crate::single_timeline_with_canvas(
                $id,
                $title,
                __root,
                $crate::__core::geometry::Vec2($w, $h),
            ))
        }
    };
    (root = $root:expr, title = $title:expr $(,)?) => {
        $crate::export_timeline!(@__emit root = $root, title = $title, id = "main");
    };
    (root = $root:expr, title = $title:expr, canvas = ($w:expr, $h:expr) $(,)?) => {
        $crate::export_timeline!(
            @__emit
            root = $root,
            title = $title,
            id = "main",
            canvas = ($w, $h)
        );
    };
    (root = $root:expr, title = $title:expr, id = $id:expr $(,)?) => {
        $crate::export_timeline!(@__emit root = $root, title = $title, id = $id);
    };
    (
        root = $root:expr,
        title = $title:expr,
        id = $id:expr,
        canvas = ($w:expr, $h:expr) $(,)?
    ) => {
        $crate::export_timeline!(
            @__emit
            root = $root,
            title = $title,
            id = $id,
            canvas = ($w, $h)
        );
    };
    ($id:expr, $title:expr, $builder:path $(,)?) => {
        ::core::compile_error!(
            "export_timeline! now accepts `root = <TimelineComponent expression>, title = <title>`; call the former factory explicitly as `root = build()`"
        );
    };
    ($id:expr, $title:expr, $builder:path, canvas = ($w:expr, $h:expr) $(,)?) => {
        ::core::compile_error!(
            "export_timeline! now accepts `root = <TimelineComponent expression>, title = <title>, canvas = (width, height)`; call the former factory explicitly as `root = build()`"
        );
    };
}

/// Exports a custom [`TimelineCollection`] builder from a `cdylib`.
#[macro_export]
macro_rules! export_timeline_collection {
    ($builder:path) => {
        $crate::__tellur_export_abi_fingerprint!();
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v9(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($builder())
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::raster::{PixelFormat, RasterComponent, RasterResidency};
    use tellur_core::render_context::PassThrough;
    use tellur_core::timeline_component::{Clock, NodeKind, Timed};
    use tellur_core::timeline_container::Timeline as TimelineContainer;

    // A trivial timeless visual so a small native timeline can be built without
    // pulling in media decode. Reaches the timeline world via the one-way
    // blanket over `RasterComponent`.
    #[derive(Clone, PartialEq, Hash)]
    struct Dot;

    impl RasterComponent for Dot {
        fn layout(&self, _c: Constraints) -> Vec2 {
            Vec2(1.0, 1.0)
        }

        fn render(
            &self,
            _s: Vec2,
            _t: Resolution,
            _residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> RasterImage {
            RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8, 0, 0, 0])
        }
    }

    // Builds an overlay `Timeline` of two windowed visuals — a small native
    // timeline that resolves to a determinate length (no media probe).
    fn build_new_api_timeline() -> TimelineContainer {
        TimelineContainer::builder()
            .child(Dot.at(0.0..2.0))
            .child(Dot.at(0.0..3.0))
            .build()
    }

    static LAST_COLLECTION_RESIDENCY: AtomicU8 = AtomicU8::new(0);

    #[derive(Clone, PartialEq, Hash)]
    struct ResidencyTimeline;

    impl TimelineComponent for ResidencyTimeline {
        fn duration(&self) -> Option<f64> {
            Some(1.0)
        }

        fn frame(
            &self,
            _clock: Clock<'_>,
            _canvas: Vec2,
            _target: Resolution,
            residency: RasterResidency,
            _ctx: &mut dyn RenderContext,
        ) -> Option<RasterImage> {
            let value = match residency {
                RasterResidency::Cpu => 1,
                RasterResidency::Gpu => 2,
            };
            LAST_COLLECTION_RESIDENCY.store(value, Ordering::Relaxed);
            Some(RasterImage::cpu(
                1,
                1,
                PixelFormat::Rgba8,
                vec![0u8, 0, 0, 0],
            ))
        }

        fn arrangement(&self, offset: f64) -> Arrangement {
            Arrangement {
                kind: NodeKind::Video,
                label: String::new(),
                name: None,
                source: None,
                start: offset,
                end: offset + 1.0,
                trim: None,
                triggers: Vec::new(),
                children: Vec::new(),
            }
        }
    }

    #[test]
    fn entry_symbol_marks_the_cloneable_timeline_component_abi() {
        assert_eq!(ENTRY_SYMBOL, b"tellur_timeline_collection_v9\0");
    }

    #[test]
    fn single_timeline_forwards_residency() {
        let collection = single_timeline("main", "Main", ResidencyTimeline);
        let mut ctx = PassThrough;
        let target = Resolution::new(1, 1);

        LAST_COLLECTION_RESIDENCY.store(0, Ordering::Relaxed);
        let _ = collection.build(
            "main",
            TimelineTime::new(0.0),
            target,
            RasterResidency::Gpu,
            &mut ctx,
        );
        assert_eq!(LAST_COLLECTION_RESIDENCY.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn single_timeline_surfaces_resolved_duration() {
        let collection = single_timeline("main", "Main", build_new_api_timeline());
        let infos = collection.timelines();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, "main");
        assert_eq!(infos[0].title, "Main");
        // The overlay length is the max child window end (3.0).
        assert_eq!(infos[0].duration, 3.0);
        assert_eq!(infos[0].error, None);
    }

    #[test]
    fn single_timeline_arrangement_walks_the_resolved_tree() {
        let collection = single_timeline("main", "Main", build_new_api_timeline());

        // A non-matching id yields nothing.
        assert!(collection.arrangement("other").is_none());

        let root = collection
            .arrangement("main")
            .expect("the resolved tree has an arrangement");

        // The root is the overlay Timeline spanning [0, 3].
        assert_eq!(root.kind, NodeKind::Timeline);
        assert_eq!(root.start, 0.0);
        assert_eq!(root.end, 3.0);
        assert!(root.trim.is_none());

        // Two children, each a Video-kind visual leaf (via the blanket): a
        // timeless visual lives on the video (映像) track.
        assert_eq!(root.children.len(), 2);
        for child in &root.children {
            assert_eq!(child.kind, NodeKind::Video);
            assert!(child.children.is_empty());
        }
    }
}
