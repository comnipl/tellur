//! The timeline plugin ABI contract shared by authored projects and the
//! live-preview host.
//!
//! An authored timeline is compiled to a `cdylib` that exports a single entry
//! symbol ([`ENTRY_SYMBOL`]) returning a [`TimelineCollection`]; the host
//! `dlopen`s the library and calls it. This contract intentionally uses Rust
//! types across the dynamic-library boundary, so it is sound only when the host
//! and plugin are built from the same `tellur` version and toolchain — it is
//! NOT a stable C ABI. The entry symbol is versioned so a stale plugin fails to
//! resolve cleanly instead of hitting a vtable mismatch.

use std::hash::{Hash, Hasher};

use tellur_core::geometry::Vec2;
use tellur_core::raster::{RasterImage, Resolution};
use tellur_core::render_context::RenderContext;
use tellur_core::time::TimelineTime;
use tellur_core::timeline::Timeline;
use tellur_core::timeline_component::{
    resolve, resolve_with_canvas, Arrangement, AudioBuffer, Clock, NodeKind, ResolveError,
    ResolvedTimeline, TimelineComponent,
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
pub const ENTRY_SYMBOL: &[u8] = b"tellur_timeline_collection_v2\0";

/// The signature of the [`ENTRY_SYMBOL`] entry point a plugin exports.
pub type EntryFn = unsafe extern "Rust" fn() -> Box<dyn TimelineCollection>;

/// Metadata for one timeline exposed by a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineInfo {
    pub id: String,
    pub title: String,
    pub duration: f32,
    /// A resolve-time error for this timeline, if its tree failed to resolve
    /// (e.g. a media probe failure or a timeless root). Mirrors the
    /// `last_error` display the server already surfaces for plugin load
    /// failures; `None` when the timeline resolved cleanly.
    pub error: Option<String>,
}

/// A collection of timelines exported by one dynamic library.
pub trait TimelineCollection: Send {
    fn timelines(&self) -> Vec<TimelineInfo>;

    fn build(
        &self,
        id: &str,
        t: TimelineTime,
        target: Resolution,
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

    /// The eager audio mix-down for `id` at `rate` / `channels`, for the live
    /// preview to mux into its video stream. `None` means the collection
    /// contributes no audio (the preview then muxes a silent track for a
    /// consistent A/V stream structure). Defaulted so collections migrate in
    /// stages, mirroring [`arrangement`](Self::arrangement).
    fn render_audio(&self, _id: &str, _rate: u32, _channels: u16) -> Option<AudioBuffer> {
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
/// `build` yields `None`, `timelines` carries the error string, `arrangement`
/// yields `None`.
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
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        if id != self.id {
            return None;
        }
        self.resolved()?.frame(t, target, ctx)
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
}

/// Adapts an old closure-based [`Timeline`] to the new [`TimelineComponent`]
/// model so the existing demo scene (which still builds a `Timeline`) can be
/// served by the migrated collection without rewriting it.
///
/// A `Timeline` is opaque (a closure with a fixed `duration`), so this presents
/// it as a single timed leaf: its `frame` plays the timeline at the global
/// clock, its length is the timeline's `duration`, and its `arrangement` is one
/// Video-kind node spanning `[0, duration]`.
///
/// `TimelineComponent` requires `PartialEq + Hash` (via the `DynEq`/`DynHash`
/// super-traits). A `Timeline` closure is neither, and timeline nodes are never
/// memoized through `ctx.render` (`.sketch/02 §11`) — this identity is only the
/// builder-marker key — so the wrapper compares all instances equal and hashes
/// to a constant. There is exactly one per collection, so that is sound.
pub struct LegacyTimeline<T: Timeline + Send> {
    timeline: T,
}

impl<T: Timeline + Send> LegacyTimeline<T> {
    /// Wraps `timeline` so it can be placed in the new timeline world.
    pub fn new(timeline: T) -> Self {
        Self { timeline }
    }
}

impl<T: Timeline + Send> PartialEq for LegacyTimeline<T> {
    fn eq(&self, _other: &Self) -> bool {
        // Opaque closure timeline: treat the single wrapped instance as its own
        // identity. Not used as a per-frame cache key (see the type doc).
        true
    }
}

impl<T: Timeline + Send> Eq for LegacyTimeline<T> {}

impl<T: Timeline + Send> Hash for LegacyTimeline<T> {
    fn hash<H: Hasher>(&self, _state: &mut H) {
        // Constant hash: consistent with the all-equal `PartialEq` above.
    }
}

impl<T: Timeline + Send + 'static> TimelineComponent for LegacyTimeline<T> {
    fn duration(&self) -> Option<f32> {
        Some(self.timeline.duration())
    }

    fn frame(
        &self,
        clock: Clock<'_>,
        canvas: Vec2,
        target: Resolution,
        ctx: &mut dyn RenderContext,
    ) -> Option<RasterImage> {
        // The legacy closure handles its own SCENE_SIZE internally; the logical
        // `canvas` is not threaded into it.
        let _ = canvas;
        Some(self.timeline.build(clock.global(), target, ctx))
    }

    fn arrangement(&self, offset: f32) -> Arrangement {
        // One Video-kind node spanning the whole timeline; no source crop,
        // no triggers, no children — the legacy timeline is opaque.
        Arrangement {
            kind: NodeKind::Video,
            label: String::new(),
            name: None,
            source: None,
            start: offset,
            end: offset + self.timeline.duration(),
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

/// Exports a single [`TimelineComponent`] builder from a `cdylib`.
///
/// ```ignore
/// fn build() -> impl tellur_core::timeline_component::TimelineComponent + Send { ... }
/// tellur_plugin::export_timeline!("main", "Main", build);
/// ```
#[macro_export]
macro_rules! export_timeline {
    ($id:expr, $title:expr, $builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline($id, $title, $builder()))
        }
    };
    ($id:expr, $title:expr, $builder:path, canvas = ($w:expr, $h:expr)) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline_with_canvas(
                $id,
                $title,
                $builder(),
                $crate::__core::geometry::Vec2($w, $h),
            ))
        }
    };
}

/// Exports a single OLD closure-based [`tellur_core::timeline::Timeline`]
/// builder from a `cdylib`, wrapping it in a [`LegacyTimeline`] adapter so it
/// serves through the migrated collection unchanged.
///
/// ```ignore
/// fn build() -> impl tellur_core::timeline::Timeline + Send { ... }
/// tellur_plugin::export_legacy_timeline!("main", "Main", build);
/// ```
#[macro_export]
macro_rules! export_legacy_timeline {
    ($id:expr, $title:expr, $builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($crate::single_timeline(
                $id,
                $title,
                $crate::LegacyTimeline::new($builder()),
            ))
        }
    };
}

/// Exports a custom [`TimelineCollection`] builder from a `cdylib`.
#[macro_export]
macro_rules! export_timeline_collection {
    ($builder:path) => {
        #[no_mangle]
        pub extern "Rust" fn tellur_timeline_collection_v2(
        ) -> ::std::boxed::Box<dyn $crate::TimelineCollection> {
            ::std::boxed::Box::new($builder())
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use tellur_core::geometry::{Constraints, Vec2};
    use tellur_core::raster::{PixelFormat, RasterComponent};
    use tellur_core::timeline_component::Timed;
    use tellur_core::timeline_container::Timeline as TimelineContainer;

    // A trivial timeless visual so a small NEW-API timeline can be built without
    // pulling in media decode. Reaches the timeline world via the one-way
    // blanket over `RasterComponent`.
    #[derive(PartialEq, Hash)]
    struct Dot;

    impl RasterComponent for Dot {
        fn layout(&self, _c: Constraints) -> Vec2 {
            Vec2(1.0, 1.0)
        }

        fn render(&self, _s: Vec2, _t: Resolution, _ctx: &mut dyn RenderContext) -> RasterImage {
            RasterImage::cpu(1, 1, PixelFormat::Rgba8, vec![0u8, 0, 0, 0])
        }
    }

    // Builds an overlay `Timeline` of two windowed visuals — a small NEW-API
    // timeline that resolves to a determinate length (no media probe).
    fn build_new_api_timeline() -> TimelineContainer {
        TimelineContainer::builder()
            .child(Dot.at(0.0..2.0))
            .child(Dot.at(0.0..3.0))
            .build()
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

    #[test]
    fn legacy_timeline_adapter_arrangement_is_one_video_span() {
        // The legacy adapter presents an opaque closure timeline as a single
        // Video-kind node spanning its whole duration.
        let legacy = LegacyTimeline::new(tellur_core::timeline::timeline(
            4.0,
            |_t, target: Resolution, _ctx| {
                RasterImage::cpu(
                    target.width,
                    target.height,
                    PixelFormat::Rgba8,
                    vec![0u8; (target.width * target.height * 4) as usize],
                )
            },
        ));
        let collection = single_timeline("main", "Main", legacy);

        assert_eq!(collection.timelines()[0].duration, 4.0);
        let root = collection.arrangement("main").expect("resolves to a span");
        assert_eq!(root.kind, NodeKind::Video);
        assert_eq!(root.start, 0.0);
        assert_eq!(root.end, 4.0);
        assert!(root.children.is_empty());
    }
}
