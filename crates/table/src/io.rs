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
        convert::{categorical_to_factor, string_to_factor},
    },
    memmap2::{Mmap, UncheckedAdvice},
    polars::prelude::{
        BooleanChunked, CategoricalPhysical, Categories, ChunkedArray, Column, CsvEncoding,
        CsvReadOptions, DataFrame, DataType, Float64Type, Int32Type, IntoColumn, NullValues,
        PlSmallStr, PolarsDataType, Schema as PolarsSchema, SerReader, Series,
    },
    polars_arrow::array::{Array, BooleanArray, PrimitiveArray, Utf8ViewArray},
    rayon::prelude::*,
    sniff::{ColumnType as TextColumn, Guess, Layout},
    std::{
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
    let file = File::open(path)?;
    // SAFETY: the map is read-only and dropped before this returns. Another process truncating
    // the file meanwhile is undefined behavior, the same trade every memory-mapped reader,
    // including `fread`, makes for not copying the file.
    let map = unsafe { Mmap::map(&file)? };
    read_bytes(&map, options, SLICE_BYTES, &|range: Range<usize>| {
        // A parsed slice's pages are dropped, so a large file is never resident all at once.
        // SAFETY: the map is a read-only file mapping and the parsed columns own their data, so
        // a later read of the range faults the file's bytes back in rather than seeing zeros.
        unsafe { map.unchecked_advise_range(UncheckedAdvice::DontNeed, range.start, range.len()) }
    })
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
}

/// Any bytes either read into a table whose columns are the ones the sniffer promised, in its
/// order, or fail with an error; a table that reads also writes. Nothing panics. Shared with the
/// coverage-guided `fuzz/` target.
pub fn check_csv_input(bytes: &[u8]) {
    let Ok(table) = read_csv_bytes(bytes) else {
        return;
    };
    let promised: Vec<String> = sniff::sniff(bytes)
        .columns
        .into_iter()
        .map(|column| column.name)
        .collect();
    let read: Vec<String> = table
        .schema()
        .columns
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    assert_eq!(
        read, promised,
        "the columns read differ from the sniffed ones for {bytes:?}"
    );
    let mut written = Vec::new();
    if let Err(error) = write_csv_to(&table, &mut written) {
        panic!("a table read from {bytes:?} does not write: {error}");
    }
}

/// Files are parsed in slices of about this many bytes, each released once parsed, so reading a
/// file never holds more than one slice of its text and the columns it has become.
const SLICE_BYTES: usize = 1 << 28;

/// Reads `bytes` as a table, calling `release` with each byte range once it has been parsed.
fn read_bytes(
    bytes: &[u8],
    options: &ReadOptions,
    slice_bytes: usize,
    release: &dyn Fn(Range<usize>) -> std::io::Result<()>,
) -> Result<Table> {
    let layout = sniff::sniff(bytes);
    if layout.columns.is_empty() {
        return Ok(Table::default());
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
    let mut frame = match read_sliced(bytes, &layout, options, slice_bytes, release) {
        Ok(frame) => frame,
        Err(_) => {
            let schema = schema(&layout, options, true);
            type_columns(
                parse(bytes, &layout, options, &schema, layout.header)?,
                &layout,
                true,
            )?
        }
    };
    let undecided: Vec<PlSmallStr> = frame
        .columns()
        .iter()
        .filter(|column| matches!(column.dtype(), DataType::Categorical(..)))
        .map(|column| column.name().clone())
        .collect();
    if !undecided.is_empty() {
        // A categorical column reads an empty field as `NA`, where `fread` reads the empty
        // string, so a column with a missing value is read again as text to tell the two apart.
        let text = ReadOptions {
            columns: Some(undecided),
            rows: options.rows,
            factors: false,
        };
        let schema = schema(&layout, &text, false);
        for column in parse(bytes, &layout, &text, &schema, layout.header)?.into_columns() {
            frame.with_column(column)?;
        }
    }
    let mut table = Table::new(frame)?;
    if options.factors {
        strings_as_factors(&mut table)?;
    }
    Ok(table)
}

/// Parses the file slice by slice with the sniffed types, then joins each column's pieces, one
/// column at a time so that only one column is ever held twice. A factor's column numbers its
/// strings with one mapping across the slices, so the codes of every slice agree.
fn read_sliced(
    bytes: &[u8],
    layout: &Layout,
    options: &ReadOptions,
    slice_bytes: usize,
    release: &dyn Fn(Range<usize>) -> std::io::Result<()>,
) -> Result<DataFrame> {
    let schema = schema(layout, options, false);
    let ranges = match options.rows {
        // A leading `head` reads only the first rows, so it parses the file whole.
        Some(_) => vec![0..bytes.len()],
        None => slices(bytes, slice_bytes),
    };
    let mut pieces: Vec<std::vec::IntoIter<Column>> = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.into_iter().enumerate() {
        let header = index == 0 && layout.header;
        let frame = parse(&bytes[range.clone()], layout, options, &schema, header)?;
        pieces.push(frame.into_columns().into_iter());
        release(range)?;
    }
    let width = pieces.first().map_or(0, ExactSizeIterator::len);
    let mut columns: Vec<Column> = Vec::with_capacity(width);
    for _ in 0..width {
        let mut parts = pieces.iter_mut().filter_map(Iterator::next);
        let Some(mut column) = parts.next() else {
            break;
        };
        for part in parts {
            column.append_owned(part)?;
        }
        columns.push(column.rechunk());
    }
    let height = columns.first().map_or(0, Column::len);
    type_columns(DataFrame::new(height, columns)?, layout, false)
}

/// Row-aligned byte ranges of about `slice_bytes` each, covering `bytes`. A range ends just
/// after a line break outside quotes, which is a line break preceded by an even number of quote
/// characters, since an escaped quote is written twice. The quotes are counted in parallel, one
/// stretch of the file per task.
fn slices(bytes: &[u8], slice_bytes: usize) -> Vec<Range<usize>> {
    let stretches = bytes.len().div_ceil(slice_bytes).max(1);
    if stretches == 1 {
        return vec![0..bytes.len()];
    }
    let odd: Vec<bool> = (0..stretches)
        .into_par_iter()
        .map(|stretch| {
            let start = stretch * slice_bytes;
            let end = (start + slice_bytes).min(bytes.len());
            bytes[start..end]
                .iter()
                .filter(|byte| **byte == sniff::QUOTE)
                .count()
                % 2
                == 1
        })
        .collect();
    let mut ranges = Vec::with_capacity(stretches);
    let mut start = 0;
    let mut quoted = false;
    for (stretch, odd) in odd.iter().enumerate().take(stretches - 1) {
        // `quoted` is the quote parity at the start of the next stretch.
        quoted ^= odd;
        let boundary = (stretch + 1) * slice_bytes;
        if boundary <= start {
            continue;
        }
        let mut inside = quoted;
        let Some(offset) = bytes[boundary..].iter().position(|byte| {
            if *byte == sniff::QUOTE {
                inside = !inside;
            }
            *byte == b'\n' && !inside
        }) else {
            break;
        };
        let end = boundary + offset + 1;
        ranges.push(start..end);
        start = end;
    }
    if start < bytes.len() || ranges.is_empty() {
        ranges.push(start..bytes.len());
    }
    ranges
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
            DataType::Enum(..) | DataType::Categorical(..) => {
                let text = column.cast(&DataType::String)?;
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
fn schema(layout: &Layout, options: &ReadOptions, as_text: bool) -> Arc<PolarsSchema> {
    let mut schema = PolarsSchema::with_capacity(layout.columns.len());
    for column in &layout.columns {
        let dtype = match (as_text, column.kind) {
            (false, TextColumn::Integer) => DataType::Int32,
            (false, TextColumn::Double) => DataType::Float64,
            // A factor's column parses straight into codes, so its strings never exist row by
            // row. Each column numbers its own strings.
            (false, TextColumn::Character) if options.factors => DataType::from_categories(
                Categories::random(PlSmallStr::from_static("ry"), CategoricalPhysical::U32),
            ),
            // Polars reads only `true` and `false` as booleans, so a logical column reads as
            // text and is converted with `fread`'s three spellings.
            _ => DataType::String,
        };
        schema.insert(PlSmallStr::from(column.name.as_str()), dtype);
    }
    Arc::new(schema)
}

/// Parses the text with `schema`, leaving logical and undecided columns as Polars read them.
fn parse(
    bytes: &[u8],
    layout: &Layout,
    options: &ReadOptions,
    schema: &Arc<PolarsSchema>,
    header: bool,
) -> Result<DataFrame> {
    Ok(CsvReadOptions::default()
        .with_has_header(header)
        .with_schema(Some(schema.clone()))
        .with_rechunk(false)
        .with_columns(
            options
                .columns
                .as_ref()
                .map(|columns| columns.iter().cloned().collect()),
        )
        .with_n_rows(options.rows)
        .map_parse_options(|options| {
            options
                .with_separator(layout.separator)
                .with_quote_char(Some(sniff::QUOTE))
                .with_encoding(CsvEncoding::LossyUtf8)
                .with_null_values(Some(NullValues::AllColumnsSingle(PlSmallStr::from("NA"))))
                // An empty field is `NA` in a typed column, which Polars gives regardless, and
                // the empty string in a character column, which this keeps.
                .with_missing_is_null(false)
        })
        .into_reader_with_file_handle(Cursor::new(bytes))
        .finish()?)
}

/// Gives each parsed column its R type: a factor read without a missing value becomes a
/// factor, and a column read as text is typed from its values.
fn type_columns(frame: DataFrame, layout: &Layout, as_text: bool) -> Result<DataFrame> {
    let height = frame.height();
    let columns: Vec<Column> = frame
        .into_columns()
        .into_par_iter()
        .map(|column| {
            let sniffed = layout
                .columns
                .iter()
                .find(|sniffed| sniffed.name == column.name().as_str());
            let declared = if as_text {
                None
            } else {
                sniffed.map(|sniffed| sniffed.kind)
            };
            match column.dtype() {
                DataType::Categorical(..) if column.null_count() == 0 => {
                    Ok(categorical_to_factor(column.as_materialized_series())?.into_column())
                }
                _ => type_text_column(column, declared),
            }
        })
        .collect::<Result<_>>()?;
    Ok(DataFrame::new(height, columns)?)
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
        .map(|column| string_to_factor(column.as_materialized_series()))
        .collect::<Result<Vec<Series>>>()?;
    for factor in factors {
        table.put_column(factor.into_column())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::{ReadOptions, SLICE_BYTES, read_bytes, read_sliced, slices},
        crate::Result,
    };

    /// Quoted fields with line breaks and doubled quotes straddle many slice boundaries.
    fn text() -> String {
        let mut text = String::from("id,name,note,x\n");
        for row in 0..400 {
            let note = match row % 4 {
                0 => "\"line\nbreak\"".to_string(),
                1 => "\"say \"\"hi\"\", ok\"".to_string(),
                2 => String::new(),
                _ => format!("n{row}"),
            };
            text.push_str(&format!("{row},k{},{note},{}.5\n", row % 7, row));
        }
        text
    }

    #[test]
    fn slices_end_at_line_breaks_outside_quotes() {
        let text = text();
        let bytes = text.as_bytes();
        for size in [1, 7, 64, 1000] {
            let ranges = slices(bytes, size);
            assert_eq!(ranges.first().map(|range| range.start), Some(0));
            assert_eq!(ranges.last().map(|range| range.end), Some(bytes.len()));
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
                let before = &bytes[..pair[0].end];
                assert_eq!(before.last(), Some(&b'\n'));
                let quotes = before.iter().filter(|byte| **byte == sniff::QUOTE).count();
                assert_eq!(quotes % 2, 0, "a slice of {size} bytes ends inside quotes");
            }
        }
    }

    #[test]
    fn a_sliced_read_matches_a_whole_read() -> Result<()> {
        let text = text();
        let bytes = text.as_bytes();
        for factors in [false, true] {
            let options = ReadOptions {
                factors,
                ..ReadOptions::default()
            };
            let whole = read_bytes(bytes, &options, SLICE_BYTES, &|_| Ok(()))?;
            // A failed sliced read falls back to a whole one, so it is checked on its own.
            read_sliced(bytes, &sniff::sniff(bytes), &options, 97, &|_| Ok(()))?;
            let sliced = read_bytes(bytes, &options, 97, &|_| Ok(()))?;
            assert_eq!(whole.frame().height(), 400);
            assert!(
                whole.frame().equals_missing(sliced.frame()),
                "factors: {factors}"
            );
        }
        Ok(())
    }
}
