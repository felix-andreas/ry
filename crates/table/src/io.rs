//! Reading and writing delimited text, the engine behind `fread` and `fwrite`.
//!
//! The reader sniffs the file's layout with the `sniff` crate, which is the same code the checker
//! uses to type a literal `fread("prices.csv")`, so a file reads into exactly the columns the
//! checker promised. Polars then parses the file on all cores with that schema.
//!
//! A sample can miss a value that widens a column, such as a `2.5` deep in a column of whole
//! numbers. Polars then fails to parse it, and the reader parses the file again with every
//! column as text and infers each column from all of its values. The sniffed schema is right for
//! almost every file, so the second pass is rare.
//!
//! The writer follows `fwrite`: `TRUE` and `FALSE`, an empty field for `NA`, a factor as its
//! labels, and quotes only around a field that needs them, which includes the empty string so it
//! reads back as `""` rather than `NA`. A double prints in the shortest form that reads back as
//! the same value, so a numeric column reads back unchanged. A string column can come back
//! different: the string "NA" and a column of digit strings are unquoted, so they read back as
//! `NA` and as numbers.

use {
    crate::{
        Error, Result, Table,
        factor::{self, Interner},
    },
    memmap2::Mmap,
    polars::prelude::{
        BooleanChunked, ChunkedArray, Column, CsvEncoding, CsvReadOptions, DataFrame, DataType,
        Field, Float64Chunked, Float64Type, Int32Chunked, Int32Type, IntoColumn, NullValues,
        PlSmallStr, PolarsDataType, Schema as PolarsSchema, SerReader, Series,
    },
    polars_arrow::{
        array::{Array, BooleanArray, PrimitiveArray, Utf8ViewArray},
        bitmap::{Bitmap, MutableBitmap},
        datatypes::ArrowDataType,
        types::NativeType,
    },
    rayon::prelude::*,
    sniff::{ColumnType as TextColumn, Guess, Layout},
    std::{
        cell::RefCell,
        fs::File,
        io::{BufWriter, Cursor, Write},
        ops::Range,
        path::Path,
        sync::Arc,
    },
};

/// Reads a delimited text file into a table, as `fread(path)` does.
pub fn read_csv(path: &Path) -> Result<Table> {
    read_csv_with(path, &ReadOptions::default())
}

/// What part of a file a reader needs: the lazy planner pushes a query's projection and a leading
/// `head` into the reader, so the columns and rows nobody reads are never parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// The columns to read, in the file's order whatever order they are listed in; `None` reads
    /// every column.
    pub columns: Option<Vec<PlSmallStr>>,
    /// The number of data rows to read; `None` reads them all.
    pub rows: Option<usize>,
    /// Reads every column sniffed as character as a factor, as `fread(stringsAsFactors = TRUE)`
    /// does.
    pub factors: bool,
}

/// Reads part of a delimited text file into a table.
pub fn read_csv_with(path: &Path, options: &ReadOptions) -> Result<Table> {
    read_file(path, options, SLICE_BYTES).map(|(table, _)| table)
}

fn read_file(path: &Path, options: &ReadOptions, slice_bytes: usize) -> Result<(Table, Pass)> {
    let file = File::open(path)?;
    // SAFETY: the map is read-only and dropped before this returns. Another process truncating
    // the file meanwhile is undefined behavior, the same trade every memory-mapped reader,
    // including `fread`, makes for not copying the file.
    let map = unsafe { Mmap::map(&file)? };
    // A parsed slice's pages are dropped, so a large file is never resident all at once.
    read_bytes(&map, options, slice_bytes, &|range: Range<usize>| {
        release_pages(&map, range)
    })
}

/// Drops the pages behind `range` of a read-only file mapping from memory. The parsed columns
/// own their data, so a later read of the range faults the file's bytes back in rather than
/// seeing zeros.
#[cfg(unix)]
fn release_pages(map: &Mmap, range: Range<usize>) -> std::io::Result<()> {
    use memmap2::UncheckedAdvice;
    // SAFETY: `MADV_DONTNEED` on a read-only file mapping discards only clean pages, which the
    // file backs.
    unsafe { map.unchecked_advise_range(UncheckedAdvice::DontNeed, range.start, range.len()) }
}

/// Windows has no `madvise`. Unlocking pages that were never locked takes them out of the
/// working set instead, which Windows reports as a failure with `ERROR_NOT_LOCKED`.
#[cfg(windows)]
fn release_pages(map: &Mmap, range: Range<usize>) -> std::io::Result<()> {
    use {
        std::io::{Error, ErrorKind},
        windows_sys::Win32::{Foundation::ERROR_NOT_LOCKED, System::Memory::VirtualUnlock},
    };
    let bytes = map
        .get(range)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "range outside the mapped file"))?;
    if bytes.is_empty() {
        return Ok(());
    }
    // SAFETY: the pointer and length describe bytes of the live mapping.
    if unsafe { VirtualUnlock(bytes.as_ptr().cast(), bytes.len()) } != 0 {
        return Ok(());
    }
    let error = Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NOT_LOCKED as i32) {
        Ok(())
    } else {
        Err(error)
    }
}

/// The columns and their types a file reads as, from its sniffed layout, without parsing it.
pub fn csv_schema(path: &Path) -> Result<crate::Schema> {
    let file = File::open(path)?;
    // SAFETY: as in `read_csv_with`.
    let map = unsafe { Mmap::map(&file)? };
    let layout = sniff::sniff(map.as_ref());
    Ok(crate::Schema::new(layout.columns.iter().map(|column| {
        let kind = match column.kind {
            TextColumn::Logical => crate::ColumnType::Logical,
            TextColumn::Integer => crate::ColumnType::Integer,
            TextColumn::Double => crate::ColumnType::Double,
            _ => crate::ColumnType::Character,
        };
        (column.name.as_str(), kind)
    })))
}

/// Reads delimited text held in memory, as `fread(text = ...)` does.
pub fn read_csv_bytes<B: AsRef<[u8]> + Send + Sync>(bytes: B) -> Result<Table> {
    read_bytes(
        bytes.as_ref(),
        &ReadOptions::default(),
        SLICE_BYTES,
        &|_| Ok(()),
    )
    .map(|(table, _)| table)
}

/// `check_csv_input` on fuzzer bytes, which the coverage-guided `fuzz/` target passes straight
/// through. The first four bytes choose the read's options and slice size, and the rest is the
/// text. Bit 0 of the flags reads factors, bit 1 reads only the first rows, bit 2 reads the
/// sniffed columns the mask selects, bit 3 lists them backwards, and bit 4 adds a column the
/// text lacks.
pub fn check_csv_bytes(data: &[u8]) {
    let [flags, slice, rows, mask, text @ ..] = data else {
        check_csv_input(data, &ReadOptions::default(), 16);
        return;
    };
    let columns = (flags & 4 != 0).then(|| {
        let mut names: Vec<PlSmallStr> = sniff::sniff(text)
            .columns
            .into_iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << (index % 8)) != 0)
            .map(|(_, column)| column.name.as_str().into())
            .collect();
        if flags & 8 != 0 {
            names.reverse();
        }
        if flags & 16 != 0 {
            names.push("absent".into());
        }
        names
    });
    let options = ReadOptions {
        columns,
        rows: (flags & 2 != 0).then_some(usize::from(*rows)),
        factors: flags & 1 != 0,
    };
    check_csv_input(text, &options, 1 + usize::from(*slice));
}

/// The reader's contract on any bytes, read with `options` in slices of about `slice_bytes`.
/// Nothing panics, and the bytes either fail to read or read into the columns the sniffer
/// promised, in its order. Each number and character column keeps its sniffed type unless a value
/// the sample missed made the reader retype the file. A read in slices takes the same pass as a
/// whole read and gives the same table, so a slicer bug cannot hide behind the retyping read, and
/// it releases every byte exactly once, in order. A read with options gives what a default read
/// gives once its columns are selected, its rows cut and its strings made factors, and a table
/// that reads also writes. Shared with the coverage-guided `fuzz/` target.
pub fn check_csv_input(bytes: &[u8], options: &ReadOptions, slice_bytes: usize) {
    let what = format!(
        "{:?} with {options:?} in slices of {slice_bytes} bytes",
        String::from_utf8_lossy(bytes)
    );
    let released = RefCell::new(Vec::new());
    let sliced = read_bytes(bytes, options, slice_bytes.max(1), &|range| {
        released.borrow_mut().push(range);
        Ok(())
    });
    let whole = read_bytes(bytes, options, SLICE_BYTES, &|_| Ok(()));
    let Some((table, pass)) = same_read(whole, sliced, &what) else {
        return;
    };
    if pass == Pass::Sniffed {
        let released = released.into_inner();
        let mut end = 0;
        for range in &released {
            assert_eq!(
                range.start, end,
                "the released ranges {released:?} do not tile {what}"
            );
            end = range.end;
        }
        assert_eq!(
            end,
            bytes.len(),
            "the released ranges {released:?} do not cover {what}"
        );
    }

    let layout = sniff::sniff(bytes);
    let promised: Vec<&sniff::Column> = layout
        .columns
        .iter()
        .filter(|column| {
            options
                .columns
                .as_ref()
                .is_none_or(|names| names.iter().any(|name| name == column.name.as_str()))
        })
        .collect();
    let read = table.schema().columns;
    let read_names: Vec<&str> = read.iter().map(|(name, _)| name.as_str()).collect();
    let promised_names: Vec<&str> = promised.iter().map(|column| column.name.as_str()).collect();
    assert_eq!(
        read_names, promised_names,
        "the columns read differ from the sniffed ones for {what}"
    );
    if pass == Pass::Sniffed {
        for (column, (name, found)) in promised.iter().zip(&read) {
            let expected = match column.kind {
                TextColumn::Integer => crate::ColumnType::Integer,
                TextColumn::Double => crate::ColumnType::Double,
                TextColumn::Character if options.factors => crate::ColumnType::Factor,
                TextColumn::Character => crate::ColumnType::Character,
                // A logical column is typed from all of its values, so a value the sample missed
                // widens it without failing the parse.
                TextColumn::Logical => continue,
            };
            assert_eq!(*found, expected, "column {name} of {what}");
        }
    }

    if *options != ReadOptions::default() {
        match read_bytes(bytes, &ReadOptions::default(), SLICE_BYTES, &|_| Ok(())) {
            // A retyping read of the first rows types its columns from those rows alone, and a
            // read that retypes only one of the two sees different values.
            Ok((full, full_pass))
                if full_pass == pass && (pass == Pass::Sniffed || options.rows.is_none()) =>
            {
                let mut frame = full
                    .frame()
                    .select(read_names.iter().copied())
                    .unwrap_or_else(|error| panic!("cannot select the columns of {what}: {error}"));
                if let Some(rows) = options.rows {
                    frame = frame.head(Some(rows));
                }
                let mut expected = Table::new(frame)
                    .unwrap_or_else(|error| panic!("cannot rebuild the table of {what}: {error}"));
                if options.factors
                    && let Err(error) = strings_as_factors(&mut expected)
                {
                    panic!("cannot make factors of the default read of {what}: {error}");
                }
                assert_eq!(table.height(), expected.height(), "the rows of {what}");
                assert!(
                    table.frame().equals_missing(expected.frame()),
                    "{what} reads differently from a default read cut to the same options:\n{}\n{}",
                    table.frame(),
                    expected.frame()
                );
            }
            Ok(_) => {}
            Err(error) => assert!(
                options.columns.is_some() || options.rows.is_some(),
                "{what} reads, but not with the default options: {error}"
            ),
        }
    }

    let mut written = Vec::new();
    if let Err(error) = write_csv_to(&table, &mut written) {
        panic!("a table read from {what} does not write: {error}");
    }
}

/// A file read through its memory map in slices of about `slice_bytes`, each slice's pages
/// released once parsed, gives the table its bytes give read whole from memory.
pub fn check_csv_file(path: &Path, options: &ReadOptions, slice_bytes: usize) {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let what = format!(
        "the file {:?} with {options:?} in slices of {slice_bytes} bytes",
        String::from_utf8_lossy(&bytes)
    );
    same_read(
        read_bytes(&bytes, options, SLICE_BYTES, &|_| Ok(())),
        read_file(path, options, slice_bytes.max(1)),
        &what,
    );
}

/// The whole read's table and pass once a sliced read agrees with it, or `None` when both fail.
fn same_read(
    whole: Result<(Table, Pass)>,
    sliced: Result<(Table, Pass)>,
    what: &str,
) -> Option<(Table, Pass)> {
    match (whole, sliced) {
        (Ok((whole, whole_pass)), Ok((sliced, sliced_pass))) => {
            assert_eq!(
                sliced_pass, whole_pass,
                "a read in slices takes a different pass from a whole read of {what}"
            );
            assert!(
                sliced.frame().equals_missing(whole.frame()),
                "a read in slices differs from a whole read of {what}:\n{}\n{}",
                sliced.frame(),
                whole.frame()
            );
            Some((whole, whole_pass))
        }
        (Err(_), Err(_)) => None,
        (whole, sliced) => panic!(
            "{what} reads whole as {:?} but in slices as {:?}",
            whole.map(|(table, _)| table.schema().to_string()),
            sliced.map(|(table, _)| table.schema().to_string())
        ),
    }
}

/// Files are parsed in slices of about this many bytes, each released once parsed, so reading a
/// file never holds more than one slice of its text and the columns it has become.
const SLICE_BYTES: usize = 1 << 28;

/// Which parse a read's table came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// Every value parsed with the sniffed types.
    Sniffed,
    /// A value the sniffer's sample missed failed the typed parse, so the file was read again
    /// with every column as text.
    Retyped,
}

/// Reads `bytes` as a table, calling `release` with each byte range once it has been parsed.
fn read_bytes(
    bytes: &[u8],
    options: &ReadOptions,
    slice_bytes: usize,
    release: &dyn Fn(Range<usize>) -> std::io::Result<()>,
) -> Result<(Table, Pass)> {
    let layout = sniff::sniff(bytes);
    if layout.columns.is_empty() {
        return Ok((Table::default(), Pass::Sniffed));
    }
    if let Some(columns) = &options.columns
        && let Some(missing) = columns.iter().find(|name| {
            !layout
                .columns
                .iter()
                .any(|column| column.name == name.as_str())
        })
    {
        return Err(Error::NoColumn(missing.to_string()));
    }
    if let (Some([]), Some(first)) = (options.columns.as_deref(), layout.columns.first()) {
        // Polars cannot parse no columns at all, so the first one is read to count the rows.
        let first = ReadOptions {
            columns: Some(vec![PlSmallStr::from(first.name.as_str())]),
            rows: options.rows,
            factors: false,
        };
        let (table, pass) = read_bytes(bytes, &first, slice_bytes, release)?;
        return Ok((
            Table::new(DataFrame::new(table.height(), Vec::new())?)?,
            pass,
        ));
    }
    let mut rows = rows_range(bytes, &layout);
    let malformed = !quotes_well_formed(&bytes[rows.clone()], layout.separator);
    if malformed && let Some(opening) = unclosed_quote(&bytes[rows.clone()], layout.separator) {
        // Polars reads past its buffer on a field that is a lone opening quote at the end.
        return Err(Error::UnclosedQuote(rows.start + opening));
    }
    if let (Some(count), false) = (options.rows, malformed) {
        // Polars stops parsing chunks once the rows it has counted so far reach a leading `head`,
        // which depends on how fast its threads run, so only the text of those rows is parsed.
        // Badly quoted text has rows only Polars can find, so it is parsed whole and cut after.
        rows.end = first_rows_end(bytes, rows.clone(), count, layout.separator);
    }
    let (frame, pass) = match read_sliced(
        bytes,
        &layout,
        &rows,
        malformed,
        options,
        slice_bytes,
        release,
    ) {
        Ok(frame) => (frame, Pass::Sniffed),
        // A value the sniffer's sample missed fails the typed parse, so the file is read again
        // with every column as text, each then typed from all of its values.
        Err(Error::Polars(_)) => {
            let schema = schema(&layout, true);
            let frame = parse(&bytes[rows.clone()], &layout, options, &schema)?;
            let height = frame.height();
            let columns: Vec<Column> = frame
                .into_columns()
                .into_par_iter()
                .map(|column| type_text_column(column, None))
                .collect::<Result<_>>()?;
            (DataFrame::new(height, columns)?, Pass::Retyped)
        }
        Err(error) => return Err(error),
    };
    let mut table = Table::new(frame)?;
    if options.factors {
        strings_as_factors(&mut table)?;
    }
    Ok((table, pass))
}

/// Parses the file slice by slice with the sniffed types, appending each slice's columns to the
/// table's final column buffers before the next slice is parsed, so a typed column is never held
/// as parsed pieces and a copy of them. A text column keeps Polars' pieces and joins them at the
/// end, which copies its strings once. A factor's column numbers each slice's strings into one
/// [`Interner`] and drops them, so the file's strings never exist as a whole column.
fn read_sliced(
    bytes: &[u8],
    layout: &Layout,
    rows: &Range<usize>,
    malformed: bool,
    options: &ReadOptions,
    slice_bytes: usize,
    release: &dyn Fn(Range<usize>) -> std::io::Result<()>,
) -> Result<DataFrame> {
    let schema = schema(layout, false);
    let start = rows.start;
    release(0..start)?;
    // One range spanning the rows is the intended value, not a mistyped `(0..n).collect()`.
    #[allow(clippy::single_range_in_vec_init)]
    let ranges = if malformed {
        // Polars opens a quoted field at any quote and ends it at the next one, where the slicer
        // follows the CSV grammar, so the two would split badly quoted text at different rows.
        vec![rows.clone()]
    } else {
        slices(&bytes[rows.clone()], slice_bytes, layout.separator)
            .into_iter()
            .map(|range| range.start + start..range.end + start)
            .collect()
    };
    let mut builders: Vec<(PlSmallStr, Builder)> = Vec::new();
    let mut height = 0;
    for (index, range) in ranges.iter().enumerate() {
        let frame = parse(&bytes[range.clone()], layout, options, &schema)?;
        if index == 0 {
            // The rows still to come are estimated from the first slice's bytes per row over
            // the bytes the read covers, which `nrows` may cut short. A column reservation the
            // file does not fill is never touched, so it costs no memory, but a factor's hash
            // tables are written in full, so the estimate must not count bytes past the read.
            let rows = frame.height() * rows.len() / range.len().max(1);
            let reserve = rows + rows / 8 + 1024;
            builders = frame
                .columns()
                .iter()
                .map(|column| {
                    let factor = options.factors
                        && layout.columns.iter().any(|sniffed| {
                            sniffed.name == column.name().as_str()
                                && sniffed.kind == TextColumn::Character
                        });
                    (
                        column.name().clone(),
                        Builder::new(column.dtype(), factor, reserve),
                    )
                })
                .collect();
        }
        height += frame.height();
        for ((_, builder), column) in builders.iter_mut().zip(frame.into_columns()) {
            builder.append(column)?;
        }
        release(range.clone())?;
    }
    release(rows.end..bytes.len())?;
    let columns: Vec<Column> = builders
        .into_par_iter()
        .map(|(name, builder)| builder.finish(name, layout))
        .collect::<Result<_>>()?;
    Ok(DataFrame::new(height, columns)?)
}

/// One column's final buffer while a file is read slice by slice. A validity bitmap is made only
/// once a missing value turns up.
enum Builder {
    Integer(Vec<i32>, Option<MutableBitmap>),
    Double(Vec<f64>, Option<MutableBitmap>),
    /// A factor's strings, numbered as each slice arrives.
    Factor(Box<Interner>),
    /// Text, kept as Polars' chunks until the column is typed from all of its values.
    Text(Option<Column>),
}

impl Builder {
    fn new(dtype: &DataType, factor: bool, rows: usize) -> Builder {
        match dtype {
            DataType::Int32 => Builder::Integer(Vec::with_capacity(rows), None),
            DataType::Float64 => Builder::Double(Vec::with_capacity(rows), None),
            DataType::String if factor => Builder::Factor(Box::new(Interner::new(rows))),
            _ => Builder::Text(None),
        }
    }

    fn append(&mut self, column: Column) -> Result<()> {
        match self {
            Builder::Integer(values, validity) => {
                for array in column.i32()?.downcast_iter() {
                    extend(values, validity, array);
                }
            }
            Builder::Double(values, validity) => {
                for array in column.f64()?.downcast_iter() {
                    extend(values, validity, array);
                }
            }
            Builder::Factor(interner) => {
                for array in column.str()?.downcast_iter() {
                    interner.intern(array)?;
                }
            }
            Builder::Text(text) => match text {
                Some(text) => {
                    text.append_owned(column)?;
                }
                None => *text = Some(column),
            },
        }
        Ok(())
    }

    fn finish(self, name: PlSmallStr, layout: &Layout) -> Result<Column> {
        Ok(match self {
            Builder::Integer(values, validity) => Int32Chunked::with_chunk(
                name,
                PrimitiveArray::new(ArrowDataType::Int32, values.into(), bitmap(validity)),
            )
            .into_column(),
            Builder::Double(values, validity) => Float64Chunked::with_chunk(
                name,
                PrimitiveArray::new(ArrowDataType::Float64, values.into(), bitmap(validity)),
            )
            .into_column(),
            Builder::Factor(interner) => interner.finish(name).into_column(),
            Builder::Text(text) => {
                let column = match text {
                    Some(text) => text.rechunk(),
                    None => Column::new_empty(name, &DataType::String),
                };
                let declared = layout
                    .columns
                    .iter()
                    .find(|sniffed| sniffed.name == column.name().as_str())
                    .map(|sniffed| sniffed.kind);
                type_text_column(column, declared)?
            }
        })
    }
}

/// Appends an array's values and validity to a column's buffers.
fn extend<T: NativeType>(
    values: &mut Vec<T>,
    validity: &mut Option<MutableBitmap>,
    array: &PrimitiveArray<T>,
) {
    let before = values.len();
    values.extend_from_slice(array.values());
    match (validity.as_mut(), array.validity()) {
        (Some(bits), Some(valid)) => bits.extend_from_bitmap(valid),
        (Some(bits), None) => bits.extend_constant(array.len(), true),
        (None, Some(valid)) if valid.unset_bits() > 0 => {
            let mut bits = MutableBitmap::with_capacity(values.capacity());
            bits.extend_constant(before, true);
            bits.extend_from_bitmap(valid);
            *validity = Some(bits);
        }
        (None, _) => {}
    }
}

fn bitmap(validity: Option<MutableBitmap>) -> Option<Bitmap> {
    validity.and_then(|bits| bits.into())
}

/// Row-aligned byte ranges of about `slice_bytes` each, covering `bytes`, whose quotes are well
/// formed. A range ends just after a line break outside quotes.
fn slices(bytes: &[u8], slice_bytes: usize, separator: u8) -> Vec<Range<usize>> {
    let Some(starts) = stretch_starts(bytes, slice_bytes, separator) else {
        // One range spanning the file is the intended value, not a mistyped `(0..n).collect()`.
        #[allow(clippy::single_range_in_vec_init)]
        return vec![0..bytes.len()];
    };
    let mut ranges = Vec::with_capacity(starts.len());
    let mut start = 0;
    for (stretch, state) in starts.iter().enumerate().skip(1) {
        let boundary = stretch * slice_bytes;
        if boundary <= start {
            continue;
        }
        let mut scan = *state;
        let Some(end) = (boundary..bytes.len()).find(|position| {
            scan = quoting_step(bytes, *position, scan, separator);
            bytes[*position] == b'\n' && scan == Quoting::Outside
        }) else {
            break;
        };
        ranges.push(start..end + 1);
        start = end + 1;
    }
    if start < bytes.len() || ranges.is_empty() {
        ranges.push(start..bytes.len());
    }
    ranges
}

/// Whether every quote follows the CSV grammar: a quote outside a quoted field opens one at a
/// field's start only, a closing quote is followed by a separator, a line break or another quote,
/// and no quoted field is left open at the end. Polars' parser reads any other quote differently
/// from the slicer, and differently from one thread count to the next.
fn quotes_well_formed(bytes: &[u8], separator: u8) -> bool {
    !bytes.contains(&sniff::QUOTE) || stretch_starts(bytes, 1 << 20, separator).is_some()
}

/// Where the `count`th record of the well-quoted `rows` ends, after its line break.
fn first_rows_end(bytes: &[u8], rows: Range<usize>, count: usize, separator: u8) -> usize {
    if count == 0 {
        return rows.start;
    }
    let mut state = Quoting::Outside;
    let mut records = 0;
    rows.clone()
        .find(|position| {
            state = quoting_step(bytes, *position, state, separator);
            records += usize::from(bytes[*position] == b'\n' && state == Quoting::Outside);
            records == count
        })
        .map_or(rows.end, |end| end + 1)
}

/// Where the quoted field left open at the end of `bytes` opens, if one is.
fn unclosed_quote(bytes: &[u8], separator: u8) -> Option<usize> {
    let mut state = Quoting::Outside;
    let mut opening = 0;
    for position in 0..bytes.len() {
        let next = quoting_step(bytes, position, state, separator);
        if state == Quoting::Outside && next == Quoting::Inside {
            opening = position;
        }
        state = next;
    }
    (state == Quoting::Inside).then_some(opening)
}

/// The quoting at the start of each stretch of `stretch_bytes`, or `None` when the quotes are not
/// well formed. Every stretch is tracked from each of the three states in parallel, and the
/// results are chained.
fn stretch_starts(bytes: &[u8], stretch_bytes: usize, separator: u8) -> Option<Vec<Quoting>> {
    let stretches = bytes.len().div_ceil(stretch_bytes).max(1);
    let ends: Vec<[Option<Quoting>; 3]> = (0..stretches)
        .into_par_iter()
        .map(|stretch| {
            let start = stretch * stretch_bytes;
            let range = start..(start + stretch_bytes).min(bytes.len());
            Quoting::ALL.map(|state| quoting_after(bytes, range.clone(), state, separator))
        })
        .collect();
    let mut starts = Vec::with_capacity(stretches);
    let mut state = Quoting::Outside;
    for ends in &ends {
        starts.push(state);
        state = ends[state as usize]?;
    }
    (state != Quoting::Inside).then_some(starts)
}

/// Where a byte stands in the CSV quoting grammar. A quote opens a quoted field only at a field's
/// start, so a stray quote inside an unquoted value, such as `5" pipe`, is text, and a doubled
/// quote inside a quoted field is an escaped quote.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    Outside = 0,
    Inside = 1,
    /// Just after a quote that may close the field or, doubled, escape a quote.
    JustClosed = 2,
}

impl Quoting {
    const ALL: [Quoting; 3] = [Quoting::Outside, Quoting::Inside, Quoting::JustClosed];
}

/// The quoting after the bytes in `range`, from `state` at its start, or `None` when a quote in
/// them breaks the grammar. A stretch without a quote leaves a quoted field open and an unquoted
/// one closed, so only stretches with quotes are walked byte by byte.
fn quoting_after(
    bytes: &[u8],
    range: Range<usize>,
    state: Quoting,
    separator: u8,
) -> Option<Quoting> {
    if !bytes[range.clone()].contains(&sniff::QUOTE) {
        return match (state, range.clone().next()) {
            (Quoting::JustClosed, Some(first)) => {
                ends_field(bytes[first], separator).then_some(Quoting::Outside)
            }
            (state, _) => Some(state),
        };
    }
    range.into_iter().try_fold(state, |state, position| {
        let byte = bytes[position];
        if state == Quoting::JustClosed && byte != sniff::QUOTE && !ends_field(byte, separator) {
            return None;
        }
        let next = quoting_step(bytes, position, state, separator);
        let stray = state == Quoting::Outside && next == Quoting::Outside && byte == sniff::QUOTE;
        (!stray).then_some(next)
    })
}

fn ends_field(byte: u8, separator: u8) -> bool {
    matches!(byte, b'\n' | b'\r') || byte == separator
}

fn quoting_step(bytes: &[u8], position: usize, state: Quoting, separator: u8) -> Quoting {
    let quote = bytes[position] == sniff::QUOTE;
    match (state, quote) {
        (Quoting::Inside, true) => Quoting::JustClosed,
        (Quoting::Inside, false) => Quoting::Inside,
        (Quoting::JustClosed, true) => Quoting::Inside,
        (Quoting::Outside | Quoting::JustClosed, false) => Quoting::Outside,
        (Quoting::Outside, true) => {
            let field_start = position.checked_sub(1).is_none_or(|before| {
                matches!(bytes[before], b'\n' | b'\r') || bytes[before] == separator
            });
            if field_start {
                Quoting::Inside
            } else {
                Quoting::Outside
            }
        }
    }
}

/// Writes a table as comma-separated text, as `fwrite(x, path)` does.
pub fn write_csv(table: &Table, path: &Path) -> Result<()> {
    let mut file = BufWriter::with_capacity(1 << 20, File::create(path)?);
    write_csv_to(table, &mut file)?;
    file.flush()?;
    Ok(())
}

/// How many rows one thread formats at a time. Large enough that a batch dwarfs the cost of
/// handing it out, small enough that a round of batches stays a few megabytes per thread.
const WRITE_BATCH_ROWS: usize = 1 << 15;

/// Writes a table as comma-separated text to any writer. Batches of rows are formatted on all
/// cores and written in order.
pub fn write_csv_to(table: &Table, writer: &mut impl Write) -> Result<()> {
    let frame = table.frame();
    // A factor is written as its labels, so it is cast to text first; every other column is
    // written straight from its Arrow buffers.
    let mut text_columns: Vec<Column> = Vec::new();
    let mut columns: Vec<Column> = Vec::with_capacity(frame.width());
    for column in frame.columns() {
        let column = match column.dtype() {
            dtype if factor::is_factor(dtype) => {
                let text = factor::text(column)?;
                text_columns.push(text.clone());
                text
            }
            _ => column.clone(),
        };
        columns.push(column.rechunk());
    }
    let mut cells = Vec::with_capacity(columns.len());
    for column in &columns {
        cells.push(Cells::of(column)?);
    }
    let mut header = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            header.push(b',');
        }
        write_text(&mut header, column.name());
    }
    header.push(b'\n');
    writer.write_all(&header)?;

    let height = frame.height();
    let batches: Vec<usize> = (0..height).step_by(WRITE_BATCH_ROWS).collect();
    let round = rayon::current_num_threads().max(1) * 2;
    for starts in batches.chunks(round) {
        let buffers: Vec<Vec<u8>> = starts
            .par_iter()
            .map(|start| {
                let end = (start + WRITE_BATCH_ROWS).min(height);
                let mut buffer = Vec::with_capacity((end - start) * cells.len() * 8);
                for row in *start..end {
                    for (index, cell) in cells.iter().enumerate() {
                        if index > 0 {
                            buffer.push(b',');
                        }
                        cell.write(&mut buffer, row);
                    }
                    buffer.push(b'\n');
                }
                buffer
            })
            .collect();
        for buffer in buffers {
            writer.write_all(&buffer)?;
        }
    }
    drop(text_columns);
    Ok(())
}

/// The Polars schema to parse with. With `as_text`, every column reads as text and is then
/// typed from all of its values, which is the fallback for a value the sample missed.
fn schema(layout: &Layout, as_text: bool) -> Arc<PolarsSchema> {
    let mut schema = PolarsSchema::with_capacity(layout.columns.len());
    for column in &layout.columns {
        let dtype = match (as_text, column.kind) {
            (false, TextColumn::Integer) => DataType::Int32,
            (false, TextColumn::Double) => DataType::Float64,
            // Polars reads only `true` and `false` as booleans, so a logical column reads as
            // text and is converted with `fread`'s three spellings.
            _ => DataType::String,
        };
        schema.insert(PlSmallStr::from(column.name.as_str()), dtype);
    }
    Arc::new(schema)
}

/// The bytes that hold the rows: past the blank lines and the header record the sniffer named the
/// columns from, and before the blank lines that end the file, which Polars would read as rows of
/// missing values. Polars never parses the header itself, because it opens a quoted field at a
/// quote in the middle of a field, where the sniffer and the slicer read the quote as text, and so
/// it would end the header record somewhere else and lose or gain rows.
fn rows_range(bytes: &[u8], layout: &Layout) -> Range<usize> {
    let blank = |line: &[u8]| line.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r'));
    let mut start = 0;
    if layout.header {
        while let Some(rest) = bytes.get(start..).filter(|rest| !rest.is_empty()) {
            let line_end = rest.iter().position(|byte| *byte == b'\n');
            if !blank(&rest[..line_end.unwrap_or(rest.len())]) {
                break;
            }
            start += line_end.map_or(rest.len(), |end| end + 1);
        }
        let mut state = Quoting::Outside;
        start = (start..bytes.len())
            .find(|position| {
                state = quoting_step(bytes, *position, state, layout.separator);
                bytes[*position] == b'\n' && state == Quoting::Outside
            })
            .map_or(bytes.len(), |end| end + 1);
    }
    let mut end = bytes.len();
    while end > start {
        let rows = &bytes[start..end];
        let content = rows.strip_suffix(b"\n").unwrap_or(rows);
        let line_start = content
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        if !blank(&content[line_start..]) {
            break;
        }
        end = start + line_start;
    }
    start..end
}

/// Parses rows of text, without a header, with `schema`, leaving logical and undecided columns as
/// Polars read them.
fn parse(
    bytes: &[u8],
    layout: &Layout,
    options: &ReadOptions,
    schema: &Arc<PolarsSchema>,
) -> Result<DataFrame> {
    // Polars rejects empty text, such as the rows of a file that is only a header.
    if bytes.is_empty() {
        let projected: PolarsSchema = schema
            .iter()
            .filter(|(name, _)| {
                options
                    .columns
                    .as_ref()
                    .is_none_or(|columns| columns.contains(*name))
            })
            .map(|(name, dtype)| Field::new(name.clone(), dtype.clone()))
            .collect();
        return Ok(DataFrame::empty_with_schema(&projected));
    }
    let frame = CsvReadOptions::default()
        .with_has_header(false)
        .with_schema(Some(schema.clone()))
        .with_rechunk(false)
        .with_columns(
            options
                .columns
                .as_ref()
                .map(|columns| columns.iter().cloned().collect()),
        )
        .map_parse_options(|options| {
            options
                .with_separator(layout.separator)
                .with_quote_char(Some(sniff::QUOTE))
                .with_encoding(CsvEncoding::LossyUtf8)
                .with_null_values(Some(NullValues::AllColumnsSingle(PlSmallStr::from("NA"))))
                // An empty field is `NA` in a typed column, which Polars gives regardless, and
                // the empty string in a character column, which this keeps.
                .with_missing_is_null(false)
                // Polars drops the extra fields of a long row, but rejects the text when its first
                // row is long, so without this a read in slices would fail where a slice begins
                // with one.
                .with_truncate_ragged_lines(true)
        })
        .into_reader_with_file_handle(Cursor::new(bytes))
        .finish()?;
    Ok(match options.rows {
        Some(rows) => frame.slice(0, rows),
        None => frame,
    })
}

/// Types a column Polars read as text. A column sniffed as character stays text. Any other
/// column is typed from all of its values, with an empty field counted as `NA` unless the
/// column turns out to be character after all.
fn type_text_column(column: Column, declared: Option<TextColumn>) -> Result<Column> {
    if column.dtype() != &DataType::String || declared == Some(TextColumn::Character) {
        return Ok(column);
    }
    let strings = column.str()?;
    let guess = strings
        .downcast_iter()
        .map(|array| {
            array
                .iter()
                .flatten()
                .fold(Guess::Missing, |guess, field| guess.join(Guess::of(field)))
        })
        .fold(Guess::Missing, Guess::join);
    let name = column.name().clone();
    Ok(match guess.column_type() {
        TextColumn::Character => column,
        TextColumn::Logical => {
            let values: BooleanChunked = strings
                .iter()
                .map(|field| field.and_then(|field| sniff::parse_logical(trim(field))))
                .collect();
            values.with_name(name).into_column()
        }
        TextColumn::Integer => {
            let values: ChunkedArray<Int32Type> = strings
                .iter()
                .map(|field| field.and_then(|field| sniff::parse_integer(trim(field))))
                .collect();
            values.with_name(name).into_column()
        }
        TextColumn::Double => {
            let values: ChunkedArray<Float64Type> = strings
                .iter()
                .map(|field| field.and_then(|field| sniff::parse_double(trim(field))))
                .collect();
            values.with_name(name).into_column()
        }
    })
}

fn trim(field: &str) -> &str {
    field.trim_matches([' ', '\t'])
}

/// One column's Arrow buffer, read by row while a batch is formatted.
enum Cells<'a> {
    Logical(&'a BooleanArray),
    Integer(&'a PrimitiveArray<i32>),
    Double(&'a PrimitiveArray<f64>),
    Text(&'a Utf8ViewArray),
    Empty,
}

impl<'a> Cells<'a> {
    fn of(column: &'a Column) -> Result<Cells<'a>> {
        fn single<T: PolarsDataType>(
            chunked: &ChunkedArray<T>,
        ) -> Option<&<T as PolarsDataType>::Array> {
            chunked.downcast_iter().next()
        }
        let series = column.as_materialized_series();
        let cells = match series.dtype() {
            DataType::Boolean => single(series.bool()?).map(Cells::Logical),
            DataType::Int32 => single(series.i32()?).map(Cells::Integer),
            DataType::Float64 => single(series.f64()?).map(Cells::Double),
            DataType::String => single(series.str()?).map(Cells::Text),
            other => {
                return Err(Error::UnsupportedType {
                    name: column.name().to_string(),
                    found: other.to_string(),
                });
            }
        };
        Ok(cells.unwrap_or(Cells::Empty))
    }

    fn write(&self, buffer: &mut Vec<u8>, row: usize) {
        match self {
            Cells::Logical(array) => {
                if array.is_valid(row) {
                    buffer.extend_from_slice(if array.value(row) { b"TRUE" } else { b"FALSE" });
                }
            }
            Cells::Integer(array) => {
                if array.is_valid(row) {
                    buffer
                        .extend_from_slice(itoa::Buffer::new().format(array.value(row)).as_bytes());
                }
            }
            Cells::Double(array) => {
                if array.is_valid(row) {
                    write_double(buffer, array.value(row));
                }
            }
            Cells::Text(array) => {
                if array.is_valid(row) {
                    write_text(buffer, array.value(row));
                }
            }
            Cells::Empty => {}
        }
    }
}

/// Writes a double in the shortest form that reads back as the same value, with R's spellings
/// for the special values and without a `.0` on a whole number, as `fwrite` writes `1`.
pub(crate) fn write_double(buffer: &mut Vec<u8>, value: f64) {
    if value.is_nan() {
        buffer.extend_from_slice(b"NaN");
    } else if value.is_infinite() {
        buffer.extend_from_slice(if value > 0.0 { b"Inf" } else { b"-Inf" });
    } else {
        let mut formatter = ryu::Buffer::new();
        let text = formatter.format_finite(value);
        buffer.extend_from_slice(text.strip_suffix(".0").unwrap_or(text).as_bytes());
    }
}

/// Writes a text field, quoted when it holds a separator, quote or line break, or when it is
/// empty, so that it cannot read back as `NA`.
fn write_text(buffer: &mut Vec<u8>, text: &str) {
    let needs_quotes = text.is_empty()
        || text
            .bytes()
            .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'));
    if !needs_quotes {
        buffer.extend_from_slice(text.as_bytes());
        return;
    }
    buffer.push(b'"');
    for byte in text.bytes() {
        if byte == b'"' {
            buffer.push(b'"');
        }
        buffer.push(byte);
    }
    buffer.push(b'"');
}

/// Reads a column sniffed as character as a factor, as `fread(stringsAsFactors = TRUE)` does.
/// The columns convert in parallel, since each one is a separate hash-and-sort.
pub fn strings_as_factors(table: &mut Table) -> Result<()> {
    let text: Vec<&Column> = table
        .frame()
        .columns()
        .iter()
        .filter(|column| column.dtype() == &DataType::String)
        .collect();
    let factors = text
        .par_iter()
        .map(|column| factor::from_strings(column.as_materialized_series()))
        .collect::<Result<Vec<Series>>>()?;
    for factor in factors {
        table.put_column(factor.into_column())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::{Pass, Quoting, ReadOptions, SLICE_BYTES, quoting_after, read_bytes, slices},
        crate::Result,
    };

    /// Quoted fields with line breaks and doubled quotes straddle many slice boundaries. With
    /// `stray`, some unquoted values also hold a quote, which only the slicer handles.
    fn text(stray: bool) -> String {
        let mut text = String::from("id,name,note,x\n");
        for row in 0..400 {
            let note = match row % 4 {
                0 => "\"line\nbreak\"".to_string(),
                1 => "\"say \"\"hi\"\", ok\"".to_string(),
                2 => String::new(),
                // A quote inside an unquoted value is text, not the start of a quoted field.
                _ if stray && row % 8 == 3 => format!("{row}\" pipe"),
                _ => format!("n{row}"),
            };
            text.push_str(&format!("{row},k{},{note},{}.5\n", row % 7, row));
        }
        text
    }

    #[test]
    fn slices_end_at_line_breaks_outside_quotes() {
        let stray = text(true);
        assert!(stray.matches("\" pipe").count() > 0);
        for size in [1, 7, 64, 1000] {
            // One range spanning the file is the expected value, not a mistyped `(0..n).collect()`.
            #[allow(clippy::single_range_in_vec_init)]
            let whole = [0..stray.len()];
            assert_eq!(slices(stray.as_bytes(), size, b','), whole);
        }
        let text = text(false);
        let bytes = text.as_bytes();
        for size in [1, 7, 64, 1000] {
            let ranges = slices(bytes, size, b',');
            assert_eq!(ranges.first().map(|range| range.start), Some(0));
            assert_eq!(ranges.last().map(|range| range.end), Some(bytes.len()));
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
                let before = &bytes[..pair[0].end];
                assert_eq!(before.last(), Some(&b'\n'));
                let state = quoting_after(bytes, 0..pair[0].end, Quoting::Outside, b',');
                assert!(
                    state == Some(Quoting::Outside),
                    "a slice of {size} bytes ends inside quotes"
                );
            }
        }
    }

    #[test]
    fn a_sliced_read_matches_a_whole_read() -> Result<()> {
        // Polars cannot read a stray quote at all, sliced or whole, so it is left out here.
        let text = text(false);
        let bytes = text.as_bytes();
        for factors in [false, true] {
            let options = ReadOptions {
                factors,
                ..ReadOptions::default()
            };
            let (whole, _) = read_bytes(bytes, &options, SLICE_BYTES, &|_| Ok(()))?;
            let (sliced, pass) = read_bytes(bytes, &options, 97, &|_| Ok(()))?;
            // A failed sliced read falls back to retyping the whole file, which would hide it.
            assert_eq!(pass, Pass::Sniffed, "factors: {factors}");
            assert_eq!(whole.frame().height(), 400);
            assert!(
                whole.frame().equals_missing(sliced.frame()),
                "factors: {factors}"
            );
        }
        Ok(())
    }

    fn read_sliced_and_whole(text: &str, options: &ReadOptions, slice_bytes: usize) -> [String; 2] {
        let bytes = text.as_bytes();
        [slice_bytes, SLICE_BYTES].map(|size| match read_bytes(bytes, options, size, &|_| Ok(())) {
            Ok((table, pass)) => format!("{pass:?} {:?}", table.frame()),
            Err(error) => format!("error: {error}"),
        })
    }

    /// Polars reads a file's trailing blank lines as rows of missing values, which a read in
    /// slices skipped. Input: `"a,b\n1,x\n2,y\n\n\n"`.
    #[test]
    fn trailing_blank_lines_hold_no_rows() -> Result<()> {
        let (table, _) = read_bytes(
            b"a,b\n1,x\n2,y\n\n\n",
            &ReadOptions::default(),
            SLICE_BYTES,
            &|_| Ok(()),
        )?;
        assert_eq!(table.height(), 2);
        Ok(())
    }

    /// A blank line inside the rows is a row of missing values however the slices fall. A slice
    /// of only blank lines used to be skipped. Input: `"c0\n1\n\n\n2\n"` in slices of 1 byte.
    #[test]
    fn a_blank_line_is_a_row_in_any_slice() {
        let [sliced, whole] = read_sliced_and_whole("c0\n1\n\n\n2\n", &ReadOptions::default(), 1);
        assert_eq!(sliced, whole);
        assert!(whole.contains("(4, 1)"), "{whole}");
    }

    /// Polars opens a quoted field at a stray quote, so text with one reads whole rather than
    /// in slices that would split its rows elsewhere. A slice holding only `break",10.3` read,
    /// where the whole text fails. Input: `"c0,c1\nbreak\",10.3\n\"line\nbreak\",69.6\n"`.
    #[test]
    fn text_with_a_stray_quote_reads_the_same_in_slices() {
        let text = "c0,c1\nbreak\",10.3\n\"line\nbreak\",69.6\n";
        let [sliced, whole] = read_sliced_and_whole(text, &ReadOptions::default(), 6);
        assert_eq!(sliced, whole);
    }

    /// Polars rejects text whose first row has more fields than the schema but drops the extras
    /// of any later row, so a slice that began with a long row failed and the read silently
    /// retyped the whole file. Input: `"c0,c1,c2\n1,2.5,3\n78,,0.3t-489,77.4,32.2\n4,5.5,6\n"`
    /// in slices of 8 bytes.
    #[test]
    fn a_slice_that_begins_with_a_long_row_reads() {
        let text = "c0,c1,c2\n1,2.5,3\n78,,0.3t-489,77.4,32.2\n4,5.5,6\n";
        let [sliced, whole] = read_sliced_and_whole(text, &ReadOptions::default(), 8);
        assert_eq!(sliced, whole);
        assert!(whole.starts_with("Sniffed"), "{whole}");
    }

    /// Polars parses an unterminated quoted field into factor codes but rejects it as text, so a
    /// read with factors used to succeed where the same read without them fails. Input:
    /// `"a\tb\n1\t\"q\u{1c}\nq!\n2\t3\n"` with `factors: true`.
    #[test]
    fn a_malformed_quote_fails_a_read_with_factors_too() {
        let text = "a\tb\n1\t\"q\u{1c}\nq!\n2\t3\n";
        let factors = ReadOptions {
            factors: true,
            ..ReadOptions::default()
        };
        let [with_factors, _] = read_sliced_and_whole(text, &factors, SLICE_BYTES);
        let [without, _] = read_sliced_and_whole(text, &ReadOptions::default(), SLICE_BYTES);
        assert!(
            with_factors.starts_with("error") && without.starts_with("error"),
            "{with_factors}"
        );
    }

    /// Polars reads past the end of its buffer on a field that is a lone opening quote at the end
    /// of the text, which aborted the process. Input: `"a,b\n1,\""`.
    #[test]
    fn a_quote_left_open_at_the_end_is_an_error() {
        for text in ["a,b\n1,\"", "a,b\n1,\"x\n", "a,b\n\"\n2,3\n"] {
            for factors in [false, true] {
                let options = ReadOptions {
                    factors,
                    ..ReadOptions::default()
                };
                let [sliced, whole] = read_sliced_and_whole(text, &options, 3);
                assert_eq!(sliced, whole);
                assert!(whole.contains("is never closed"), "{text:?}: {whole}");
            }
        }
    }

    /// Polars stops parsing chunks for a leading `head` once its threads have counted enough
    /// rows, so whether it reached a badly quoted field further on depended on timing, and one
    /// read failed where the next succeeded. Input (found by `fuzz_csv_reading`, seed 532): rows
    /// like `1.5,"a, b",T` with one `"line\nbreak,2.5,T` left to close at a later quote, read
    /// with `rows: Some(9)` and factors.
    #[test]
    fn badly_quoted_text_reads_the_same_on_every_run() {
        let mut text = String::from("c0,c1,c2\n");
        for row in 0..4000 {
            let note = match row {
                2000 => "\"line\nbreak",
                _ if row % 3 == 0 => "\"say \"\"hi\"\"\"",
                _ => "\"a, b\"",
            };
            text.push_str(&format!("{row}.5,{note},T\n"));
        }
        let options = ReadOptions {
            rows: Some(9),
            factors: true,
            ..ReadOptions::default()
        };
        let first = read_sliced_and_whole(&text, &options, 180);
        for _ in 0..20 {
            assert_eq!(read_sliced_and_whole(&text, &options, 180), first);
        }
    }
}
