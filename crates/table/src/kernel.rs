//! Kernels that evaluate row-wise query expressions directly over a column's Arrow buffers.
//!
//! Polars' lazy engine is general, but a row-wise `:=` pays for planning, for widening integers to
//! 64 bits to detect overflow, and for rebuilding a whole column when only a subset of its rows
//! changes. These kernels compute R's semantics in one pass per operation on Polars' thread pool,
//! and a filtered update writes into the column's buffer in place when nothing else shares it.
//! The grouping and join kernels evaluate their arguments and their post-aggregation arithmetic
//! here too.
//!
//! An expression these kernels cannot evaluate makes them return `None`, and the caller falls
//! back to the Polars plan, so both paths must agree on every result.

use std::mem::MaybeUninit;

use polars::prelude::{
    BooleanChunked, BooleanType, ChunkedArray, Column, DataFrame, DataType, Float64Chunked,
    Float64Type, Int32Chunked, Int32Type, IntoColumn, PlSmallStr,
};
use polars_arrow::array::{BooleanArray, PrimitiveArray};
use polars_arrow::bitmap::{Bitmap, MutableBitmap};
use polars_arrow::datatypes::ArrowDataType;
use polars_buffer::Buffer;
use polars_core::runtime::THREAD_POOL;
use rayon::prelude::*;

use crate::convert::factor_levels;
use crate::query::{BinaryOp, Expr, Typer, UnaryOp, comparison};
use crate::{ColumnType, Error, Result, Schema};

/// Below this many rows a kernel runs on the calling thread, because handing work to the pool
/// costs more than it saves.
pub(crate) const PARALLEL_ROWS: usize = 1 << 16;
/// The rows one parallel task covers. A multiple of 64, so a task's bits fill whole words.
const CHUNK_ROWS: usize = 1 << 16;

/// A column of logical, integer or double values, with Arrow's validity bitmap marking `NA`.
/// The value behind an `NA` is unspecified.
#[derive(Clone, Debug)]
pub(crate) enum Vector {
    Logical {
        values: Bitmap,
        validity: Option<Bitmap>,
    },
    Integer {
        values: Buffer<i32>,
        validity: Option<Bitmap>,
    },
    Double {
        values: Buffer<f64>,
        validity: Option<Bitmap>,
    },
}

/// A literal, or the `NA` of a type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scalar {
    Logical(Option<bool>),
    Integer(Option<i32>),
    Double(Option<f64>),
}

/// What evaluating an expression gives: a literal stays a scalar until it meets a column, so
/// `v1 + 1L` never materializes a column of ones.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Scalar(Scalar),
    Vector(Vector),
}

impl Vector {
    pub(crate) fn validity(&self) -> Option<&Bitmap> {
        match self {
            Vector::Logical { validity, .. }
            | Vector::Integer { validity, .. }
            | Vector::Double { validity, .. } => validity.as_ref(),
        }
    }

    pub(crate) fn kind(&self) -> ColumnType {
        match self {
            Vector::Logical { .. } => ColumnType::Logical,
            Vector::Integer { .. } => ColumnType::Integer,
            Vector::Double { .. } => ColumnType::Double,
        }
    }

    /// A logical, integer or double column's buffers, shared rather than copied. A column of
    /// another type gives `None`.
    pub(crate) fn of(column: &Column) -> Option<Vector> {
        let series = column.as_materialized_series();
        Some(match series.dtype() {
            DataType::Boolean => {
                let chunked = series.bool().ok()?.rechunk();
                match chunked.downcast_iter().next() {
                    Some(array) => Vector::Logical {
                        values: array.values().clone(),
                        validity: with_nulls(array.validity()),
                    },
                    None => Vector::Logical {
                        values: Bitmap::new(),
                        validity: None,
                    },
                }
            }
            DataType::Int32 => {
                let chunked = series.i32().ok()?.rechunk();
                match chunked.downcast_iter().next() {
                    Some(array) => Vector::Integer {
                        values: array.values().clone(),
                        validity: with_nulls(array.validity()),
                    },
                    None => Vector::Integer {
                        values: Buffer::from(Vec::new()),
                        validity: None,
                    },
                }
            }
            DataType::Float64 => {
                let chunked = series.f64().ok()?.rechunk();
                match chunked.downcast_iter().next() {
                    Some(array) => Vector::Double {
                        values: array.values().clone(),
                        validity: with_nulls(array.validity()),
                    },
                    None => Vector::Double {
                        values: Buffer::from(Vec::new()),
                        validity: None,
                    },
                }
            }
            _ => return None,
        })
    }

    /// The vector as a column. It holds no valid `i32::MIN`, so it needs no normalizing.
    pub(crate) fn into_column(self, name: PlSmallStr) -> Column {
        match self {
            Vector::Logical { values, validity } => BooleanChunked::with_chunk(
                name,
                BooleanArray::new(ArrowDataType::Boolean, values, validity),
            )
            .into_column(),
            Vector::Integer { values, validity } => Int32Chunked::with_chunk(
                name,
                PrimitiveArray::new(ArrowDataType::Int32, values, validity),
            )
            .into_column(),
            Vector::Double { values, validity } => Float64Chunked::with_chunk(
                name,
                PrimitiveArray::new(ArrowDataType::Float64, values, validity),
            )
            .into_column(),
        }
    }

    /// The values at `rows`, in that order.
    pub(crate) fn gather(&self, rows: &[u32]) -> Vector {
        let validity = self.validity().map(|validity| gather_bits(validity, rows));
        match self {
            Vector::Logical { values, .. } => Vector::Logical {
                values: gather_bits(values, rows),
                validity,
            },
            Vector::Integer { values, .. } => Vector::Integer {
                values: gather_values(values, rows),
                validity,
            },
            Vector::Double { values, .. } => Vector::Double {
                values: gather_values(values, rows),
                validity,
            },
        }
    }

    pub(crate) fn null(kind: ColumnType, len: usize) -> Option<Vector> {
        let validity = Some(Bitmap::new_zeroed(len));
        Some(match kind {
            ColumnType::Logical => Vector::Logical {
                values: Bitmap::new_zeroed(len),
                validity,
            },
            ColumnType::Integer => Vector::Integer {
                values: Buffer::from(vec![0; len]),
                validity,
            },
            ColumnType::Double => Vector::Double {
                values: Buffer::from(vec![0.0; len]),
                validity,
            },
            ColumnType::Character | ColumnType::Factor => return None,
        })
    }
}

impl Scalar {
    fn kind(self) -> ColumnType {
        match self {
            Scalar::Logical(_) => ColumnType::Logical,
            Scalar::Integer(_) => ColumnType::Integer,
            Scalar::Double(_) => ColumnType::Double,
        }
    }

    fn is_na(self) -> bool {
        matches!(
            self,
            Scalar::Logical(None) | Scalar::Integer(None) | Scalar::Double(None)
        )
    }
}

impl Value {
    pub(crate) fn kind(&self) -> ColumnType {
        match self {
            Value::Scalar(scalar) => scalar.kind(),
            Value::Vector(vector) => vector.kind(),
        }
    }

    /// The value as a vector of `len` rows, repeating a scalar.
    pub(crate) fn into_vector(self, len: usize) -> Option<Vector> {
        match self {
            Value::Vector(vector) => Some(vector),
            Value::Scalar(scalar) if scalar.is_na() => Vector::null(scalar.kind(), len),
            Value::Scalar(Scalar::Logical(value)) => Some(Vector::Logical {
                values: Bitmap::new_with_value(value.unwrap_or(false), len),
                validity: None,
            }),
            Value::Scalar(Scalar::Integer(value)) => Some(Vector::Integer {
                values: Buffer::from(vec![value.unwrap_or(0); len]),
                validity: None,
            }),
            Value::Scalar(Scalar::Double(value)) => Some(Vector::Double {
                values: Buffer::from(vec![value.unwrap_or(0.0); len]),
                validity: None,
            }),
        }
    }

    /// The value converted to `kind`, as R coerces it: a double truncates toward zero into an
    /// integer and is `NA` when it does not fit, as Polars' cast does on the other path. Only the
    /// conversions both paths agree on are offered; any other gives `None`.
    pub(crate) fn cast(self, kind: ColumnType) -> Option<Value> {
        if self.kind() == kind {
            return Some(self);
        }
        match kind {
            ColumnType::Integer => Some(integers(self)?.into_value()),
            ColumnType::Double => Some(doubles(self)?.into_value()),
            _ => None,
        }
    }
}

/// A validity bitmap worth keeping: one without nulls is dropped, so kernels take their fast path.
fn with_nulls(validity: Option<&Bitmap>) -> Option<Bitmap> {
    validity
        .filter(|validity| validity.unset_bits() > 0)
        .cloned()
}

/// Evaluates row-wise expressions over a frame, or over a subset of its rows.
pub(crate) struct Evaluator<'a> {
    pub(crate) frame: &'a DataFrame,
    pub(crate) schema: &'a Schema,
    /// Evaluate at these rows only, in this order. `None` means every row.
    pub(crate) rows: Option<&'a [u32]>,
}

impl Evaluator<'_> {
    pub(crate) fn len(&self) -> usize {
        self.rows.map_or(self.frame.height(), <[u32]>::len)
    }

    fn typer(&self) -> Typer<'_> {
        Typer {
            schema: self.schema,
        }
    }

    fn column(&self, name: &str) -> Option<Vector> {
        let vector = Vector::of(self.frame.column(name).ok()?)?;
        Some(match self.rows {
            Some(rows) => vector.gather(rows),
            None => vector,
        })
    }

    /// The expression's value, or `None` when it is not row-wise or uses an operation the
    /// kernels do not implement.
    pub(crate) fn eval(&self, expr: &Expr) -> Option<Value> {
        let len = self.len();
        Some(match expr {
            Expr::Column(name) => Value::Vector(self.column(name)?),
            // `i32::MIN` is R's `NA_integer_`.
            Expr::Integer(value) => Value::Scalar(Scalar::Integer(
                Some(*value).filter(|value| *value != i32::MIN),
            )),
            Expr::Double(value) => Value::Scalar(Scalar::Double(Some(*value))),
            Expr::Logical(value) => Value::Scalar(Scalar::Logical(Some(*value))),
            Expr::Na(ColumnType::Logical) => Value::Scalar(Scalar::Logical(None)),
            Expr::Na(ColumnType::Integer) => Value::Scalar(Scalar::Integer(None)),
            Expr::Na(ColumnType::Double) => Value::Scalar(Scalar::Double(None)),
            Expr::Na(_) | Expr::Character(_) => return None,
            Expr::Unary(UnaryOp::Not, arg) => not(logicals(self.eval(arg)?, len)?),
            Expr::Unary(UnaryOp::IsNa, arg) => is_na(self.eval(arg)?, len),
            Expr::Unary(UnaryOp::Negate, arg) => {
                let kind = self.typer().expr(expr).ok()?;
                match kind {
                    ColumnType::Integer => {
                        let operand = integers(self.eval(arg)?)?;
                        unary_map(operand, len, |value: i32| value.wrapping_neg())
                    }
                    ColumnType::Double => {
                        let operand = doubles(self.eval(arg)?)?;
                        unary_map(operand, len, |value: f64| -value)
                    }
                    _ => return None,
                }
            }
            Expr::Binary(op, left, right) => self.binary(*op, left, right, expr)?,
            Expr::IfElse(test, yes, no) => {
                let kind = self.typer().expr(expr).ok()?;
                let test = logicals(self.eval(test)?, len)?;
                let yes = self.eval(yes)?.cast(kind)?.into_vector(len)?;
                let no = self.eval(no)?.cast(kind)?.into_vector(len)?;
                if_else(test, yes, no, len)?
            }
            Expr::Aggregate { .. } | Expr::Count | Expr::Head(..) | Expr::Correlation(..) => {
                return None;
            }
        })
    }

    fn binary(&self, op: BinaryOp, left: &Expr, right: &Expr, expr: &Expr) -> Option<Value> {
        let len = self.len();
        let typer = self.typer();
        let kind = typer.expr(expr).ok()?;
        let (left_kind, right_kind) = (typer.expr(left).ok()?, typer.expr(right).ok()?);
        match op {
            BinaryOp::And | BinaryOp::Or => {
                let left = logicals(self.eval(left)?, len)?;
                let right = logicals(self.eval(right)?, len)?;
                Some(kleene(op, left, right))
            }
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => {
                match comparison(left_kind, right_kind).ok()? {
                    ColumnType::Character => self.compare_factor(op, left, right),
                    ColumnType::Double => {
                        let (l, r) = (doubles(self.eval(left)?)?, doubles(self.eval(right)?)?);
                        compare(op, l, r, len)
                    }
                    // Logicals compare as the integers they coerce to.
                    _ => {
                        let (l, r) = (integers(self.eval(left)?)?, integers(self.eval(right)?)?);
                        compare(op, l, r, len)
                    }
                }
            }
            _ if kind == ColumnType::Integer => {
                let (l, r) = (integers(self.eval(left)?)?, integers(self.eval(right)?)?);
                integer_arithmetic(op, l, r, len)
            }
            _ => {
                let (l, r) = (doubles(self.eval(left)?)?, doubles(self.eval(right)?)?);
                double_arithmetic(op, l, r, len)
            }
        }
    }

    /// `f == "label"` and `f != "label"` for a factor column, compared on the factor's codes.
    fn compare_factor(&self, op: BinaryOp, left: &Expr, right: &Expr) -> Option<Value> {
        let (name, label) = match (left, right) {
            (Expr::Column(name), Expr::Character(label))
            | (Expr::Character(label), Expr::Column(name)) => (name, label),
            _ => return None,
        };
        if !matches!(op, BinaryOp::Equal | BinaryOp::NotEqual)
            || self.schema.column(name) != Some(ColumnType::Factor)
        {
            return None;
        }
        let column = self.frame.column(name).ok()?;
        let levels = factor_levels(column.dtype())?;
        let codes = Codes::of(column)?;
        let codes = match self.rows {
            Some(rows) => codes.gather(rows),
            None => codes,
        };
        let len = self.len();
        let equal = op == BinaryOp::Equal;
        let values = match levels.iter().position(|level| level == label) {
            // A label the factor lacks equals no row.
            None => Bitmap::new_with_value(!equal, len),
            Some(code) => {
                let code = u32::try_from(code).ok()?;
                codes.pack(len, move |value| (value == code) == equal)
            }
        };
        Some(Value::Vector(Vector::Logical {
            values,
            validity: codes.validity,
        }))
    }
}

/// A factor column's codes (zero-based level indices) and validity. Polars stores them in the
/// narrowest unsigned type that holds every level.
#[derive(Clone)]
pub(crate) struct Codes {
    pub(crate) values: CodeValues,
    pub(crate) validity: Option<Bitmap>,
}

#[derive(Clone)]
pub(crate) enum CodeValues {
    U8(Buffer<u8>),
    U16(Buffer<u16>),
    U32(Buffer<u32>),
}

impl Codes {
    pub(crate) fn of(column: &Column) -> Option<Codes> {
        let physical = column.as_materialized_series().to_physical_repr();
        macro_rules! codes {
            ($accessor:ident, $variant:ident) => {{
                let chunked = physical.$accessor().ok()?.rechunk();
                match chunked.downcast_iter().next() {
                    Some(array) => Codes {
                        values: CodeValues::$variant(array.values().clone()),
                        validity: with_nulls(array.validity()),
                    },
                    None => Codes {
                        values: CodeValues::$variant(Buffer::from(Vec::new())),
                        validity: None,
                    },
                }
            }};
        }
        Some(match physical.dtype() {
            DataType::UInt8 => codes!(u8, U8),
            DataType::UInt16 => codes!(u16, U16),
            DataType::UInt32 => codes!(u32, U32),
            _ => return None,
        })
    }

    fn gather(&self, rows: &[u32]) -> Codes {
        Codes {
            values: match &self.values {
                CodeValues::U8(values) => CodeValues::U8(gather_values(values, rows)),
                CodeValues::U16(values) => CodeValues::U16(gather_values(values, rows)),
                CodeValues::U32(values) => CodeValues::U32(gather_values(values, rows)),
            },
            validity: self
                .validity
                .as_ref()
                .map(|validity| gather_bits(validity, rows)),
        }
    }

    fn pack(&self, len: usize, test: impl Fn(u32) -> bool + Sync) -> Bitmap {
        match &self.values {
            CodeValues::U8(values) => pack_map(len, values, |value| test(u32::from(value))),
            CodeValues::U16(values) => pack_map(len, values, |value| test(u32::from(value))),
            CodeValues::U32(values) => pack_map(len, values, test),
        }
    }
}

/// One side of an elementwise operation, converted to the operation's type.
struct Operand<T> {
    side: Side<T>,
    validity: Option<Bitmap>,
}

enum Side<T> {
    Array(Buffer<T>),
    /// A literal, or `None` for its `NA`.
    Scalar(Option<T>),
}

impl<T: Copy + Default> Side<T> {
    fn get(&self, row: usize) -> T {
        match self {
            Side::Array(values) => values.get(row).copied().unwrap_or_default(),
            Side::Scalar(value) => value.unwrap_or_default(),
        }
    }
}

impl Operand<i32> {
    fn into_value(self) -> Value {
        match self.side {
            Side::Array(values) => Value::Vector(Vector::Integer {
                values,
                validity: self.validity,
            }),
            Side::Scalar(value) => Value::Scalar(Scalar::Integer(value)),
        }
    }
}

impl Operand<f64> {
    fn into_value(self) -> Value {
        match self.side {
            Side::Array(values) => Value::Vector(Vector::Double {
                values,
                validity: self.validity,
            }),
            Side::Scalar(value) => Value::Scalar(Scalar::Double(value)),
        }
    }
}

impl<T> Operand<T> {
    fn is_na_scalar(&self) -> bool {
        matches!(self.side, Side::Scalar(None))
    }
}

/// A logical or integer value as integers. A double is truncated toward zero, and one that is
/// `NaN` or out of range becomes `NA`.
fn integers(value: Value) -> Option<Operand<i32>> {
    Some(match value {
        Value::Scalar(Scalar::Logical(value)) => Operand {
            side: Side::Scalar(value.map(i32::from)),
            validity: None,
        },
        Value::Scalar(Scalar::Integer(value)) => Operand {
            side: Side::Scalar(value),
            validity: None,
        },
        Value::Scalar(Scalar::Double(value)) => Operand {
            side: Side::Scalar(value.and_then(truncate)),
            validity: None,
        },
        Value::Vector(Vector::Logical { values, validity }) => Operand {
            side: Side::Array(Buffer::from(
                values.iter().map(i32::from).collect::<Vec<_>>(),
            )),
            validity,
        },
        Value::Vector(Vector::Integer { values, validity }) => Operand {
            side: Side::Array(values),
            validity,
        },
        Value::Vector(Vector::Double { values, validity }) => {
            let truncated = map_values(&values, |value| truncate(value).unwrap_or(i32::MIN));
            let validity = mark_na(&truncated, validity);
            Operand {
                side: Side::Array(Buffer::from(truncated)),
                validity,
            }
        }
    })
}

fn truncate(value: f64) -> Option<i32> {
    let truncated = value.trunc();
    (truncated > f64::from(i32::MIN) && truncated <= f64::from(i32::MAX))
        .then_some(truncated as i32)
}

fn doubles(value: Value) -> Option<Operand<f64>> {
    Some(match value {
        Value::Scalar(Scalar::Logical(value)) => Operand {
            side: Side::Scalar(value.map(|value| f64::from(u8::from(value)))),
            validity: None,
        },
        Value::Scalar(Scalar::Integer(value)) => Operand {
            side: Side::Scalar(value.map(f64::from)),
            validity: None,
        },
        Value::Scalar(Scalar::Double(value)) => Operand {
            side: Side::Scalar(value),
            validity: None,
        },
        Value::Vector(Vector::Logical { values, validity }) => Operand {
            side: Side::Array(Buffer::from(
                values
                    .iter()
                    .map(|value| f64::from(u8::from(value)))
                    .collect::<Vec<_>>(),
            )),
            validity,
        },
        Value::Vector(Vector::Integer { values, validity }) => Operand {
            side: Side::Array(Buffer::from(map_values(&values, f64::from))),
            validity,
        },
        Value::Vector(Vector::Double { values, validity }) => Operand {
            side: Side::Array(values),
            validity,
        },
    })
}

/// A logical operand as bits: a number is `TRUE` unless it is zero, and `NaN` is `NA`.
struct Logicals {
    values: Bitmap,
    validity: Option<Bitmap>,
}

fn logicals(value: Value, len: usize) -> Option<Logicals> {
    let vector = match value {
        Value::Scalar(Scalar::Double(Some(value))) if value.is_nan() => {
            Vector::null(ColumnType::Logical, len)?
        }
        Value::Scalar(Scalar::Double(value)) => {
            Value::Scalar(Scalar::Logical(value.map(|value| value != 0.0))).into_vector(len)?
        }
        Value::Scalar(Scalar::Integer(value)) => {
            Value::Scalar(Scalar::Logical(value.map(|value| value != 0))).into_vector(len)?
        }
        other => other.into_vector(len)?,
    };
    Some(match vector {
        Vector::Logical { values, validity } => Logicals { values, validity },
        Vector::Integer { values, validity } => Logicals {
            values: pack_map(len, &values, |value| value != 0),
            validity,
        },
        Vector::Double { values, validity } => {
            let nan = pack_map(len, &values, f64::is_nan);
            let validity = if nan.set_bits() > 0 {
                and(validity.as_ref(), Some(&!&nan))
            } else {
                validity
            };
            Logicals {
                values: pack_map(len, &values, |value| value != 0.0),
                validity,
            }
        }
    })
}

fn not(operand: Logicals) -> Value {
    Value::Vector(Vector::Logical {
        values: !&operand.values,
        validity: operand.validity,
    })
}

/// R's `is.na`, which is also true for `NaN`.
fn is_na(value: Value, len: usize) -> Value {
    match value {
        Value::Scalar(Scalar::Double(Some(value))) => {
            Value::Scalar(Scalar::Logical(Some(value.is_nan())))
        }
        Value::Scalar(scalar) => Value::Scalar(Scalar::Logical(Some(scalar.is_na()))),
        Value::Vector(vector) => {
            let missing = match vector.validity() {
                Some(validity) => !validity,
                None => Bitmap::new_zeroed(len),
            };
            let values = match &vector {
                Vector::Double { values, .. } => &missing | &pack_map(len, values, f64::is_nan),
                _ => missing,
            };
            Value::Vector(Vector::Logical {
                values,
                validity: None,
            })
        }
    }
}

/// `&` and `|` with R's three-valued logic: `NA & FALSE` is `FALSE` and `NA | TRUE` is `TRUE`.
fn kleene(op: BinaryOp, left: Logicals, right: Logicals) -> Value {
    let and_op = op == BinaryOp::And;
    let values = if and_op {
        &left.values & &right.values
    } else {
        &left.values | &right.values
    };
    let validity = match (&left.validity, &right.validity) {
        (None, None) => None,
        (left_validity, right_validity) => {
            let all = Bitmap::new_with_value(true, values.len());
            let lv = left_validity.as_ref().unwrap_or(&all);
            let rv = right_validity.as_ref().unwrap_or(&all);
            // A side that is known and decides the result makes the result known.
            let (left_decides, right_decides) = if and_op {
                (lv & &!&left.values, rv & &!&right.values)
            } else {
                (lv & &left.values, rv & &right.values)
            };
            Some(&(&(lv & rv) | &left_decides) | &right_decides)
        }
    };
    Value::Vector(Vector::Logical { values, validity })
}

fn compare<T>(op: BinaryOp, left: Operand<T>, right: Operand<T>, len: usize) -> Option<Value>
where
    T: Copy + PartialOrd + Send + Sync + Default + IsNan,
{
    if left.is_na_scalar() || right.is_na_scalar() {
        return Some(Value::Vector(Vector::null(ColumnType::Logical, len)?));
    }
    let values = match op {
        BinaryOp::Equal => pack_zip(len, &left.side, &right.side, |l, r| l == r),
        BinaryOp::NotEqual => pack_zip(len, &left.side, &right.side, |l, r| l != r),
        BinaryOp::Less => pack_zip(len, &left.side, &right.side, |l, r| l < r),
        BinaryOp::LessEqual => pack_zip(len, &left.side, &right.side, |l, r| l <= r),
        BinaryOp::Greater => pack_zip(len, &left.side, &right.side, |l, r| l > r),
        _ => pack_zip(len, &left.side, &right.side, |l, r| l >= r),
    };
    let mut validity = and(left.validity.as_ref(), right.validity.as_ref());
    // Any comparison with `NaN` is `NA` in R.
    if T::CAN_BE_NAN && (side_has_nan(&left.side) || side_has_nan(&right.side)) {
        let nan = pack_zip(len, &left.side, &right.side, |l, r| {
            l.is_nan() || r.is_nan()
        });
        validity = and(validity.as_ref(), Some(&!&nan));
    }
    Some(Value::Vector(Vector::Logical { values, validity }))
}

pub(crate) trait IsNan {
    const CAN_BE_NAN: bool;
    fn is_nan(self) -> bool;
}

impl IsNan for i32 {
    const CAN_BE_NAN: bool = false;
    fn is_nan(self) -> bool {
        false
    }
}

impl IsNan for f64 {
    const CAN_BE_NAN: bool = true;
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }
}

fn side_has_nan<T: Copy + IsNan + Sync>(side: &Side<T>) -> bool {
    match side {
        Side::Array(values) => any(values, |value| value.is_nan()),
        Side::Scalar(value) => value.is_some_and(IsNan::is_nan),
    }
}

/// R's integer arithmetic: a result outside the integer range is `NA`, as is a division or a
/// remainder by zero.
fn integer_arithmetic(
    op: BinaryOp,
    left: Operand<i32>,
    right: Operand<i32>,
    len: usize,
) -> Option<Value> {
    fn checked(value: i64) -> i32 {
        if value > i64::from(i32::MAX) || value < -i64::from(i32::MAX) {
            i32::MIN
        } else {
            value as i32
        }
    }
    let scalar = matches!(
        (&left.side, &right.side),
        (Side::Scalar(_), Side::Scalar(_))
    );
    if left.is_na_scalar() || right.is_na_scalar() {
        return Some(if scalar {
            Value::Scalar(Scalar::Integer(None))
        } else {
            Value::Vector(Vector::null(ColumnType::Integer, len)?)
        });
    }
    let apply = |l: i32, r: i32| match op {
        BinaryOp::Add => checked(i64::from(l) + i64::from(r)),
        BinaryOp::Subtract => checked(i64::from(l) - i64::from(r)),
        BinaryOp::Multiply => checked(i64::from(l) * i64::from(r)),
        BinaryOp::IntegerDivide if r == 0 => i32::MIN,
        BinaryOp::IntegerDivide => {
            let quotient = l.wrapping_div(r);
            if l.wrapping_rem(r) != 0 && ((l < 0) != (r < 0)) {
                quotient - 1
            } else {
                quotient
            }
        }
        BinaryOp::Modulo if r == 0 => i32::MIN,
        BinaryOp::Modulo => {
            let remainder = l.wrapping_rem(r);
            if remainder != 0 && ((remainder < 0) != (r < 0)) {
                remainder + r
            } else {
                remainder
            }
        }
        _ => i32::MIN,
    };
    if !matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::IntegerDivide
            | BinaryOp::Modulo
    ) {
        return None;
    }
    if scalar {
        let value = apply(left.side.get(0), right.side.get(0));
        return Some(Value::Scalar(Scalar::Integer(
            (value != i32::MIN).then_some(value),
        )));
    }
    let values = match op {
        BinaryOp::Add => zip_map(len, &left.side, &right.side, |l, r| {
            checked(i64::from(l) + i64::from(r))
        }),
        BinaryOp::Subtract => zip_map(len, &left.side, &right.side, |l, r| {
            checked(i64::from(l) - i64::from(r))
        }),
        BinaryOp::Multiply => zip_map(len, &left.side, &right.side, |l, r| {
            checked(i64::from(l) * i64::from(r))
        }),
        _ => zip_map(len, &left.side, &right.side, apply),
    };
    let validity = mark_na(
        &values,
        and(left.validity.as_ref(), right.validity.as_ref()),
    );
    Some(Value::Vector(Vector::Integer {
        values: Buffer::from(values),
        validity,
    }))
}

fn double_arithmetic(
    op: BinaryOp,
    left: Operand<f64>,
    right: Operand<f64>,
    len: usize,
) -> Option<Value> {
    let scalar = matches!(
        (&left.side, &right.side),
        (Side::Scalar(_), Side::Scalar(_))
    );
    let apply = |l: f64, r: f64| match op {
        BinaryOp::Add => l + r,
        BinaryOp::Subtract => l - r,
        BinaryOp::Multiply => l * r,
        BinaryOp::Divide => l / r,
        // `1^NaN` and `NaN^0` are 1 in R. `powf` gives NaN for a signaling NaN such as the bits
        // of R's `NA_real_`, which a null slot may still hold, so both cases are decided here.
        BinaryOp::Power if l == 1.0 || r == 0.0 => 1.0,
        BinaryOp::Power => l.powf(r),
        BinaryOp::IntegerDivide => (l / r).floor(),
        // R's `%%` takes the sign of the divisor.
        BinaryOp::Modulo => l - (l / r).floor() * r,
        _ => f64::NAN,
    };
    if !matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Power
            | BinaryOp::IntegerDivide
            | BinaryOp::Modulo
    ) {
        return None;
    }
    // `NA^0` and `1^NA` are 1 in R, so a power is known whenever a known side decides it.
    let power_decides =
        |side: &Side<f64>, validity: &Option<Bitmap>, decides: fn(f64) -> bool| match side {
            Side::Scalar(Some(value)) => Some(Bitmap::new_with_value(decides(*value), len)),
            Side::Scalar(None) => Some(Bitmap::new_zeroed(len)),
            Side::Array(values) => {
                let hits = pack_map(len, values, decides);
                Some(match validity {
                    Some(validity) => &hits & validity,
                    None => hits,
                })
            }
        };
    if scalar {
        let (l, r) = match (&left.side, &right.side) {
            (Side::Scalar(l), Side::Scalar(r)) => (*l, *r),
            _ => return None,
        };
        let value = match (l, r) {
            (Some(l), Some(r)) => Some(apply(l, r)),
            (Some(l), None) if op == BinaryOp::Power && l == 1.0 => Some(1.0),
            (None, Some(r)) if op == BinaryOp::Power && r == 0.0 => Some(1.0),
            _ => None,
        };
        return Some(Value::Scalar(Scalar::Double(value)));
    }
    let any_na = left.is_na_scalar()
        || right.is_na_scalar()
        || left.validity.is_some()
        || right.validity.is_some();
    let validity = if !any_na {
        None
    } else {
        let all = Bitmap::new_with_value(true, len);
        let side_validity = |operand: &Operand<f64>| match &operand.side {
            Side::Scalar(None) => Bitmap::new_zeroed(len),
            _ => operand.validity.clone().unwrap_or_else(|| all.clone()),
        };
        let both = &side_validity(&left) & &side_validity(&right);
        if op == BinaryOp::Power {
            let one = power_decides(&left.side, &left.validity, |value| value == 1.0)?;
            let zero = power_decides(&right.side, &right.validity, |value| value == 0.0)?;
            Some(&(&both | &one) | &zero)
        } else {
            Some(both)
        }
    };
    let values = match op {
        BinaryOp::Add => zip_map(len, &left.side, &right.side, |l, r| l + r),
        BinaryOp::Subtract => zip_map(len, &left.side, &right.side, |l, r| l - r),
        BinaryOp::Multiply => zip_map(len, &left.side, &right.side, |l, r| l * r),
        BinaryOp::Divide => zip_map(len, &left.side, &right.side, |l, r| l / r),
        _ => zip_map(len, &left.side, &right.side, apply),
    };
    Some(Value::Vector(Vector::Double {
        values: Buffer::from(values),
        validity,
    }))
}

fn unary_map<T, F>(operand: Operand<T>, len: usize, apply: F) -> Value
where
    T: Copy + Send + Sync + Default,
    F: Fn(T) -> T + Sync,
    Operand<T>: IntoValue,
{
    let side = match operand.side {
        Side::Scalar(value) => Side::Scalar(value.map(&apply)),
        Side::Array(values) => {
            let mapped = fill(len, |start, out| {
                let end = start + out.len();
                for (slot, value) in out.iter_mut().zip(&values[start..end]) {
                    slot.write(apply(*value));
                }
            });
            Side::Array(Buffer::from(mapped))
        }
    };
    Operand {
        side,
        validity: operand.validity,
    }
    .into_value()
}

trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for Operand<i32> {
    fn into_value(self) -> Value {
        Operand::<i32>::into_value(self)
    }
}

impl IntoValue for Operand<f64> {
    fn into_value(self) -> Value {
        Operand::<f64>::into_value(self)
    }
}

/// `fifelse(test, yes, no)`: an `NA` test gives `NA`.
fn if_else(test: Logicals, yes: Vector, no: Vector, len: usize) -> Option<Value> {
    let all = Bitmap::new_with_value(true, len);
    let test_validity = test.validity.clone().unwrap_or_else(|| all.clone());
    let yes_validity = yes.validity().cloned().unwrap_or_else(|| all.clone());
    let no_validity = no.validity().cloned().unwrap_or_else(|| all.clone());
    let chosen = test.values.select(&yes_validity, &no_validity);
    let validity = &test_validity & &chosen;
    let validity = (validity.unset_bits() > 0).then_some(validity);
    Some(Value::Vector(match (yes, no) {
        (Vector::Logical { values: y, .. }, Vector::Logical { values: n, .. }) => Vector::Logical {
            values: test.values.select(&y, &n),
            validity,
        },
        (Vector::Integer { values: y, .. }, Vector::Integer { values: n, .. }) => Vector::Integer {
            values: Buffer::from(select_values(&test.values, &y, &n)),
            validity,
        },
        (Vector::Double { values: y, .. }, Vector::Double { values: n, .. }) => Vector::Double {
            values: Buffer::from(select_values(&test.values, &y, &n)),
            validity,
        },
        _ => return None,
    }))
}

fn select_values<T: Copy + Send + Sync>(test: &Bitmap, yes: &[T], no: &[T]) -> Vec<T> {
    let len = test.len();
    fill(len, |start, out| {
        for (offset, slot) in out.iter_mut().enumerate() {
            let row = start + offset;
            slot.write(if test.get_bit(row) { yes[row] } else { no[row] });
        }
    })
}

/// The rows a logical filter selects: `TRUE` and not `NA`.
pub(crate) fn selected_rows(filter: Value, len: usize) -> Option<Vec<u32>> {
    let filter = logicals(filter, len)?;
    let selected = match &filter.validity {
        Some(validity) => &filter.values & validity,
        None => filter.values,
    };
    selected
        .true_idx_iter()
        .map(|row| u32::try_from(row).ok())
        .collect()
}

/// Writes `value` (one per row of `rows`, or a scalar) into `column` at `rows`. The column's
/// buffer is reused when nothing else shares it, so a 1% update costs 1% of the column. `value`
/// must already have the column's type.
pub(crate) fn scatter(column: Column, rows: &[u32], value: Value) -> Result<Column> {
    let name = column.name().clone();
    let len = column.len();
    let mismatch = || Error::Query(format!("cannot write into column `{name}`"));
    let dtype = column.dtype().clone();
    let value_validity = |offset: usize| match &value {
        Value::Scalar(scalar) => !scalar.is_na(),
        Value::Vector(vector) => vector
            .validity()
            .is_none_or(|validity| validity.get_bit(offset)),
    };
    let value_has_na = match &value {
        Value::Scalar(scalar) => scalar.is_na(),
        Value::Vector(vector) => vector.validity().is_some(),
    };
    let update_validity = |validity: Option<Bitmap>| -> Option<Bitmap> {
        if validity.is_none() && !value_has_na {
            return None;
        }
        let mut bits = match validity {
            Some(validity) => validity.make_mut(),
            None => {
                let mut bits = MutableBitmap::with_capacity(len);
                bits.extend_constant(len, true);
                bits
            }
        };
        for (offset, row) in rows.iter().enumerate() {
            bits.set(*row as usize, value_validity(offset));
        }
        let bits: Bitmap = bits.into();
        (bits.unset_bits() > 0).then_some(bits)
    };
    match (&dtype, &value) {
        (
            DataType::Int32,
            Value::Scalar(Scalar::Integer(_)) | Value::Vector(Vector::Integer { .. }),
        ) => {
            let (values, validity) = take_primitive::<Int32Type>(column);
            let mut values = values.into_mut().right_or_else(|buffer| buffer.to_vec());
            match &value {
                Value::Scalar(Scalar::Integer(scalar)) => {
                    let scalar = scalar.unwrap_or(0);
                    for row in rows {
                        values[*row as usize] = scalar;
                    }
                }
                Value::Vector(Vector::Integer {
                    values: written, ..
                }) => {
                    for (row, written) in rows.iter().zip(written.iter()) {
                        values[*row as usize] = *written;
                    }
                }
                _ => return Err(mismatch()),
            }
            let validity = update_validity(validity);
            Ok(Vector::Integer {
                values: Buffer::from(values),
                validity,
            }
            .into_column(name))
        }
        (
            DataType::Float64,
            Value::Scalar(Scalar::Double(_)) | Value::Vector(Vector::Double { .. }),
        ) => {
            let (values, validity) = take_primitive::<Float64Type>(column);
            let mut values = values.into_mut().right_or_else(|buffer| buffer.to_vec());
            match &value {
                Value::Scalar(Scalar::Double(scalar)) => {
                    let scalar = scalar.unwrap_or(0.0);
                    for row in rows {
                        values[*row as usize] = scalar;
                    }
                }
                Value::Vector(Vector::Double {
                    values: written, ..
                }) => {
                    for (row, written) in rows.iter().zip(written.iter()) {
                        values[*row as usize] = *written;
                    }
                }
                _ => return Err(mismatch()),
            }
            let validity = update_validity(validity);
            Ok(Vector::Double {
                values: Buffer::from(values),
                validity,
            }
            .into_column(name))
        }
        (
            DataType::Boolean,
            Value::Scalar(Scalar::Logical(_)) | Value::Vector(Vector::Logical { .. }),
        ) => {
            let chunked: BooleanChunked = column
                .take_materialized_series()
                .take_inner::<BooleanType>();
            let chunked = chunked.rechunk().into_owned();
            let (values, validity) = match chunked.downcast_into_iter().next() {
                Some(array) => {
                    let (_, values, validity) = array.into_inner();
                    (values, validity)
                }
                None => (Bitmap::new(), None),
            };
            let mut values = values.make_mut();
            for (offset, row) in rows.iter().enumerate() {
                let written = match &value {
                    Value::Scalar(Scalar::Logical(scalar)) => scalar.unwrap_or(false),
                    Value::Vector(Vector::Logical { values, .. }) => values.get_bit(offset),
                    _ => return Err(mismatch()),
                };
                values.set(*row as usize, written);
            }
            let validity = update_validity(validity);
            Ok(Vector::Logical {
                values: values.into(),
                validity,
            }
            .into_column(name))
        }
        _ => Err(mismatch()),
    }
}

/// A numeric column's values and validity, owned: unshared buffers stay unshared, so the caller
/// can write into them.
fn take_primitive<T>(column: Column) -> (Buffer<T::Native>, Option<Bitmap>)
where
    T: polars::prelude::PolarsNumericType,
{
    let chunked: ChunkedArray<T> = column.take_materialized_series().take_inner::<T>();
    let chunked = if chunked.chunks().len() > 1 {
        chunked.rechunk().into_owned()
    } else {
        chunked
    };
    match chunked.downcast_into_iter().next() {
        Some(array) => {
            let (_, values, validity) = array.into_inner();
            (values, validity)
        }
        None => (Buffer::from(Vec::new()), None),
    }
}

/// Builds a vector of `len` elements in parallel chunks: `write(start, out)` fills the elements
/// from `start` on.
pub(crate) fn fill<R: Send>(
    len: usize,
    write: impl Fn(usize, &mut [MaybeUninit<R>]) + Sync,
) -> Vec<R> {
    let mut out = Vec::with_capacity(len);
    let spare = &mut out.spare_capacity_mut()[..len];
    if len < PARALLEL_ROWS {
        write(0, spare);
    } else {
        THREAD_POOL.install(|| {
            spare
                .par_chunks_mut(CHUNK_ROWS)
                .enumerate()
                .for_each(|(index, chunk)| write(index * CHUNK_ROWS, chunk));
        });
    }
    // SAFETY: every element in `0..len` was written: the chunks cover the spare capacity.
    unsafe { out.set_len(len) };
    out
}

fn map_values<T: Copy + Sync, R: Send>(values: &[T], apply: impl Fn(T) -> R + Sync) -> Vec<R> {
    fill(values.len(), |start, out| {
        let end = start + out.len();
        for (slot, value) in out.iter_mut().zip(&values[start..end]) {
            slot.write(apply(*value));
        }
    })
}

fn zip_map<T, R>(
    len: usize,
    left: &Side<T>,
    right: &Side<T>,
    apply: impl Fn(T, T) -> R + Sync,
) -> Vec<R>
where
    T: Copy + Sync + Send + Default,
    R: Send,
{
    fill(len, |start, out| {
        let end = start + out.len();
        match (left, right) {
            (Side::Array(l), Side::Array(r)) => {
                for ((slot, l), r) in out.iter_mut().zip(&l[start..end]).zip(&r[start..end]) {
                    slot.write(apply(*l, *r));
                }
            }
            (Side::Array(l), Side::Scalar(r)) => {
                let r = r.unwrap_or_default();
                for (slot, l) in out.iter_mut().zip(&l[start..end]) {
                    slot.write(apply(*l, r));
                }
            }
            (Side::Scalar(l), Side::Array(r)) => {
                let l = l.unwrap_or_default();
                for (slot, r) in out.iter_mut().zip(&r[start..end]) {
                    slot.write(apply(l, *r));
                }
            }
            (Side::Scalar(l), Side::Scalar(r)) => {
                let (l, r) = (l.unwrap_or_default(), r.unwrap_or_default());
                for slot in out.iter_mut() {
                    slot.write(apply(l, r));
                }
            }
        }
    })
}

/// Packs one bit per row, eight rows per byte, in parallel: `write(start_row, bytes)` fills the
/// bytes covering the rows from `start_row` on.
fn pack(len: usize, write: impl Fn(usize, &mut [MaybeUninit<u8>]) + Sync) -> Bitmap {
    let bytes = fill(len.div_ceil(8), |start_byte, out| {
        write(start_byte * 8, out)
    });
    Bitmap::from_u8_vec(bytes, len)
}

fn pack_byte<T: Copy>(values: &[T], test: &impl Fn(T) -> bool) -> u8 {
    let mut byte = 0u8;
    for (bit, value) in values.iter().enumerate() {
        byte |= u8::from(test(*value)) << bit;
    }
    byte
}

pub(crate) fn pack_map<T: Copy + Sync>(
    len: usize,
    values: &[T],
    test: impl Fn(T) -> bool + Sync,
) -> Bitmap {
    pack(len, |start, out| {
        let end = (start + out.len() * 8).min(len);
        for (slot, chunk) in out.iter_mut().zip(values[start..end].chunks(8)) {
            slot.write(pack_byte(chunk, &test));
        }
    })
}

fn pack_zip<T>(
    len: usize,
    left: &Side<T>,
    right: &Side<T>,
    test: impl Fn(T, T) -> bool + Sync,
) -> Bitmap
where
    T: Copy + Sync + Send + Default,
{
    pack(len, |start, out| {
        let end = (start + out.len() * 8).min(len);
        match (left, right) {
            (Side::Array(l), Side::Array(r)) => {
                for ((slot, l), r) in out
                    .iter_mut()
                    .zip(l[start..end].chunks(8))
                    .zip(r[start..end].chunks(8))
                {
                    let mut byte = 0u8;
                    for (bit, (l, r)) in l.iter().zip(r).enumerate() {
                        byte |= u8::from(test(*l, *r)) << bit;
                    }
                    slot.write(byte);
                }
            }
            (Side::Array(l), Side::Scalar(r)) => {
                let r = r.unwrap_or_default();
                for (slot, l) in out.iter_mut().zip(l[start..end].chunks(8)) {
                    slot.write(pack_byte(l, &|l| test(l, r)));
                }
            }
            (Side::Scalar(l), Side::Array(r)) => {
                let l = l.unwrap_or_default();
                for (slot, r) in out.iter_mut().zip(r[start..end].chunks(8)) {
                    slot.write(pack_byte(r, &|r| test(l, r)));
                }
            }
            (Side::Scalar(l), Side::Scalar(r)) => {
                let bit = test(l.unwrap_or_default(), r.unwrap_or_default());
                for (index, slot) in out.iter_mut().enumerate() {
                    let rows = (end - start).saturating_sub(index * 8).min(8);
                    slot.write(if bit { ((1u16 << rows) - 1) as u8 } else { 0 });
                }
            }
        }
    })
}

fn any<T: Copy + Sync>(values: &[T], test: impl Fn(T) -> bool + Sync) -> bool {
    if values.len() < PARALLEL_ROWS {
        values.iter().any(|value| test(*value))
    } else {
        THREAD_POOL.install(|| values.par_iter().any(|value| test(*value)))
    }
}

/// Folds the `i32::MIN` marker an integer kernel writes for an `NA` result into the validity.
fn mark_na(values: &[i32], validity: Option<Bitmap>) -> Option<Bitmap> {
    if !any(values, |value| value == i32::MIN) {
        return validity;
    }
    let marked = pack_map(values.len(), values, |value| value != i32::MIN);
    and(validity.as_ref(), Some(&marked))
}

pub(crate) fn and(left: Option<&Bitmap>, right: Option<&Bitmap>) -> Option<Bitmap> {
    match (left, right) {
        (None, None) => None,
        (Some(bits), None) | (None, Some(bits)) => Some(bits.clone()),
        (Some(left), Some(right)) => Some(left & right),
    }
}

pub(crate) fn gather_values<T: Copy + Send + Sync>(values: &[T], rows: &[u32]) -> Buffer<T> {
    Buffer::from(fill(rows.len(), |start, out| {
        let end = start + out.len();
        for (slot, row) in out.iter_mut().zip(&rows[start..end]) {
            slot.write(values[*row as usize]);
        }
    }))
}

pub(crate) fn gather_bits(bits: &Bitmap, rows: &[u32]) -> Bitmap {
    pack(rows.len(), |start, out| {
        let end = (start + out.len() * 8).min(rows.len());
        for (slot, chunk) in out.iter_mut().zip(rows[start..end].chunks(8)) {
            let mut byte = 0u8;
            for (bit, row) in chunk.iter().enumerate() {
                byte |= u8::from(bits.get_bit(*row as usize)) << bit;
            }
            slot.write(byte);
        }
    })
}

/// Wraps a pointer so parallel tasks can write disjoint elements of one buffer.
#[derive(Clone, Copy)]
pub(crate) struct Shared<T>(pub(crate) *mut T);
// SAFETY: tasks only write disjoint elements, as each use documents.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    /// # Safety
    /// `index` must be in bounds, and no other task may access that element concurrently.
    pub(crate) unsafe fn write(self, index: usize, value: T) {
        // SAFETY: guaranteed by the caller.
        unsafe { self.0.add(index).write(value) }
    }
}
