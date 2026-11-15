//! Local route library and autonomous Linnea mining.
pub mod routes;
pub mod runtime;
mod vision;

pub use routes::{Route, RouteEntry, RouteStore};
pub use runtime::{MiningReport, MiningRequest};
mod navigation;
