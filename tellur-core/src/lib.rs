pub mod color;
pub mod dyn_compare;
pub mod geometry;
pub mod interpolate;
pub mod layer;
pub mod layout;
pub mod phase;
pub mod placement;
pub mod raster;
pub mod render_context;
pub mod shapes;
pub mod time;
pub mod timeline;
pub mod vector;

// Re-export the component macros so users only need to depend on `tellur-core`.
// The macros emit fully-qualified `::tellur_core::...` paths, so this self-name
// must be reachable inside this crate too if someone uses them internally.
extern crate self as tellur_core;

pub use tellur_macros::{raster_component, vector_component};
