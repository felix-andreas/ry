//! The answer `vapply` assembles from its calls' results: each result is checked against
//! `FUN.VALUE` and written straight into one preallocated vector, and the names, dims and
//! dimnames are set once at the end. Every loop that calls `FUN` per element (the interpreted
//! `vapply` and a compiled one alike) hands its results here, so the checks and their error
//! text have one implementation.

use {
    crate::{
        subset::int_to_real,
        unwind::{R, error},
        values::{self, alloc, get_attr, length, nil, symbols},
    },
    heap::object::{NA_INTEGER, NA_REAL, Rcomplex, Sexp, SexpType},
};

pub struct VapplyResults {
    result: Sexp,
    kind: SexpType,
    count: usize,
    /// The length every result must have.
    width: usize,
    /// `FUN.VALUE`'s integer dims, when it is an array: they become the answer's leading dims.
    array_dims: Option<Sexp>,
    use_names: bool,
    /// `FUN.VALUE`'s names (dimnames for an array), or failing that the first result's.
    row_names: Sexp,
}

impl VapplyResults {
    /// Checks `FUN.VALUE` and allocates the answer for `count` results. `use_names` is
    /// `USE.NAMES` as given; it must be `TRUE` or `FALSE`.
    pub unsafe fn new(template: Sexp, count: usize, use_names: Sexp) -> R<VapplyResults> {
        let kind = template.kind();
        if !kind.is_vector() {
            return Err(error("'FUN.VALUE' must be a vector"));
        }
        let use_names = match values::as_logical_scalar(use_names) {
            0 => false,
            1 => true,
            _ => return Err(error("invalid 'USE.NAMES' value")),
        };
        let width = length(template);
        let dims = get_attr(template, symbols().dim);
        let array_dims = (dims.kind() == SexpType::Integer && length(dims) >= 1).then_some(dims);
        let row_names = if array_dims.is_some() {
            get_attr(template, symbols().dimnames)
        } else {
            values::names(template)
        };
        Ok(VapplyResults {
            result: alloc(kind, count * width)?,
            kind,
            count,
            width,
            array_dims,
            use_names,
            row_names,
        })
    }

    /// Checks `FUN(X[[index + 1]])`'s result and writes it as column `index`. The caller keeps
    /// `value` reachable until `finish`, because the first result may supply the row names.
    pub unsafe fn push(&mut self, index: usize, value: Sexp) -> R<()> {
        if length(value) != self.width {
            return Err(error(format!(
                "values must be length {},\n but FUN(X[[{}]]) result is length {}",
                self.width,
                index + 1,
                length(value)
            )));
        }
        let from = value.kind();
        let widens = match self.kind {
            SexpType::Complex => {
                matches!(from, SexpType::Real | SexpType::Integer | SexpType::Logical)
            }
            SexpType::Real => matches!(from, SexpType::Integer | SexpType::Logical),
            SexpType::Integer => from == SexpType::Logical,
            _ => false,
        };
        if from != self.kind && !widens {
            return Err(error(format!(
                "values must be type '{}',\n but FUN(X[[{}]]) result is type '{}'",
                self.kind.name(),
                index + 1,
                from.name()
            )));
        }
        if index == 0 && self.use_names && self.row_names == nil() {
            // A one-dimensional array, such as a table, names its elements through its dimnames.
            self.row_names = if self.array_dims.is_some() {
                get_attr(value, symbols().dimnames)
            } else {
                values::names(value)
            };
        }
        let value = values::materialize(value)?;
        let start = index * self.width;
        let end = start + self.width;
        match (self.kind, from) {
            (SexpType::Logical | SexpType::Integer, SexpType::Logical | SexpType::Integer) => {
                values::ints_mut(self.result)[start..end].copy_from_slice(values::ints(value))
            }
            (SexpType::Real, SexpType::Real) => {
                values::reals_mut(self.result)[start..end].copy_from_slice(values::reals(value))
            }
            (SexpType::Real, _) => {
                for (slot, &element) in values::reals_mut(self.result)[start..end]
                    .iter_mut()
                    .zip(values::ints(value))
                {
                    *slot = int_to_real(element);
                }
            }
            (SexpType::Complex, SexpType::Complex) => values::complexes_mut(self.result)
                [start..end]
                .copy_from_slice(values::complexes(value)),
            (SexpType::Complex, SexpType::Real) => {
                for (slot, &element) in values::complexes_mut(self.result)[start..end]
                    .iter_mut()
                    .zip(values::reals(value))
                {
                    *slot = complex_from_real(element);
                }
            }
            (SexpType::Complex, _) => {
                for (slot, &element) in values::complexes_mut(self.result)[start..end]
                    .iter_mut()
                    .zip(values::ints(value))
                {
                    *slot = complex_from_integer(element);
                }
            }
            (SexpType::Raw, _) => {
                values::raws_mut(self.result)[start..end].copy_from_slice(values::raws(value))
            }
            _ => {
                for offset in 0..self.width {
                    values::set_elt(self.result, start + offset, value.element(offset));
                }
            }
        }
        Ok(())
    }

    /// The answer, with dims when results are not scalars and names taken from `list` (`X`).
    pub unsafe fn finish(self, list: Sexp) -> R<Sexp> {
        let symbols = symbols();
        let rank = self.array_dims.map_or(1, |dims| length(dims));
        if self.width != 1 {
            let dims = alloc(SexpType::Integer, rank + 1)?;
            let extents = values::ints_mut(dims);
            match self.array_dims {
                Some(array_dims) => extents[..rank].copy_from_slice(values::ints(array_dims)),
                None => extents[0] = self.width as i32,
            }
            extents[rank] = self.count as i32;
            values::set_attr(self.result, symbols.dim, dims)?;
        }
        if !self.use_names {
            return Ok(self.result);
        }
        let mut names = values::names(list);
        if names == nil() && list.kind() == SexpType::String {
            names = list;
        }
        if self.width == 1 {
            if names != nil() {
                values::set_attr(self.result, symbols.names, names)?;
            }
            return Ok(self.result);
        }
        // Setting dimnames stores a component without elements as NULL, which is what the names
        // of a zero-length character input are.
        if names != nil() && length(names) == 0 {
            names = nil();
        }
        if names == nil() && self.row_names == nil() {
            return Ok(self.result);
        }
        let dimnames = if self.array_dims.is_some() && self.row_names != nil() {
            if self.row_names.kind() != SexpType::List || length(self.row_names) != rank {
                return Err(error(format!(
                    "dimnames(<value>) is neither NULL nor list of length {rank}"
                )));
            }
            let dimnames = alloc(SexpType::List, rank + 1)?;
            for position in 0..rank {
                values::set_elt(dimnames, position, self.row_names.element(position));
            }
            values::set_elt(dimnames, rank, names);
            dimnames
        } else {
            values::list_vector(&[self.row_names, names])?
        };
        values::set_attr(self.result, symbols.dimnames, dimnames)?;
        Ok(self.result)
    }
}

// GNU R's coercions differ on NA: a double NA keeps a zero imaginary part, an integer or
// logical NA becomes NA in both parts.
fn complex_from_real(value: f64) -> Rcomplex {
    Rcomplex { r: value, i: 0.0 }
}

fn complex_from_integer(value: i32) -> Rcomplex {
    if value == NA_INTEGER {
        Rcomplex {
            r: NA_REAL,
            i: NA_REAL,
        }
    } else {
        complex_from_real(f64::from(value))
    }
}
