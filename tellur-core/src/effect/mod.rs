//! Visual effects for vector components.

pub mod outline;
pub mod write;

pub use outline::{OutlineJoin, OutlineSide, Outlined, VectorBuilderOutline, VectorOutline};
pub use write::{TimedWrite, VectorBuilderWrite, VectorWrite, Write, WritePacing};
