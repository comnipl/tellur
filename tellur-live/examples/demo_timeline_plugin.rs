//! Live-preview plugin entry for the shared `demo_scene` timeline.
//!
//! The actual scene logic lives in `demo_scene/mod.rs` so the same
//! timeline can also be encoded to mp4 by `demo_timeline_mp4`.

#[path = "demo_scene/mod.rs"]
mod scene;

// The demo scene now builds a native `TimelineComponent` (`Scene`), so it is
// exported directly through the v6 collection. The scene is authored against the
// 1920x1080 logical canvas (`SCENE_CANVAS`), passed here so the resolve pass
// lays the tree out at SCENE_SIZE — matching the original `.render(SCENE_SIZE,…)`.
tellur_live::export_timeline!(
    "main",
    scene::TITLE,
    scene::build_timeline,
    canvas = (1920.0, 1080.0)
);
