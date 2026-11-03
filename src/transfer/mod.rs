//! Data in and out of a connection: CSV import, exports (text, and Parquet), comparing two connections, result snapshots.

pub(crate) mod diff;
pub(crate) mod export;
pub(crate) mod import;
pub(crate) mod parquet;
pub(crate) mod snapshots;
