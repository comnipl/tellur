pub mod build_watch;
pub mod plugin;
pub mod server;

pub use build_watch::{
    run_build_once, AutoBuildOptions, CompileSnapshot, CompileState, CompileStatus,
};
pub use plugin::{HotReloadPlugin, PluginLoadError};
pub use server::{serve, ServerOptions};

// The plugin authoring contract now lives in `tellur-plugin`. Re-export it so
// existing call sites — `tellur_live::export_timeline!`, the demo plugins, and
// the server's `TimelineInfo` — keep reaching it through `tellur_live`.
pub use tellur_plugin::{
    export_legacy_timeline, export_timeline, export_timeline_collection, single_timeline,
    single_timeline_with_canvas, LegacyTimeline, SingleTimeline, TimelineCollection, TimelineInfo,
};
