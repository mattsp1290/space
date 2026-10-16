//! UI-independent scan observations. Paths are native, totals are checked, and
//! published accounting is deliberately separate from provisional observations.
mod identity;
mod model;
mod native;
pub use identity::*;
pub use model::*;
pub use native::*;

#[cfg(test)]
mod tests;
