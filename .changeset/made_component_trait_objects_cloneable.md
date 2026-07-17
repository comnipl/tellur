---
default: major
---

# Made raster and vector component trait objects cloneable

`Box<dyn RasterComponent>` and `Box<dyn VectorComponent>` now implement `Clone`, enabling function-form `#[component]` definitions to accept boxed effects and children and allowing custom wrappers to clone boxed descendants.

This changes the component implementation contract: custom raster and vector component types must support cloning, normally with `#[derive(Clone)]`. Generic wrappers may also need an explicit `T: Clone` bound.

Live-preview plugins now use the v7 entry symbol and must be rebuilt.
