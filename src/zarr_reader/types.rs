//! Column types from `zarr_core::types`, plus their DuckDB side: logical types
//! and writing decoded elements into output vectors.

use duckdb::core::{FlatVector, LogicalTypeHandle, LogicalTypeId};

pub use zarr_core::types::*;

/// DuckDB output type of a column.
pub trait DuckDbType {
    fn to_duckdb_type(&self, encoding: &ColumnEncoding) -> LogicalTypeHandle;
}

impl DuckDbType for ZarrDtype {
    /// DuckDB output type for this column, accounting for packed-int decoding.
    fn to_duckdb_type(&self, encoding: &ColumnEncoding) -> LogicalTypeHandle {
        match encoding {
            ColumnEncoding::PackedInt { .. } => LogicalTypeId::Double.into(),
            // Microseconds since the Unix epoch — DuckDB TIMESTAMP's physical layout.
            ColumnEncoding::CfTime(_) => LogicalTypeId::Timestamp.into(),
            ColumnEncoding::Plain => match self {
                Self::Bool => LogicalTypeId::Boolean.into(),
                Self::Int8 => LogicalTypeId::Tinyint.into(),
                Self::Int16 => LogicalTypeId::Smallint.into(),
                Self::Int32 => LogicalTypeId::Integer.into(),
                Self::Int64 => LogicalTypeId::Bigint.into(),
                Self::UInt8 => LogicalTypeId::UTinyint.into(),
                Self::UInt16 => LogicalTypeId::USmallint.into(),
                Self::UInt32 => LogicalTypeId::UInteger.into(),
                Self::UInt64 => LogicalTypeId::UBigint.into(),
                Self::Float32 => LogicalTypeId::Float.into(),
                Self::Float64 => LogicalTypeId::Double.into(),
                Self::String => LogicalTypeId::Varchar.into(),
            },
        }
    }
}

/// Writing decoded values into a DuckDB output vector.
pub trait WriteElement {
    /// Write the element at row-major index `src_idx` into slot `dst` of `vector`.
    fn write_element(&self, vector: &mut FlatVector<'_>, src_idx: usize, dst: usize);
}

impl WriteElement for dyn ColumnValues {
    fn write_element(&self, vector: &mut FlatVector<'_>, src_idx: usize, dst: usize) {
        if let Some(f) = self.as_fixed() {
            super::scan::fill_element(
                vector,
                &f.bytes,
                &f.dtype,
                &f.encoding,
                &f.sentinel,
                src_idx,
                f.elem_size(),
                dst,
            );
        } else if let Some(s) = self.as_strings() {
            super::scan::fill_string_element_pub(vector, &s.strings, src_idx, dst);
        } else {
            unreachable!("ColumnValues is either FixedValues or StringValues")
        }
    }
}
