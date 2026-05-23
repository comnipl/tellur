pub trait Component {}

// Compile-time guarantee that `Component` is dyn-safe. If a future change
// introduces a method that breaks object safety (generics, `self` by value,
// `Self` return type, etc.), this assertion fails to compile.
const _: Option<&dyn Component> = None;
