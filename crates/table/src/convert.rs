//! Conversion between Arrow columns and R's vector layouts.
//!
//! The layouts differ in how they mark a missing value. Arrow keeps a separate validity bitmap,
//! while R reserves a value: `INT_MIN` for `NA_integer_` and `NA` (logical), and one specific
//! `NaN` bit pattern for `NA_real_`. Every other `NaN` is R's `NaN`, a value, which Arrow also
//! treats as a value. So integer and double columns convert by rewriting nulls into the reserved
//! value and back, and a column without nulls has the same bytes on both sides.

use {
    crate::{Error, Result},
    polars::prelude::{
        BooleanChunked, ChunkUnique, DataType, Float64Chunked, FrozenCategories, Int32Chunked,
        IntoSeries, NewChunkedArray, PlSmallStr, Series, StringChunked,
    },
    polars_arrow::{array::PrimitiveArray, bitmap::Bitmap, datatypes::ArrowDataType},
    polars_buffer::{Buffer, SharedStorage},
};

/// R's `NA_integer_`, which is also `NA` for a logical vector.
pub const NA_INTEGER: i32 = i32::MIN;

/// R's `NA_real_`: a quiet `NaN` whose low word is 1954.
pub const NA_REAL: f64 = f64::from_bits(0x7FF0_0000_0000_07A2);

/// Whether `value` is R's `NA_real_`, as opposed to an ordinary `NaN`. This is `R_IsNA`: R only
/// looks at the low word, because arithmetic may set the quiet bit of the pattern.
pub fn is_na_real(value: f64) -> bool {
    value.is_nan() && (value.to_bits() & 0xFFFF_FFFF) == 1954
}

/// An R vector in R's own layout, owned by Rust. It is what a column becomes when the R side
/// reads it, and what `frame(...)` receives.
#[derive(Clone, PartialEq, Debug)]
pub enum RVector {
    /// `TRUE` is 1, `FALSE` is 0, and `NA` is [`NA_INTEGER`].
    Logical(Vec<i32>),
    Integer(Vec<i32>),
    Double(Vec<f64>),
    Character(Vec<Option<String>>),
    /// One-based codes into `levels`, with [`NA_INTEGER`] for `NA`, which is R's layout.
    Factor {
        codes: Vec<i32>,
        levels: Vec<String>,
    },
}

impl RVector {
    pub fn len(&self) -> usize {
        match self {
            RVector::Logical(values) | RVector::Integer(values) => values.len(),
            RVector::Double(values) => values.len(),
            RVector::Character(values) => values.len(),
            RVector::Factor { codes, .. } => codes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Builds the Arrow column for an R vector. Integer and double data move into the Arrow buffer
/// without a copy, and only a vector holding NAs pays for a pass that builds the validity bitmap.
pub fn to_series(name: PlSmallStr, vector: RVector) -> Result<Series> {
    Ok(match vector {
        RVector::Integer(values) => {
            let validity = validity(&values, |value| *value != NA_INTEGER);
            Int32Chunked::from_vec_validity(name, values, validity).into_series()
        }
        RVector::Double(values) => {
            let validity = validity(&values, |value| !is_na_real(*value));
            Float64Chunked::from_vec_validity(name, values, validity).into_series()
        }
        RVector::Logical(values) => BooleanChunked::from_iter_options(
            name,
            values
                .iter()
                .map(|value| (*value != NA_INTEGER).then_some(*value != 0)),
        )
        .into_series(),
        RVector::Character(values) => {
            StringChunked::from_iter_options(name, values.iter().map(|value| value.as_deref()))
                .into_series()
        }
        RVector::Factor { codes, levels } => {
            let dtype = enum_dtype(&levels)?;
            let mut strings = Vec::with_capacity(codes.len());
            for code in &codes {
                strings.push(match *code {
                    NA_INTEGER => None,
                    code => Some(
                        levels
                            .get((code - 1) as usize)
                            .ok_or_else(|| {
                                Error::Query(format!("factor code {code} has no level"))
                            })?
                            .as_str(),
                    ),
                });
            }
            StringChunked::from_iter_options(name, strings.into_iter())
                .into_series()
                .cast(&dtype)?
        }
    })
}

/// Builds an integer column over memory the caller owns, such as an R vector's data, without
/// copying it. Only the validity bitmap is allocated, and only when the values hold an NA.
///
/// # Safety
/// `values` must stay valid and unmodified for as long as the series, or any column derived from
/// it, is alive.
pub unsafe fn borrow_integer(name: PlSmallStr, values: &[i32]) -> Series {
    let validity = validity(values, |value| *value != NA_INTEGER);
    let buffer = unsafe { Buffer::from_storage(SharedStorage::from_slice_with_owner(values, ())) };
    Int32Chunked::with_chunk(
        name,
        PrimitiveArray::new(ArrowDataType::Int32, buffer, validity),
    )
    .into_series()
}

/// [`borrow_integer`] for a double column.
///
/// # Safety
/// As for [`borrow_integer`].
pub unsafe fn borrow_double(name: PlSmallStr, values: &[f64]) -> Series {
    let validity = validity(values, |value| !is_na_real(*value));
    let buffer = unsafe { Buffer::from_storage(SharedStorage::from_slice_with_owner(values, ())) };
    Float64Chunked::with_chunk(
        name,
        PrimitiveArray::new(ArrowDataType::Float64, buffer, validity),
    )
    .into_series()
}

/// Converts a column to R's layout. The column's dtype must be one [`crate::ColumnType`] names.
pub fn to_r(series: &Series) -> Result<RVector> {
    Ok(match series.dtype() {
        DataType::Int32 => {
            let chunked = series.i32()?;
            let mut values = Vec::with_capacity(chunked.len());
            for array in chunked.downcast_iter() {
                match array.validity() {
                    None => values.extend_from_slice(array.values()),
                    Some(valid) => values.extend(
                        array
                            .values()
                            .iter()
                            .zip(valid.iter())
                            .map(|(value, ok)| if ok { *value } else { NA_INTEGER }),
                    ),
                }
            }
            RVector::Integer(values)
        }
        DataType::Float64 => {
            let chunked = series.f64()?;
            let mut values = Vec::with_capacity(chunked.len());
            for array in chunked.downcast_iter() {
                match array.validity() {
                    None => values.extend_from_slice(array.values()),
                    Some(valid) => values.extend(
                        array
                            .values()
                            .iter()
                            .zip(valid.iter())
                            .map(|(value, ok)| if ok { *value } else { NA_REAL }),
                    ),
                }
            }
            RVector::Double(values)
        }
        DataType::Boolean => RVector::Logical(
            series
                .bool()?
                .iter()
                .map(|value| value.map_or(NA_INTEGER, i32::from))
                .collect(),
        ),
        DataType::String => RVector::Character(
            series
                .str()?
                .iter()
                .map(|value| value.map(str::to_string))
                .collect(),
        ),
        dtype @ DataType::Enum(..) => {
            let levels = factor_levels(dtype).unwrap_or_default();
            let physical = series.to_physical_repr().cast(&DataType::Int32)?;
            let codes = physical
                .i32()?
                .iter()
                .map(|code| code.map_or(NA_INTEGER, |code| code + 1))
                .collect();
            RVector::Factor { codes, levels }
        }
        other => {
            return Err(Error::UnsupportedType {
                name: series.name().to_string(),
                found: other.to_string(),
            });
        }
    })
}

/// A factor column's levels in order, read from its `Enum` dtype.
pub fn factor_levels(dtype: &DataType) -> Option<Vec<String>> {
    match dtype {
        DataType::Enum(categories, _) => Some(
            categories
                .categories()
                .values_iter()
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

/// The `Enum` dtype whose categories are `levels`, in order, which is how a factor's level order
/// survives in Arrow.
pub fn enum_dtype(levels: &[String]) -> Result<DataType> {
    Ok(DataType::from_frozen_categories(FrozenCategories::new(
        levels.iter().map(String::as_str),
    )?))
}

/// A string column as a factor with R's default levels, its distinct values sorted, as
/// `factor(x)` makes them.
pub fn string_to_factor(series: &Series) -> Result<Series> {
    let strings = series.cast(&DataType::String)?;
    let mut levels: Vec<String> = strings
        .str()?
        .unique()?
        .iter()
        .flatten()
        .map(str::to_string)
        .collect();
    levels.sort_unstable();
    Ok(strings.cast(&enum_dtype(&levels)?)?)
}

fn validity<T>(values: &[T], valid: impl Fn(&T) -> bool) -> Option<Bitmap> {
    if values.iter().all(&valid) {
        return None;
    }
    Some(values.iter().map(valid).collect())
}
