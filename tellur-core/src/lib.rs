pub mod audio;
pub mod builder;
pub mod color;
pub mod composite;
pub mod dyn_compare;
pub mod fragment;
pub mod geometry;
pub mod interpolate;
pub mod layer;
pub mod layout;
pub mod phase;
pub mod placement;
pub mod raster;
pub mod render_context;
pub mod shapes;
pub mod text;
pub mod time;
pub mod timeline;
pub mod timeline_component;
pub mod timeline_container;
pub mod vector;

// Re-export the component macros so users only need to depend on `tellur-core`.
// The macros emit fully-qualified `::tellur_core::...` paths, so this self-name
// must be reachable inside this crate too if someone uses them internally.
extern crate self as tellur_core;

pub use tellur_macros::{component, raster_component, vector_component, Keyable};

// Re-export `bon` so downstream crates and the component macro can reach its
// runtime (and the `Builder` derive) through `tellur_core` without depending
// on `bon` directly. The component macro emits `::tellur_core::__bon` paths and
// sets `#[builder(crate = ::tellur_core::__bon)]` so generated code resolves.
#[doc(hidden)]
pub use bon as __bon;
