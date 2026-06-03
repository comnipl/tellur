//! Live-preview plugin entry for the shared `demo_scene` timeline.
//!
//! The actual scene logic lives in `demo_scene/mod.rs` so the same
//! timeline can also be encoded to mp4 by `demo_timeline_mp4`.

#[path = "demo_scene/mod.rs"]
mod scene;

// The demo scene still builds the OLD closure-based `Timeline`; the legacy
// adapter macro wraps it so it serves through the migrated v2 collection
// without touching `demo_scene/mod.rs`.
tellur_live::export_legacy_timeline!("main", scene::TITLE, scene::build_timeline);
