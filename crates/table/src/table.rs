//! ry's built-in typed table, the value R code writes as `frame(...)` and the checker types as
//! `frame{id: integer[], price: double[]}`.
//!
//! A [`Table`] holds a Polars [`DataFrame`]. Its columns are restricted to the dtypes that have an
//! R vector type (see [`ColumnType`]), so every column can cross into R and back without a lossy
//! conversion. Queries lower from the small IR in [`query`] to a Polars lazy plan, so the planner
//! sees a whole `tbl[i, j, by]` at once and every aggregation runs as a Polars kernel.
//!
//! R code and native code read a column as an R vector. The table itself holds only Arrow data:
//! an R-layout copy, when a reader needs one, belongs to the ALTREP vector that exposes the
//! column (see `altrep`), so there is one place that records whether a column is resident in R.

pub mod convert;
pub mod ffi;
mod group;
pub mod io;
mod join;
mod kernel;
pub mod ops;
pub mod query;

#[cfg(feature = "runtime")]
pub mod altrep;

use convert::RVector;
use polars::prelude::{
    ChunkAgg, Column, DataFrame, DataType, Int32Chunked, IntoColumn, NamedFrom, PlSmallStr, Series,
};
use std::fmt;

pub use polars;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the frame has no column `{0}`")]
    NoColumn(String),
    #[error("column `{name}` has {found} rows, but the frame has {expected}")]
    Length {
        name: String,
        expected: usize,
        found: usize,
    },
    #[error("column `{name}` is {found}, which has no R vector type")]
    UnsupportedType { name: String, found: String },
    #[error("column `{name}` is {found}, but the schema declares {expected}")]
    SchemaMismatch {
        name: String,
        expected: ColumnType,
        found: ColumnType,
    },
    #[error("the schema declares {expected} columns, but the frame has {found}")]
    SchemaWidth { expected: usize, found: usize },
    #[error("cannot parse the frame type `{0}`")]
    SchemaSyntax(String),
    #[error("invalid query: {0}")]
    Query(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Polars(#[from] polars::error::PolarsError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The R vector type of a column, which is also its element type in the checker's `frame{...}`
/// notation. Each one has exactly one Polars dtype, so a table never holds a column R cannot see.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ColumnType {
    /// `logical[]`, a Polars `Boolean` column. R stores it as 32-bit integers, so it always
    /// converts at the boundary.
    Logical,
    /// `integer[]`, `Int32`, with R's `NA_integer_` as an Arrow null.
    Integer,
    /// `double[]`, `Float64`. R's `NA_real_` is an Arrow null, while `NaN` stays a value.
    Double,
    /// `character[]`, `String`.
    Character,
    /// `factor`, an Arrow dictionary (`Enum`) whose categories are the factor's levels in order.
    Factor,
}

impl ColumnType {
    pub fn of(dtype: &DataType) -> Option<ColumnType> {
        Some(match dtype {
            DataType::Boolean => ColumnType::Logical,
            DataType::Int32 => ColumnType::Integer,
            DataType::Float64 => ColumnType::Double,
            DataType::String => ColumnType::Character,
            DataType::Enum(..) | DataType::Categorical(..) => ColumnType::Factor,
            _ => return None,
        })
    }

    /// The name the checker prints for the column's type.
    pub fn name(self) -> &'static str {
        match self {
            ColumnType::Logical => "logical[]",
            ColumnType::Integer => "integer[]",
            ColumnType::Double => "double[]",
            ColumnType::Character => "character[]",
            ColumnType::Factor => "factor",
        }
    }

    fn parse(text: &str) -> Option<ColumnType> {
        Some(match text {
            "logical[]" => ColumnType::Logical,
            "integer[]" => ColumnType::Integer,
            "double[]" => ColumnType::Double,
            "character[]" => ColumnType::Character,
            "factor" => ColumnType::Factor,
            _ => return None,
        })
    }
}

impl fmt::Display for ColumnType {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A frame's columns and their types, in order: the runtime side of the checker's
/// `frame{id: integer[], price: double[]}`. It is derived from a table's Polars schema, never
/// stored beside it, so the two cannot disagree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Schema {
    pub columns: Vec<(PlSmallStr, ColumnType)>,
}

impl Schema {
    pub fn new<N: Into<PlSmallStr>>(columns: impl IntoIterator<Item = (N, ColumnType)>) -> Schema {
        Schema {
            columns: columns
                .into_iter()
                .map(|(name, kind)| (name.into(), kind))
                .collect(),
        }
    }

    pub fn names(&self) -> Vec<&PlSmallStr> {
        self.columns.iter().map(|(name, _)| name).collect()
    }

    pub fn column(&self, name: &str) -> Option<ColumnType> {
        self.columns
            .iter()
            .find(|(column, _)| column == name)
            .map(|(_, kind)| *kind)
    }

    /// Parses the checker's notation, such as `frame{id: integer[], ticker: character[]}`.
    pub fn parse(text: &str) -> Result<Schema> {
        let syntax = || Error::SchemaSyntax(text.to_string());
        let body = text
            .trim()
            .strip_prefix("frame{")
            .and_then(|rest| rest.strip_suffix('}'))
            .ok_or_else(syntax)?;
        let mut columns = Vec::new();
        for field in body
            .split(',')
            .map(str::trim)
            .filter(|field| !field.is_empty())
        {
            let (name, kind) = field.split_once(':').ok_or_else(syntax)?;
            let kind = ColumnType::parse(kind.trim()).ok_or_else(syntax)?;
            columns.push((PlSmallStr::from(name.trim()), kind));
        }
        Ok(Schema { columns })
    }

    /// Checks that `table` has exactly these columns, in this order, with these types: the check
    /// a typed boundary runs when an untyped value enters typed code.
    pub fn check(&self, table: &Table) -> Result<()> {
        let actual = table.schema();
        if actual.columns.len() != self.columns.len() {
            return Err(Error::SchemaWidth {
                expected: self.columns.len(),
                found: actual.columns.len(),
            });
        }
        for ((name, expected), (found_name, found)) in self.columns.iter().zip(&actual.columns) {
            if name != found_name {
                return Err(Error::NoColumn(name.to_string()));
            }
            if expected != found {
                return Err(Error::SchemaMismatch {
                    name: name.to_string(),
                    expected: *expected,
                    found: *found,
                });
            }
        }
        Ok(())
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("frame{")?;
        for (index, (name, kind)) in self.columns.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{name}: {kind}")?;
        }
        formatter.write_str("}")
    }
}

/// A table: a Polars frame of R-typed columns and its sort key.
///
/// Updates follow R's copy-on-modify rule, driven by the reference count of the R object that
/// holds the table: an unshared table is updated in place through `&mut self`, and a shared one
/// is first [`fork`](Table::fork)ed, which copies only the column list, because the Arrow
/// buffers themselves are immutable and shared.
///
/// Every column is one contiguous chunk, so kernels read it as a slice without copying.
#[derive(Clone, Debug, Default)]
pub struct Table {
    frame: DataFrame,
    /// The columns the table is sorted by, as `setkey` leaves it. Keyed joins use it.
    key: Vec<PlSmallStr>,
}

impl Table {
    /// Builds a table from a frame, converting every column to its R dtype: narrower integers
    /// widen to `Int32`, wider ones and unsigned 32-bit ones become `Float64` (as R reads an
    /// `integer64` without the bit64 package), floats widen to `Float64`, and a categorical
    /// becomes an `Enum` so its level order is fixed. A valid `i32::MIN` becomes a null, because
    /// R reads that bit pattern as `NA_integer_`.
    pub fn new(frame: DataFrame) -> Result<Table> {
        let mut columns = Vec::with_capacity(frame.width());
        for column in frame.into_columns() {
            columns.push(normalize(column)?);
        }
        Ok(Table {
            frame: DataFrame::new_infer_height(columns)?,
            key: Vec::new(),
        })
    }

    /// Builds a table from R vectors, as `frame(id = 1:3, ...)` does. Vectors move into Arrow
    /// buffers without a copy unless they hold NAs, which need a validity bitmap.
    pub fn from_r(columns: Vec<(PlSmallStr, RVector)>) -> Result<Table> {
        let height = columns.first().map_or(0, |(_, vector)| vector.len());
        let mut converted = Vec::with_capacity(columns.len());
        for (name, vector) in columns {
            if vector.len() != height {
                return Err(Error::Length {
                    name: name.to_string(),
                    expected: height,
                    found: vector.len(),
                });
            }
            converted.push(convert::to_series(name, vector)?.into_column());
        }
        Ok(Table {
            frame: DataFrame::new(height, converted)?,
            key: Vec::new(),
        })
    }

    pub fn height(&self) -> usize {
        self.frame.height()
    }

    pub fn width(&self) -> usize {
        self.frame.width()
    }

    pub fn schema(&self) -> Schema {
        Schema {
            columns: self
                .frame
                .columns()
                .iter()
                .filter_map(|column| {
                    // `Table::new` and `set_column` normalize every column, so none is skipped.
                    ColumnType::of(column.dtype()).map(|kind| (column.name().clone(), kind))
                })
                .collect(),
        }
    }

    pub fn key(&self) -> &[PlSmallStr] {
        &self.key
    }

    pub fn frame(&self) -> &DataFrame {
        &self.frame
    }

    pub fn into_frame(self) -> DataFrame {
        self.frame
    }

    /// The column as an R vector, converted from Arrow.
    pub fn column_r(&self, name: &str) -> Result<RVector> {
        let column = self
            .frame
            .column(name)
            .map_err(|_| Error::NoColumn(name.to_string()))?;
        convert::to_r(column.as_materialized_series())
    }

    /// The values of an `Int32` column without nulls stored in one chunk, which R can read in
    /// place because the layouts agree. This is what makes `INTEGER_RO` on a table column free.
    pub fn column_slice_i32(&self, name: &str) -> Option<&[i32]> {
        let chunked = self
            .frame
            .column(name)
            .ok()?
            .as_materialized_series()
            .i32()
            .ok()?;
        if chunked.null_count() > 0 {
            return None;
        }
        chunked.cont_slice().ok()
    }

    /// As [`Table::column_slice_i32`], for `Float64`. A `NaN` is fine, because R's `NaN` is the
    /// same value; only a null needs R's `NA_real_` bit pattern.
    pub fn column_slice_f64(&self, name: &str) -> Option<&[f64]> {
        let chunked = self
            .frame
            .column(name)
            .ok()?
            .as_materialized_series()
            .f64()
            .ok()?;
        if chunked.null_count() > 0 {
            return None;
        }
        chunked.cont_slice().ok()
    }

    /// An independent table with the same contents, for a copy-on-modify update of a shared
    /// table. Only the column list is copied; the buffers stay shared.
    pub fn fork(&self) -> Table {
        self.clone()
    }

    /// Adds or replaces a column. A key that includes it is dropped, because the table is no
    /// longer known to be sorted by it.
    pub fn set_column(&mut self, column: Column) -> Result<()> {
        let column = normalize(column)?;
        if column.len() != self.frame.height() && self.frame.width() > 0 {
            return Err(Error::Length {
                name: column.name().to_string(),
                expected: self.frame.height(),
                found: column.len(),
            });
        }
        self.forget_key(column.name().clone().as_str());
        self.frame.with_column(column)?;
        Ok(())
    }

    /// Moves a column out of the table, leaving an all-`NA` placeholder until
    /// [`put_column`](Table::put_column) brings it back. Nothing else then shares the column's
    /// buffers if nothing shared the table, so a kernel can write into them in place.
    pub(crate) fn take_column(&mut self, name: &str) -> Result<Column> {
        let height = self.frame.height();
        // SAFETY: the placeholder has the frame's height, so every column keeps it.
        let columns = unsafe { self.frame.columns_mut() };
        let slot = columns
            .iter_mut()
            .find(|column| column.name() == name)
            .ok_or_else(|| Error::NoColumn(name.to_string()))?;
        let placeholder = Column::full_null(slot.name().clone(), height, &DataType::Boolean);
        Ok(std::mem::replace(slot, placeholder))
    }

    /// Adds or replaces a column that a kernel built, which holds no valid `i32::MIN` and has an
    /// R dtype already, so it skips [`set_column`](Table::set_column)'s normalizing scan.
    pub(crate) fn put_column(&mut self, column: Column) -> Result<()> {
        self.forget_key(column.name().clone().as_str());
        self.frame.with_column(column.rechunk())?;
        Ok(())
    }

    pub fn drop_column(&mut self, name: &str) -> Result<()> {
        self.frame
            .drop_in_place(name)
            .map_err(|_| Error::NoColumn(name.to_string()))?;
        self.forget_key(name);
        Ok(())
    }

    /// Sorts the table by `columns` and records them as its key, as `setkey` does.
    pub fn set_key(&mut self, columns: &[&str]) -> Result<()> {
        let frame = std::mem::take(&mut self.frame);
        let mut sorted = frame.sort(
            columns
                .iter()
                .copied()
                .map(PlSmallStr::from)
                .collect::<Vec<_>>(),
            Default::default(),
        )?;
        sorted.rechunk_mut_par();
        self.frame = sorted;
        self.key = columns.iter().copied().map(PlSmallStr::from).collect();
        Ok(())
    }

    fn forget_key(&mut self, name: &str) {
        if self.key.iter().any(|column| column == name) {
            self.key.clear();
        }
    }

    /// Wraps a frame whose columns already hold R dtypes, such as one gathered from other tables'
    /// columns, without [`Table::new`]'s scan of every integer column for `i32::MIN`.
    pub(crate) fn from_normalized(mut frame: DataFrame) -> Table {
        frame.rechunk_mut_par();
        Table {
            frame,
            key: Vec::new(),
        }
    }

    pub(crate) fn with_key(mut self, key: Vec<PlSmallStr>) -> Table {
        self.key = key;
        self
    }
}

/// Converts a column to the dtype its R vector type uses, or refuses a dtype R has no type for,
/// and makes it one contiguous chunk. Every kernel reads a column as one slice, so a column left
/// in the many chunks a CSV reader produces would be copied again by every query that reads it.
fn normalize(column: Column) -> Result<Column> {
    Ok(convert(column)?.rechunk())
}

fn convert(column: Column) -> Result<Column> {
    let target = match column.dtype() {
        DataType::Int32 => {
            // R's `NA_integer_` is `i32::MIN`, so a valid `i32::MIN` must become a null, or R would
            // read it as `NA` while the column claims to have none.
            let values = column.as_materialized_series().i32()?;
            if values.min() != Some(i32::MIN) {
                return Ok(column);
            }
            let cleaned: Int32Chunked = values
                .iter()
                .map(|value| value.filter(|value| *value != i32::MIN))
                .collect();
            return Ok(cleaned.with_name(column.name().clone()).into_column());
        }
        DataType::Boolean | DataType::Float64 | DataType::String | DataType::Enum(..) => {
            return Ok(column);
        }
        DataType::Int8 | DataType::Int16 | DataType::UInt8 | DataType::UInt16 => DataType::Int32,
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 | DataType::Float32 => {
            DataType::Float64
        }
        DataType::Categorical(..) => {
            return Ok(convert::string_to_factor(column.as_materialized_series())?.into_column());
        }
        DataType::Null => {
            return Ok(
                Series::new(column.name().clone(), vec![None::<bool>; column.len()]).into_column(),
            );
        }
        other => {
            return Err(Error::UnsupportedType {
                name: column.name().to_string(),
                found: other.to_string(),
            });
        }
    };
    Ok(column.cast(&target)?)
}
