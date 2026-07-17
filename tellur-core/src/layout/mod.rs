//! Layout containers for composing components.
//!
//! tellur's layout splits into two worlds that share one component protocol
//! ([`VectorComponent`](crate::vector::VectorComponent) /
//! [`RasterComponent`](crate::raster::RasterComponent): `layout(Constraints)
//! -> Vec2`, then `render` at the chosen size):
//!
//! - The **canvas world**: a fixed-size [`Layer`](crate::layer::Layer) whose
//!   children carry their own absolute offsets via
//!   [`Positioned`](crate::placement::Positioned), plus the auto-fit
//!   [`Fragment`](crate::fragment::Fragment) for transparent grouping.
//! - The **flow world** in this module, where parents hand
//!   [`Constraints`](crate::geometry::Constraints) down and children report
//!   sizes up:
//!   - [`Padding`] adds an outer border of empty space around a child.
//!   - [`Frame`] picks the outer width / height per axis with [`SizeMode`]
//!     (Fill / Hug / Fixed) and keeps its child at top-left. Wrap the child in
//!     [`Positioned`](crate::placement::Positioned) for anchor placement.
//!   - [`Flex`] arranges children along an axis with spacing, main/cross
//!     alignment, and flexbox-style grow weights: a [`Flexible`] child (made
//!     with `.grow(w)` or [`Flexible::spacer`]) takes a weighted share of the
//!     leftover main-axis space. `CrossAlign::Stretch` propagates a tight
//!     cross-axis constraint so children fill the flex's cross extent.
//!   - [`DecoratedBox`] paints a background fill (and optionally a border
//!     on the vector variant) behind the child.
//!   - [`SizedBox`] is an empty placeholder of a given size.
//!
//! Vector containers live at the module root and operate on
//! `Box<dyn VectorComponent>`. Their raster counterparts share the same
//! names under [`raster`] and operate on `Box<dyn RasterComponent>`. Each
//! container's source file holds both variants side by side.

mod decorated_box;
mod flex;
mod frame;
mod padding;
mod sized_box;

pub use crate::geometry::Axis;
pub use decorated_box::DecoratedBox;
pub use flex::{CrossAlign, Flex, Flexible, MainAlign, VectorFlex};
pub use frame::{Frame, SizeMode};
pub use padding::Padding;
pub use sized_box::SizedBox;

// Re-export the raster flex trait at the module root, mirroring how
// `placement` re-exports `RasterPlacement`.
pub use raster::RasterFlex;

pub mod raster {
    //! Raster equivalents of the vector layout containers. Same shape
    //! and semantics; operate on `Box<dyn RasterComponent>`.

    pub use super::decorated_box::raster::DecoratedBox;
    pub use super::flex::raster::{Flex, Flexible, RasterFlex};
    pub use super::frame::raster::Frame;
    pub use super::padding::raster::Padding;
    pub use super::sized_box::raster::SizedBox;
}
