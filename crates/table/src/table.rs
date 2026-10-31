//! ry's built-in typed table, the value R code writes as `frame(...)` and the checker types as
//! `frame{id: integer[], price: double[]}`.
//!
//! A [`Table`] holds a Polars [`DataFrame`]. Its columns are restricted to the dtypes that have an
//! R vector type (see [`ColumnType`]), so every column can cross into R and back without a lossy
//! conversion. Queries lower from the small IR in [`query`] to a Polars lazy plan, so the planner
//! sees a whole `tbl[i, j, by]` at once and every aggregation runs as a Polars kernel.
//!
//! R code and native code read a column as an R vector. Producing one is a conversion whenever
//! the Arrow layout and R's layout differ (nulls, booleans, strings), so it happens lazily, per
//! column, and the result is kept as a [`Mirror`] beside the Arrow data until the column changes.
//! [`Residency`] names where a column's authoritative data currently lives.

pub mod convert;
pub mod ffi;
pub mod query;

#[cfg(feature = "runtime")]
pub mod altrep;

use convert::RVector;
use polars::prelude::{Column, DataFrame, DataType, IntoColumn, NamedFrom, PlSmallStr, Series};
use std::{fmt, sync::Arc};

pub use polars;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the frame has no column `{0}`")]
    NoColumn(String),
    #[error("column `{name}` has {found} rows, but the frame has {expected}")]
    Length { name: String, expected: usize, found: usize },
    #[error("column `{name}` is {found}, which has no R vector type")]
    UnsupportedType { name: String, found: String },
    #[error("column `{name}` is {found}, but the schema declares {expected}")]
    SchemaMismatch { name: String, expected: ColumnType, found: ColumnType },
    #[error("the schema declares {expected} columns, but the frame has {found}")]
    SchemaWidth { expected: usize, found: usize },
    #[error("cannot parse the frame type `{0}`")]
    SchemaSyntax(String),
    #[error("invalid query: {0}")]
    Query(String),
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
            columns: columns.into_iter().map(|(name, kind)| (name.into(), kind)).collect(),
        }
    }

    pub fn column(&self, name: &str) -> Option<ColumnType> {
        self.columns.iter().find(|(column, _)| column == name).map(|(_, kind)| *kind)
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
        for field in body.split(',').map(str::trim).filter(|field| !field.is_empty()) {
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
            return Err(Error::SchemaWidth { expected: self.columns.len(), found: actual.columns.len() });
        }
        for ((name, expected), (found_name, found)) in self.columns.iter().zip(&actual.columns) {
            if name != found_name {
                return Err(Error::NoColumn(name.to_string()));
            }
            if expected != found {
                return Err(Error::SchemaMismatch { name: name.to_string(), expected: *expected, found: *found });
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

/// Where a column's authoritative data lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Residency {
    /// Only in the Arrow buffers Polars owns.
    Arrow,
    /// In Arrow, with an R-layout copy kept for readers on the R side. The two agree.
    Shared,
    /// In the R-layout copy, because a writable pointer to it was handed out. The Arrow column is
    /// stale until the next [`Table::sync`].
    R,
}

/// The R-layout copy of one column. It is shared between a table and the tables forked from it
/// until either changes the column, which is why the vector sits behind an `Arc`.
#[derive(Clone, Debug)]
pub struct Mirror {
    name: PlSmallStr,
    vector: Arc<RVector>,
    writable: bool,
}

/// A table: a Polars frame of R-typed columns, its sort key, and the R-layout copies of the
/// columns R has read.
///
/// Updates follow R's copy-on-modify rule, driven by the reference count of the R object that
/// holds the table: an unshared table is updated in place through `&mut self`, and a shared one
/// is first [`fork`](Table::fork)ed, which copies only the column list, because the Arrow
/// buffers themselves are immutable and shared.
#[derive(Debug, Default)]
pub struct Table {
    frame: DataFrame,
    /// The columns the table is sorted by, as `setkey` leaves it. Keyed joins use it.
    key: Vec<PlSmallStr>,
    /// Sparse: only columns that have been read from the R side have one.
    mirrors: Vec<Mirror>,
}

impl Table {
    /// Builds a table from a frame, converting every column to its R dtype: narrower integers
    /// widen to `Int32`, wider ones and unsigned 32-bit ones become `Float64` (as R reads an
    /// `integer64` without the bit64 package), floats widen to `Float64`, and a categorical
    /// becomes an `Enum` so its level order is fixed.
    pub fn new(frame: DataFrame) -> Result<Table> {
        let mut columns = Vec::with_capacity(frame.width());
        for column in frame.into_columns() {
            columns.push(normalize(column)?);
        }
        Ok(Table { frame: DataFrame::new_infer_height(columns)?, key: Vec::new(), mirrors: Vec::new() })
    }

    /// Builds a table from R vectors, as `frame(id = 1:3, ...)` does. Vectors move into Arrow
    /// buffers without a copy unless they hold NAs, which need a validity bitmap.
    pub fn from_r(columns: Vec<(PlSmallStr, RVector)>) -> Result<Table> {
        let height = columns.first().map_or(0, |(_, vector)| vector.len());
        let mut converted = Vec::with_capacity(columns.len());
        for (name, vector) in columns {
            if vector.len() != height {
                return Err(Error::Length { name: name.to_string(), expected: height, found: vector.len() });
            }
            converted.push(convert::to_series(name, vector)?.into_column());
        }
        Ok(Table { frame: DataFrame::new(height, converted)?, key: Vec::new(), mirrors: Vec::new() })
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
                .map(|column| {
                    let kind = ColumnType::of(column.dtype())
                        .unwrap_or_else(|| unreachable!("Table::new normalizes every column"));
                    (column.name().clone(), kind)
                })
                .collect(),
        }
    }

    pub fn key(&self) -> &[PlSmallStr] {
        &self.key
    }

    /// The frame, with every column the R side may have written folded back in first.
    pub fn frame(&mut self) -> Result<&DataFrame> {
        self.sync()?;
        Ok(&self.frame)
    }

    /// The frame as it stands in Arrow, which is current only when no column is resident in R.
    pub fn frame_unsynced(&self) -> &DataFrame {
        &self.frame
    }

    pub fn into_frame(mut self) -> Result<DataFrame> {
        self.sync()?;
        Ok(self.frame)
    }

    pub fn residency(&self, name: &str) -> Result<Residency> {
        self.frame.column(name).map_err(|_| Error::NoColumn(name.to_string()))?;
        Ok(match self.mirrors.iter().find(|mirror| mirror.name == name) {
            None => Residency::Arrow,
            Some(mirror) if mirror.writable => Residency::R,
            Some(_) => Residency::Shared,
        })
    }

    /// The column as an R vector, for readers. The conversion runs at most once per version of
    /// the column, and integer and double columns without nulls could skip it entirely through
    /// [`Table::column_slice_i32`] and [`Table::column_slice_f64`].
    pub fn read_r(&mut self, name: &str) -> Result<Arc<RVector>> {
        if let Some(mirror) = self.mirrors.iter().find(|mirror| mirror.name == name) {
            return Ok(mirror.vector.clone());
        }
        let column = self.frame.column(name).map_err(|_| Error::NoColumn(name.to_string()))?;
        let vector = Arc::new(convert::to_r(column.as_materialized_series())?);
        self.mirrors.push(Mirror { name: name.into(), vector: vector.clone(), writable: false });
        Ok(vector)
    }

    /// The column as an R vector that the caller may write, as `INTEGER(x)` or `REAL(x)` allow
    /// native code to. From now on the R copy is authoritative, and the Arrow column is rebuilt
    /// from it on the next [`Table::sync`].
    pub fn write_r(&mut self, name: &str) -> Result<&mut RVector> {
        self.read_r(name)?;
        let mirror = self
            .mirrors
            .iter_mut()
            .find(|mirror| mirror.name == name)
            .unwrap_or_else(|| unreachable!("read_r just created the mirror"));
        mirror.writable = true;
        Ok(Arc::make_mut(&mut mirror.vector))
    }

    /// The values of an `Int32` column without nulls stored in one chunk, which R can read in
    /// place because the layouts agree. This is what makes `INTEGER_RO` on a table column free.
    pub fn column_slice_i32(&self, name: &str) -> Option<&[i32]> {
        let column = self.frame.column(name).ok()?;
        if self.mirrors.iter().any(|mirror| mirror.name == name && mirror.writable) {
            return None;
        }
        let chunked = column.as_materialized_series().i32().ok()?;
        if chunked.null_count() > 0 {
            return None;
        }
        chunked.cont_slice().ok()
    }

    /// As [`Table::column_slice_i32`], for `Float64`. A `NaN` is fine, because R's `NaN` is the
    /// same value; only a null needs R's `NA_real_` bit pattern.
    pub fn column_slice_f64(&self, name: &str) -> Option<&[f64]> {
        let column = self.frame.column(name).ok()?;
        if self.mirrors.iter().any(|mirror| mirror.name == name && mirror.writable) {
            return None;
        }
        let chunked = column.as_materialized_series().f64().ok()?;
        if chunked.null_count() > 0 {
            return None;
        }
        chunked.cont_slice().ok()
    }

    /// Folds every column the R side may have written back into Arrow. The R copy stays, and
    /// stays authoritative, because native code may still hold the pointer it was given.
    pub fn sync(&mut self) -> Result<()> {
        for mirror in self.mirrors.iter().filter(|mirror| mirror.writable) {
            let series = convert::to_series(mirror.name.clone(), (*mirror.vector).clone())?;
            self.frame.replace(&mirror.name, series.into_column())?;
        }
        Ok(())
    }

    /// An independent table with the same contents, for a copy-on-modify update of a shared
    /// table. Only the column list is copied; the buffers stay shared.
    pub fn fork(&mut self) -> Result<Table> {
        self.sync()?;
        Ok(Table {
            frame: self.frame.clone(),
            key: self.key.clone(),
            mirrors: self.mirrors.iter().filter(|mirror| !mirror.writable).cloned().collect(),
        })
    }

    /// Adds or replaces a column. Its R copy, if any, is dropped, and a key that includes it is
    /// dropped too, because the table is no longer known to be sorted by it.
    pub fn set_column(&mut self, column: Column) -> Result<()> {
        let column = normalize(column)?;
        if column.len() != self.frame.height() && self.frame.width() > 0 {
            return Err(Error::Length {
                name: column.name().to_string(),
                expected: self.frame.height(),
                found: column.len(),
            });
        }
        self.forget(column.name().clone().as_str());
        self.frame.with_column(column)?;
        Ok(())
    }

    pub fn drop_column(&mut self, name: &str) -> Result<()> {
        self.frame.drop_in_place(name).map_err(|_| Error::NoColumn(name.to_string()))?;
        self.forget(name);
        Ok(())
    }

    /// Sorts the table by `columns` and records them as its key, as `setkey` does.
    pub fn set_key(&mut self, columns: &[&str]) -> Result<()> {
        self.sync()?;
        let frame = std::mem::take(&mut self.frame);
        self.frame = frame.sort(columns.iter().copied().map(PlSmallStr::from).collect::<Vec<_>>(), Default::default())?;
        self.mirrors.clear();
        self.key = columns.iter().copied().map(PlSmallStr::from).collect();
        Ok(())
    }

    fn forget(&mut self, name: &str) {
        self.mirrors.retain(|mirror| mirror.name != name);
        if self.key.iter().any(|column| column == name) {
            self.key.clear();
        }
    }

    pub(crate) fn with_key(mut self, key: Vec<PlSmallStr>) -> Table {
        self.key = key;
        self
    }
}

/// Converts a column to the dtype its R vector type uses, or refuses a dtype R has no type for.
fn normalize(column: Column) -> Result<Column> {
    let target = match column.dtype() {
        DataType::Boolean | DataType::Int32 | DataType::Float64 | DataType::String | DataType::Enum(..) => {
            return Ok(column);
        }
        DataType::Int8 | DataType::Int16 | DataType::UInt8 | DataType::UInt16 => DataType::Int32,
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 | DataType::Float32 => DataType::Float64,
        DataType::Categorical(..) => {
            return Ok(convert::string_to_factor(column.as_materialized_series())?.into_column());
        }
        DataType::Null => {
            return Ok(Series::new(column.name().clone(), vec![None::<bool>; column.len()]).into_column());
        }
        other => {
            return Err(Error::UnsupportedType { name: column.name().to_string(), found: other.to_string() });
        }
    };
    Ok(column.cast(&target)?)
}
