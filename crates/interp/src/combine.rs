//! `c()` and `unlist()`, in GNU R's two passes: the first finds the result's type, its length
//! and whether any name exists, and the second writes the elements and their names straight into
//! the result, so nothing is recorded per element in between.

use {
    crate::{
        builtins::{Arg, BuiltinDef, Call, Generic, builtin, internal, is_native_value},
        coerce,
        subset::{copy_element, int_to_real},
        unwind::R,
        values::{self, alloc, char_str, length, na_string, nil, symbols},
    },
    heap::object::{Sexp, SexpType},
};

pub const BUILTINS: &[BuiltinDef] = &[
    builtin("c", -1, do_c)
        .generic(Generic::Internal)
        .direct(direct_c),
    internal("unlist", 3, do_unlist).generic(Generic::Internal),
];

/// `c()` of already evaluated values, without names.
pub unsafe fn combine(items: &[Sexp]) -> R<Sexp> {
    combine_items(Items::Values(items), false, true)
}

/// `c()` of evaluated arguments, whose tags name them.
pub unsafe fn combine_args(args: &[Arg], recursive: bool, use_names: bool) -> R<Sexp> {
    combine_items(Items::Args(args), recursive, use_names)
}

unsafe fn do_c(call: &Call<'_>) -> R<Sexp> {
    let symbols = symbols();
    let mut recursive = false;
    let mut use_names = true;
    let options = call
        .args
        .iter()
        .filter(|arg| arg.tag == symbols.recursive || arg.tag == symbols.use_names)
        .count();
    for arg in call.args {
        if arg.tag == symbols.recursive {
            recursive = values::as_logical_scalar(arg.value) == 1;
        } else if arg.tag == symbols.use_names {
            use_names = values::as_logical_scalar(arg.value) != 0;
        }
    }
    if options == 0 {
        return combine_items(Items::Args(call.args), recursive, use_names);
    }
    let args: smallvec::SmallVec<[Arg; 8]> = call
        .args
        .iter()
        .filter(|arg| arg.tag != symbols.recursive && arg.tag != symbols.use_names)
        .copied()
        .collect();
    combine_items(Items::Args(&args), recursive, use_names)
}

/// `c(...)` of untagged arguments, none of which dispatches. When every argument is `NULL` or an
/// atomic vector without attributes, no names can arise, so one pass sizes the result and a
/// second copies the elements, without the naming walk.
unsafe fn direct_c(_call: Sexp, args: &[Sexp]) -> R<Option<Sexp>> {
    let mut kind = SexpType::Nil;
    let mut total = 0;
    let mut plain = true;
    for &value in args {
        if value.is_object() || is_native_value(value) {
            return Ok(None);
        }
        if value == nil() {
            continue;
        }
        if !value.kind().is_atomic() || value.is_altrep() || value.attrib() != nil() {
            plain = false;
            continue;
        }
        kind = coerce::higher_type(kind, value.kind());
        total += length(value);
    }
    if !plain {
        return combine_items(Items::Values(args), false, true).map(Some);
    }
    if kind == SexpType::Nil {
        return Ok(Some(nil()));
    }
    let result = alloc(kind, total)?;
    crate::heap().root(result);
    let mut combiner = Combiner {
        recursive: false,
        result,
        names: nil(),
        position: 0,
        any_names: false,
        base: String::new(),
        base_name: None,
    };
    for &value in args {
        if value != nil() {
            let count = length(value);
            combiner.write_elements(value, count)?;
            combiner.position += count;
        }
    }
    Ok(Some(result))
}

unsafe fn do_unlist(call: &Call<'_>) -> R<Sexp> {
    let object = call.arg(0);
    let recursive = values::as_logical_scalar(call.arg(1)) != 0;
    let use_names = values::as_logical_scalar(call.arg(2)) != 0;
    if !matches!(
        object.kind(),
        SexpType::List | SexpType::Pairlist | SexpType::Expression
    ) {
        return Ok(object);
    }
    let list = if object.kind() == SexpType::Pairlist {
        let list = values::pairlist_to_list(object)?;
        crate::heap().root(list);
        list
    } else {
        object
    };
    let names = values::names(list);
    combine_items(Items::List { list, names }, recursive, use_names)
}

/// The arguments being combined, each with the name it carries.
#[derive(Clone, Copy)]
enum Items<'a> {
    Values(&'a [Sexp]),
    Args(&'a [Arg]),
    /// The elements of `unlist()`'s list, named by `names` (a character vector or `NULL`), where
    /// an `NA` name stands for itself.
    List {
        list: Sexp,
        names: Sexp,
    },
}

impl Items<'_> {
    unsafe fn len(self) -> usize {
        match self {
            Items::Values(values) => values.len(),
            Items::Args(args) => args.len(),
            Items::List { list, .. } => length(list),
        }
    }

    /// The item's value and its name: a blank string when it has none.
    unsafe fn get(self, index: usize) -> (Sexp, Sexp) {
        match self {
            Items::Values(values) => (values[index], values::blank_string()),
            Items::Args(args) => {
                let arg = args[index];
                let name = if arg.tag == nil() {
                    values::blank_string()
                } else {
                    arg.tag.car()
                };
                (arg.value, name)
            }
            Items::List { list, names } => (list.element(index), own_name(names, index)),
        }
    }
}

unsafe fn combine_items(items: Items<'_>, recursive: bool, use_names: bool) -> R<Sexp> {
    let mut kind = SexpType::Nil;
    for index in 0..items.len() {
        kind = answer_type(items.get(index).0, kind, recursive)?;
    }
    if kind == SexpType::Nil {
        return Ok(nil());
    }
    let mut combiner = Combiner {
        recursive,
        result: nil(),
        names: nil(),
        position: 0,
        any_names: false,
        base: String::new(),
        base_name: None,
    };
    for index in 0..items.len() {
        let (value, name) = items.get(index);
        combiner.add(value, name)?;
    }
    let heap = crate::heap();
    let result = alloc(kind, combiner.position)?;
    heap.root(result);
    let named = combiner.any_names && use_names && combiner.position > 0;
    if named {
        combiner.names = alloc(SexpType::String, combiner.position)?;
        heap.root(combiner.names);
    }
    combiner.result = result;
    combiner.position = 0;
    for index in 0..items.len() {
        let (value, name) = items.get(index);
        combiner.add(value, name)?;
    }
    if named {
        values::set_attr(result, symbols().names, combiner.names)?;
    }
    Ok(result)
}

/// The type of the result, widened by `value`: GNU R's `AnswerType`.
unsafe fn answer_type(value: Sexp, kind: SexpType, recursive: bool) -> R<SexpType> {
    let value = values::materialize(value)?;
    let own = match value.kind() {
        SexpType::Nil => return Ok(kind),
        SexpType::List | SexpType::Pairlist if recursive => {
            let list = if value.kind() == SexpType::Pairlist {
                let list = values::pairlist_to_list(value)?;
                crate::heap().root(list);
                list
            } else {
                value
            };
            let mut kind = kind;
            for index in 0..length(list) {
                kind = answer_type(list.element(index), kind, recursive)?;
            }
            return Ok(kind);
        }
        SexpType::Expression => SexpType::Expression,
        own if own.is_atomic() => own,
        _ => SexpType::List,
    };
    Ok(coerce::higher_type(kind, own))
}

/// One walk over the items: with `result` `NULL` it only counts the elements and notes whether
/// any name exists, and otherwise it writes each element, and its name when `names` is set, at
/// `position`.
struct Combiner {
    recursive: bool,
    result: Sexp,
    names: Sexp,
    position: usize,
    any_names: bool,
    /// The tagged base of the elements being added, such as `a.b` inside `c(a = list(b = ...))`;
    /// empty when there is none.
    base: String,
    /// The interned name `base` was started from, while `base` still holds exactly it, so a
    /// lone unnamed leaf under a tag (`c(a = 1)`) reuses the tag's string instead of
    /// interning `base` again.
    base_name: Option<Sexp>,
}

/// The names GNU R gives the leaves under one tagged base: a leaf without a name of its own is
/// the base alone when it is the only such leaf, and otherwise the base followed by its position
/// among all the leaves under that base.
struct NameScope {
    /// The base is an NA name, which a lone unnamed leaf keeps as NA.
    base_is_na: bool,
    count: usize,
    position: usize,
}

impl Combiner {
    unsafe fn add(&mut self, value: Sexp, name: Sexp) -> R<()> {
        let value = flattenable(value)?;
        let unnamed = name != na_string() && values::is_blank(name);
        if !unnamed {
            self.base.push_str(&char_str(name));
            self.base_name = Some(name);
        }
        let mut scope = NameScope {
            base_is_na: name == na_string(),
            count: if unnamed || self.names == nil() {
                0
            } else {
                self.unnamed_leaves(value)?
            },
            position: 0,
        };
        let added = self.add_in(value, &mut scope);
        self.base.clear();
        self.base_name = None;
        added
    }

    unsafe fn add_in(&mut self, value: Sexp, scope: &mut NameScope) -> R<()> {
        let count = length(value);
        let element_names = values::names(value);
        if !self.base.is_empty() || element_names != nil() {
            self.any_names = true;
        }
        let recursed =
            self.recursive && matches!(value.kind(), SexpType::List | SexpType::Expression);
        if recursed {
            for index in 0..count {
                let element = flattenable(value.element(index))?;
                let own = own_name(element_names, index);
                if own != na_string() && values::is_blank(own) {
                    self.add_in(element, scope)?;
                    continue;
                }
                // A named element starts a new base, so its leaves count afresh, but they still
                // take their places in the enclosing base's numbering.
                let mark = self.base.len();
                let base_is_na = own == na_string() && mark == 0;
                if mark > 0 {
                    self.base.push('.');
                }
                self.base.push_str(&char_str(own));
                let mut inner = NameScope {
                    base_is_na,
                    count: if self.names == nil() {
                        0
                    } else {
                        self.unnamed_leaves(element)?
                    },
                    position: 0,
                };
                let added = self.add_in(element, &mut inner);
                self.base.truncate(mark);
                added?;
                scope.position += inner.position;
            }
            return Ok(());
        }
        if value == nil() {
            return Ok(());
        }
        if !value.kind().is_vector() {
            if self.result != nil() {
                values::set_elt(self.result, self.position, value);
                self.write_name(scope, self.position, values::blank_string())?;
            }
            self.position += 1;
            return Ok(());
        }
        if self.result != nil() {
            self.write_elements(value, count)?;
            for index in 0..count {
                self.write_name(scope, self.position + index, own_name(element_names, index))?;
            }
        }
        self.position += count;
        Ok(())
    }

    /// Copies the `count` elements of the vector `value` to `position` onwards, converting them
    /// to the result's type, slice by slice where the types allow.
    unsafe fn write_elements(&mut self, value: Sexp, count: usize) -> R<()> {
        let result = self.result;
        let start = self.position;
        let target = start..start + count;
        match (value.kind(), result.kind()) {
            (SexpType::Real, SexpType::Real) => {
                values::reals_mut(result)[target].copy_from_slice(values::reals(value))
            }
            (SexpType::Integer, SexpType::Integer)
            | (SexpType::Logical, SexpType::Logical)
            | (SexpType::Logical, SexpType::Integer) => {
                values::ints_mut(result)[target].copy_from_slice(values::ints(value))
            }
            (SexpType::Integer | SexpType::Logical, SexpType::Real) => {
                for (slot, &element) in values::reals_mut(result)[target]
                    .iter_mut()
                    .zip(values::ints(value))
                {
                    *slot = int_to_real(element);
                }
            }
            (SexpType::Complex, SexpType::Complex) => {
                values::complexes_mut(result)[target].copy_from_slice(values::complexes(value))
            }
            (SexpType::Raw, SexpType::Raw) => {
                values::raws_mut(result)[target].copy_from_slice(values::raws(value))
            }
            _ => {
                for index in 0..count {
                    copy_element(value, index, result, start + index)?;
                }
            }
        }
        Ok(())
    }

    /// Writes the name of the leaf at `index`, whose own name is `own`: blank when it has none,
    /// and NA kept as NA unless a base is prefixed to it.
    unsafe fn write_name(&mut self, scope: &mut NameScope, index: usize, own: Sexp) -> R<()> {
        scope.position += 1;
        if self.names == nil() {
            return Ok(());
        }
        if self.base.is_empty() {
            values::set_elt(self.names, index, own);
            return Ok(());
        }
        let mark = self.base.len();
        if own == na_string() || !values::is_blank(own) {
            self.base.push('.');
            self.base.push_str(&char_str(own));
        } else if scope.count == 1 {
            if scope.base_is_na {
                values::set_elt(self.names, index, na_string());
                return Ok(());
            }
            if let Some(name) = self.base_name
                && name.length() as usize == self.base.len()
            {
                values::set_elt(self.names, index, name);
                return Ok(());
            }
        } else {
            let mut digits = [0u8; 20];
            let mut start = digits.len();
            let mut rest = scope.position;
            loop {
                start -= 1;
                digits[start] = b'0' + (rest % 10) as u8;
                rest /= 10;
                if rest == 0 {
                    break;
                }
            }
            self.base
                .push_str(std::str::from_utf8(&digits[start..]).unwrap_or_default());
        }
        let written = values::set_mkchar(self.names, index, &self.base);
        self.base.truncate(mark);
        written
    }

    /// GNU R's `namesCount`: every element of an atomic vector or of a list that is not
    /// recursed into, and the leaves of unnamed list elements, stopping once past one.
    unsafe fn unnamed_leaves(&self, value: Sexp) -> R<usize> {
        let recursed =
            matches!(value.kind(), SexpType::List | SexpType::Expression) && self.recursive;
        if !recursed {
            return Ok(if value.kind().is_vector() || value == nil() {
                length(value)
            } else {
                1
            });
        }
        let names = values::names(value);
        let mut count = 0;
        for index in 0..length(value) {
            if count > 1 {
                break;
            }
            let own = own_name(names, index);
            if own != na_string() && values::is_blank(own) {
                count += self.unnamed_leaves(flattenable(value.element(index))?)?;
            }
        }
        Ok(count)
    }
}

/// A pairlist combines like the list of its elements, and an ALTREP vector like its elements.
unsafe fn flattenable(value: Sexp) -> R<Sexp> {
    let value = values::materialize(value)?;
    if value.kind() == SexpType::Pairlist {
        let list = values::pairlist_to_list(value)?;
        crate::heap().root(list);
        Ok(list)
    } else {
        Ok(value)
    }
}

/// The element's own name: blank when it has none.
unsafe fn own_name(names: Sexp, index: usize) -> Sexp {
    if names != nil() && index < length(names) {
        names.element(index)
    } else {
        values::blank_string()
    }
}
