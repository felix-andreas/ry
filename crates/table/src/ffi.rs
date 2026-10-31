//! Zero-copy interchange through the Arrow C data interface.
//!
//! The R `polars` and `arrow` packages, duckdb and nanoarrow all speak this interface, so a frame
//! crosses to and from them by handing over buffer pointers and a release callback, never by
//! copying data. A single column travels as one array, and a whole frame as a struct array whose
//! children are its columns, which is how `arrow::RecordBatch` and nanoarrow move a data frame.

use crate::{Error, Result, Table};
use polars::prelude::{CompatLevel, DataFrame, IntoColumn, PlSmallStr, Series};
use polars_arrow::{
    array::{Array, StructArray},
    datatypes::{ArrowDataType, Field},
    ffi::{export_array_to_c, export_field_to_c, import_array_from_c, import_field_from_c},
};

pub use polars_arrow::ffi::{ArrowArray, ArrowSchema};

/// An exported array and the schema that describes it. The consumer takes ownership of both and
/// calls their release callbacks when it is done, which drops the Arrow buffers on this side.
pub struct Exported {
    pub array: ArrowArray,
    pub schema: ArrowSchema,
}

/// Exports one column. Several chunks are first concatenated, because the C interface describes
/// one contiguous array; a column produced by a query is usually a single chunk already.
pub fn export_column(table: &mut Table, name: &str) -> Result<Exported> {
    let frame = table.frame()?;
    let column = frame
        .column(name)
        .map_err(|_| Error::NoColumn(name.to_string()))?;
    let series = column.as_materialized_series().rechunk();
    let field = series.field().to_arrow(CompatLevel::newest());
    Ok(Exported {
        array: export_array_to_c(series.to_arrow(0, CompatLevel::newest())),
        schema: export_field_to_c(&field),
    })
}

/// Exports the whole frame as a struct array with one child per column.
pub fn export_table(table: &mut Table) -> Result<Exported> {
    let frame = table.frame()?;
    let height = frame.height();
    let mut fields = Vec::with_capacity(frame.width());
    let mut arrays = Vec::with_capacity(frame.width());
    for column in frame.columns() {
        let series = column.as_materialized_series().rechunk();
        fields.push(series.field().to_arrow(CompatLevel::newest()));
        arrays.push(series.to_arrow(0, CompatLevel::newest()));
    }
    let dtype = ArrowDataType::Struct(fields);
    let array = StructArray::try_new(dtype.clone(), height, arrays, None)?;
    let field = Field::new(PlSmallStr::from_static("frame"), dtype, false);
    Ok(Exported {
        array: export_array_to_c(array.boxed()),
        schema: export_field_to_c(&field),
    })
}

/// Imports one array as a column named after its schema field, taking ownership of the array.
///
/// # Safety
///
/// `array` and `schema` must be valid according to the Arrow C data interface, and `array` must
/// match `schema`.
pub unsafe fn import_column(array: ArrowArray, schema: &ArrowSchema) -> Result<Series> {
    let field = unsafe { import_field_from_c(schema) }?;
    let array = unsafe { import_array_from_c(array, field.dtype.clone()) }?;
    Ok(Series::from_arrow(field.name, array)?)
}

/// Imports a struct array as a frame, one column per child.
///
/// # Safety
///
/// As for [`import_column`], and the array must be a struct array.
pub unsafe fn import_table(array: ArrowArray, schema: &ArrowSchema) -> Result<Table> {
    let field = unsafe { import_field_from_c(schema) }?;
    let array = unsafe { import_array_from_c(array, field.dtype.clone()) }?;
    let array = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| {
            Error::Query("an imported frame must be an Arrow struct array".to_string())
        })?;
    let ArrowDataType::Struct(fields) = array.dtype() else {
        return Err(Error::Query(
            "an imported frame must be an Arrow struct array".to_string(),
        ));
    };
    let mut columns = Vec::with_capacity(fields.len());
    for (field, values) in fields.iter().zip(array.values()) {
        columns.push(Series::from_arrow(field.name.clone(), values.clone())?.into_column());
    }
    Table::new(DataFrame::new(array.len(), columns)?)
}
