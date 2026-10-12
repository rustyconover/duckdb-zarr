//! The DuckDB side of zarr reading. Everything engine-independent lives in
//! `crates/zarr-core` and is re-exported here under the module paths the
//! table functions already use.

pub use zarr_core::bounds;
pub mod duckdb_store;
pub mod meta;
pub mod scan;
pub mod types;
