//! Frame columns exposed to R as ALTREP vectors, read back through libr's own accessors, which
//! are the paths `INTEGER`, `REAL_ELT`, `STRING_ELT` and `DATAPTR_OR_NULL` take.

#![cfg(feature = "runtime")]

use std::sync::{Mutex, MutexGuard};

use heap::object::{NA_INTEGER, NA_REAL, Sexp, SexpType};
use libr::{
    altrep::{dataptr, dataptr_or_null, int_elt, length, no_na, real_elt, sexp_elt},
    attributes::get_attrib,
    globals::{R_ClassSymbol, R_LevelsSymbol, R_NamesSymbol},
    runtime,
};
use table::{
    Table,
    altrep::{expose_column, expose_table, import_column},
    convert::{RVector, is_na_real},
    polars::prelude::PlSmallStr,
};

/// The R runtime is one process-wide session, so the tests take turns.
static SESSION: Mutex<()> = Mutex::new(());

fn session() -> MutexGuard<'static, ()> {
    SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn frame(columns: Vec<(&str, RVector)>) -> Table {
    Table::from_r(
        columns
            .into_iter()
            .map(|(name, vector)| (PlSmallStr::from(name), vector))
            .collect(),
    )
    .unwrap_or_else(|error| panic!("cannot build the frame: {error}"))
}

fn expose(table: &mut Table, name: &str) -> Sexp {
    unsafe { expose_column(table, name) }
        .unwrap_or_else(|error| panic!("cannot expose {name}: {error:?}"))
}

fn strings_of(vector: Sexp) -> Vec<Option<String>> {
    unsafe {
        let na = runtime().heap.known().na_string;
        let count = if vector.is_altrep() {
            length(vector).unwrap_or_else(|error| panic!("{error:?}"))
        } else {
            vector.length()
        };
        (0..count)
            .map(|index| {
                let element = if vector.is_altrep() {
                    sexp_elt(vector, index).unwrap_or_else(|error| panic!("{error:?}"))
                } else {
                    vector.element(index as usize)
                };
                (element != na).then(|| String::from_utf8_lossy(element.bytes()).into_owned())
            })
            .collect()
    }
}

#[test]
fn an_integer_column_without_na_shares_its_arrow_buffer() {
    let _session = session();
    let mut table = frame(vec![("x", RVector::Integer(vec![3, 1, 4, 1, 5]))]);
    let arrow = table.column_slice_i32("x").map(<[i32]>::as_ptr);
    let vector = expose(&mut table, "x");
    unsafe {
        assert!(vector.is_altrep());
        assert_eq!(vector.kind(), SexpType::Integer);
        assert_eq!(length(vector).ok(), Some(5));
        assert_eq!(
            dataptr_or_null(vector)
                .ok()
                .map(|pointer| pointer.cast::<i32>()),
            arrow
        );
        assert_eq!(
            dataptr(vector, false)
                .ok()
                .map(|pointer| pointer.cast::<i32>().cast_const()),
            arrow
        );
        assert_eq!(int_elt(vector, 2).ok(), Some(4));
        assert_eq!(no_na(vector).ok(), Some(1));
        assert_eq!(vector.cdr(), runtime().nil(), "reading never materializes");
    }
}

#[test]
fn a_column_with_na_materializes_only_when_a_pointer_is_needed() {
    let _session = session();
    let mut table = frame(vec![("x", RVector::Integer(vec![7, NA_INTEGER, 9]))]);
    let vector = expose(&mut table, "x");
    unsafe {
        assert_eq!(int_elt(vector, 1).ok(), Some(NA_INTEGER));
        assert!(
            dataptr_or_null(vector).is_ok_and(|pointer| pointer.is_null()),
            "Arrow validity is not R's layout"
        );
        assert_eq!(no_na(vector).ok(), Some(0));
        let pointer = dataptr(vector, false)
            .unwrap_or_else(|error| panic!("{error:?}"))
            .cast::<i32>();
        assert_eq!(std::slice::from_raw_parts(pointer, 3), [7, NA_INTEGER, 9]);
        assert_ne!(vector.cdr(), runtime().nil());
    }
}

#[test]
fn doubles_keep_na_and_nan_apart() {
    let _session = session();
    let mut table = frame(vec![("x", RVector::Double(vec![1.5, NA_REAL, f64::NAN]))]);
    let vector = expose(&mut table, "x");
    unsafe {
        assert_eq!(real_elt(vector, 0).ok(), Some(1.5));
        assert!(real_elt(vector, 1).is_ok_and(is_na_real));
        assert!(real_elt(vector, 2).is_ok_and(|value| value.is_nan() && !is_na_real(value)));
    }
}

#[test]
fn logical_and_character_columns_read_as_r_vectors() {
    let _session = session();
    let mut table = frame(vec![
        ("flag", RVector::Logical(vec![1, 0, NA_INTEGER])),
        (
            "name",
            RVector::Character(vec![Some("a".into()), None, Some("ü".into())]),
        ),
    ]);
    let flag = expose(&mut table, "flag");
    let name = expose(&mut table, "name");
    unsafe {
        assert_eq!(flag.kind(), SexpType::Logical);
        assert_eq!(
            (0..3)
                .map(|index| int_elt(flag, index).ok())
                .collect::<Vec<_>>(),
            [Some(1), Some(0), Some(NA_INTEGER)]
        );
        assert_eq!(name.kind(), SexpType::String);
    }
    assert_eq!(
        strings_of(name),
        [Some("a".to_string()), None, Some("ü".to_string())]
    );
}

#[test]
fn a_factor_is_an_integer_vector_with_levels() {
    let _session = session();
    let levels = vec!["lo".to_string(), "hi".to_string()];
    let mut table = frame(vec![(
        "f",
        RVector::Factor {
            codes: vec![2, NA_INTEGER, 1],
            levels,
        },
    )]);
    let vector = expose(&mut table, "f");
    unsafe {
        assert_eq!(vector.slice::<i32>(), [2, NA_INTEGER, 1]);
        let levels =
            get_attrib(vector, R_LevelsSymbol.get()).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            strings_of(levels),
            [Some("lo".to_string()), Some("hi".to_string())]
        );
        let class =
            get_attrib(vector, R_ClassSymbol.get()).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(strings_of(class), [Some("factor".to_string())]);
    }
}

#[test]
fn an_unmaterialized_column_comes_back_as_the_same_buffer() {
    let _session = session();
    let mut table = frame(vec![("x", RVector::Double(vec![2.0, 4.0, 8.0]))]);
    let arrow = table.column_slice_f64("x").map(<[f64]>::as_ptr);
    let vector = expose(&mut table, "x");
    let series =
        unsafe { import_column("y".into(), vector) }.unwrap_or_else(|error| panic!("{error:?}"));
    let back = Table::new(
        table::polars::prelude::DataFrame::new(3, vec![series.into()])
            .unwrap_or_else(|error| panic!("{error}")),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(back.column_slice_f64("y").map(<[f64]>::as_ptr), arrow);
}

#[test]
fn a_write_through_a_writable_pointer_changes_the_vector_not_the_frame() {
    let _session = session();
    let mut table = frame(vec![("x", RVector::Integer(vec![1, 2, 3]))]);
    let vector = expose(&mut table, "x");
    unsafe {
        let pointer = dataptr(vector, true)
            .unwrap_or_else(|error| panic!("{error:?}"))
            .cast::<i32>();
        *pointer.add(1) = 20;
        assert_eq!(int_elt(vector, 1).ok(), Some(20));
        let series = import_column("x".into(), vector).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            series.i32().ok().and_then(|values| values.get(1)),
            Some(20),
            "importing reads R's copy"
        );
    }
    assert_eq!(
        table.column_slice_i32("x"),
        Some(&[1, 2, 3][..]),
        "the frame keeps its values"
    );
}

#[test]
fn a_plain_r_vector_imports_with_na_as_validity() {
    let _session = session();
    unsafe {
        let vector = libr::memory::alloc_vector(SexpType::Integer, 3)
            .unwrap_or_else(|error| panic!("{error:?}"));
        vector
            .slice_mut::<i32>()
            .copy_from_slice(&[5, NA_INTEGER, 6]);
        let series = import_column("v".into(), vector).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(series.null_count(), 1);
        assert_eq!(
            series
                .i32()
                .ok()
                .map(|values| values.iter().collect::<Vec<_>>()),
            Some(vec![Some(5), None, Some(6)])
        );
    }
}

#[test]
fn a_whole_frame_is_a_classed_list_of_columns() {
    let _session = session();
    let table = frame(vec![
        ("a", RVector::Integer(vec![1, 2])),
        ("b", RVector::Character(vec![Some("x".into()), None])),
    ]);
    let list = unsafe { expose_table(&table) }.unwrap_or_else(|error| panic!("{error:?}"));
    unsafe {
        assert_eq!(list.kind(), SexpType::List);
        assert_eq!(list.length(), 2);
        assert!(list.element(0).is_altrep());
        let names =
            get_attrib(list, R_NamesSymbol.get()).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            strings_of(names),
            [Some("a".to_string()), Some("b".to_string())]
        );
        let class =
            get_attrib(list, R_ClassSymbol.get()).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(
            strings_of(class),
            [Some("frame".to_string()), Some("data.frame".to_string())]
        );
    }
}
