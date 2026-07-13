pub mod audio;
pub mod builder;
pub mod cache_budget;
pub mod clip;
pub mod color;
pub mod composite;
pub mod dyn_compare;
pub mod easing;
pub mod effect;
pub mod fragment;
pub mod geometry;
pub mod interpolate;
pub mod layer;
pub mod layout;
#[cfg(feature = "latex")]
pub mod math;
pub mod phase;
pub mod placement;
pub mod raster;
pub mod render_context;
pub(crate) mod scalar;
pub mod shapes;
pub mod span;
pub mod text;
pub mod time;
pub mod timeline;
pub mod timeline_component;
pub mod timeline_container;
pub mod vector;
pub mod video_decode;
pub mod window;

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

/// Clears both global composite caches (`composite::clear_composite_frames_cache`
/// and `layer::clear_composite_children_cache`), releasing all VRAM/RAM they
/// pin. Downstream crates call this as an emergency eviction path under
/// memory pressure, since the caches otherwise only shrink via their own
/// LRU/byte-cap eviction on the next `put`.
pub fn clear_composite_caches() {
    composite::clear_composite_frames_cache();
    layer::clear_composite_children_cache();
}
