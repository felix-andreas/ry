//! Peak memory per row of the reader, the joins and the grouped aggregates, on tables shaped like
//! the db-benchmark's. At 1e8 rows the benchmark's tables and one answer fill most of a 16 GB
//! machine, so a buffer that scales with the rows and outlives its use decides whether a question
//! runs at all. Timing does not show such a buffer, and at the sizes a test can afford nothing
//! runs out of memory, so every step runs here under an allocator that tracks the bytes
//! allocated, and its peak above the bytes held before it must stay within a pinned budget per
//! row.
//!
//! The budgets count allocated bytes, which includes reserved capacity a run never touches. They
//! are pinned 1 to 4 bytes per row above the measured peaks, because the pool's threads
//! interleave their allocations differently from run to run, so a smaller buffer can hide in
//! that margin. The test fixes the thread count, because the reader's peak grows with it. A step that goes over its budget either gained a
//! per-row buffer or kept one alive longer: fix that, and raise a budget only for memory the
//! answer itself needs.

use {
    std::{
        alloc::{GlobalAlloc, Layout, System},
        fmt::Write as _,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering::Relaxed},
    },
    table::{
        Table,
        io::{ReadOptions, read_csv_in_slices},
        lazy::{How, Plan, Source, Step, collect},
        polars::prelude::PlSmallStr,
        query::{
            Aggregate, BinaryOp, Expr, Query, Rows, Select,
            build::{aggregate, binary, column},
            execute,
        },
    },
};

struct Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(live, Relaxed);
}

fn freed(bytes: usize) {
    LIVE.fetch_sub(bytes, Relaxed);
}

// SAFETY: every method forwards to the system allocator unchanged.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // A growing block may move, and then the old and the new block exist together.
        allocated(size);
        // SAFETY: as above.
        let moved = unsafe { System.realloc(pointer, layout, size) };
        freed(layout.size());
        moved
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        freed(layout.size());
        // SAFETY: as above.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static GLOBAL: Tracking = Tracking;

/// The rows of the left-hand join table and of the grouped table. The file is read in
/// [`SLICE_BYTES`] slices, so it takes as many slices as the benchmark's 5 GB file does.
const ROWS: usize = 1 << 20;
const SLICE_BYTES: usize = 1 << 22;

/// One step's memory in bytes per row: the most it allocates above what was held when it
/// started, and what its result holds. A budget is the most a step may use, and a measurement is
/// what it used.
struct Budget {
    step: &'static str,
    peak: f64,
    held: f64,
}

const BUDGETS: &[Budget] = &[
    Budget {
        step: "read join_x",
        peak: 88.5,
        held: 33.0,
    },
    Budget {
        step: "read join_big",
        peak: 90.5,
        held: 34.0,
    },
    Budget {
        step: "join small on id1",
        peak: 37.0,
        held: 33.5,
    },
    Budget {
        step: "join medium on id2",
        peak: 42.5,
        held: 39.0,
    },
    Budget {
        step: "left join medium on id2",
        peak: 47.0,
        held: 43.0,
    },
    Budget {
        step: "join medium on id5",
        peak: 44.5,
        held: 40.5,
    },
    Budget {
        step: "join big on id3",
        peak: 49.5,
        held: 46.0,
    },
    Budget {
        step: "read groupby",
        peak: 82.0,
        held: 36.5,
    },
    Budget {
        step: "sum by id1",
        peak: 9.0,
        held: 0.5,
    },
    Budget {
        step: "sum by id3",
        peak: 28.0,
        held: 0.5,
    },
    Budget {
        step: "sum by id6",
        peak: 21.0,
        held: 0.5,
    },
    Budget {
        step: "sum and count by all six ids",
        peak: 55.0,
        held: 29.0,
    },
    Budget {
        step: "sum of every row",
        peak: 0.5,
        held: 0.5,
    },
    Budget {
        step: "add one to v1 where id4 is each value",
        peak: 5.0,
        held: 5.0,
    },
];

/// The threads every step runs on. A thread holds a slice of the file and its own scratch while
/// it works, which at this test's row count is a visible share of the peak, so the budgets hold
/// for one thread count.
const THREADS: &str = "4";

#[test]
fn peak_memory_per_row_stays_within_budget() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: this is the binary's only test, and it sets the variables before anything it calls
    // reads the environment or starts a thread pool.
    unsafe {
        std::env::set_var("POLARS_MAX_THREADS", THREADS);
        std::env::set_var("RAYON_NUM_THREADS", THREADS);
    }
    let dir = std::env::temp_dir().join(format!("ry-table-memory-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let outcome = measure_steps(&dir);
    std::fs::remove_dir_all(&dir)?;
    let measured = outcome?;

    let mut report = String::new();
    let mut over = Vec::new();
    for Budget { step, peak, held } in &measured {
        let budget = BUDGETS
            .iter()
            .find(|budget| budget.step == *step)
            .ok_or_else(|| format!("the step `{step}` has no budget"))?;
        writeln!(
            report,
            "{step:<30} peak {peak:>7.1} of {:>7.1}   held {held:>7.1} of {:>7.1}",
            budget.peak, budget.held
        )?;
        if *peak > budget.peak || *held > budget.held {
            over.push(*step);
        }
    }
    println!("bytes per row at {ROWS} rows\n{report}");
    assert!(
        over.is_empty(),
        "over the memory budget: {over:?}\nbytes per row at {ROWS} rows\n{report}"
    );
    Ok(())
}

/// What every step allocated at its peak and what its result holds, in bytes per row.
fn measure_steps(dir: &Path) -> Result<Vec<Budget>, Box<dyn std::error::Error>> {
    let small_rows = 10;
    let medium_rows = ROWS / 1000;
    let mut measured = Vec::new();
    let mut step = |name: &'static str, run: &mut dyn FnMut() -> table::Result<Table>| {
        let before = LIVE.load(Relaxed);
        PEAK.store(before, Relaxed);
        let result = run()?;
        let peak = PEAK.load(Relaxed).saturating_sub(before);
        let held = LIVE.load(Relaxed).saturating_sub(before);
        measured.push(Budget {
            step: name,
            peak: peak as f64 / ROWS as f64,
            held: held as f64 / ROWS as f64,
        });
        Ok::<Table, Box<dyn std::error::Error>>(result)
    };

    let x_path = write(dir, "join_x.csv", &join_x(small_rows, medium_rows))?;
    let small_path = write(dir, "join_small.csv", &join_right(small_rows, 1))?;
    let medium_path = write(dir, "join_medium.csv", &join_right(medium_rows, 2))?;
    let big_path = write(dir, "join_big.csv", &join_right(ROWS, 3))?;
    let x = step("read join_x", &mut || read(&x_path))?;
    let small = read(&small_path)?;
    let medium = read(&medium_path)?;
    let big = step("read join_big", &mut || read(&big_path))?;
    let mut join = |name, other: &Table, key: &str, how| {
        step(name, &mut || {
            let plan = Plan::new(Source::Table(x.clone())).with(Step::Join {
                other: Box::new(Plan::new(Source::Table(other.clone()))),
                left_on: vec![key.into()],
                right_on: vec![key.into()],
                how,
                suffix: "_right".into(),
            });
            Ok(collect(plan)?.table)
        })
        .map(drop)
    };
    join("join small on id1", &small, "id1", How::Inner)?;
    join("join medium on id2", &medium, "id2", How::Inner)?;
    join("left join medium on id2", &medium, "id2", How::Left)?;
    join("join medium on id5", &medium, "id5", How::Inner)?;
    join("join big on id3", &big, "id3", How::Inner)?;
    drop((x, small, medium, big));

    let groupby_path = write(dir, "groupby.csv", &groupby())?;
    let table = step("read groupby", &mut || read(&groupby_path))?;
    let sum = |name: &str| {
        (
            PlSmallStr::from(name),
            aggregate(Aggregate::Sum, column(name)),
        )
    };
    let mut grouped = |name, keys: &[&str], aggregates: Vec<(PlSmallStr, Expr)>| {
        step(name, &mut || {
            let plan = Plan::new(Source::Table(table.clone())).with(Step::Aggregate {
                keys: keys
                    .iter()
                    .map(|key| (PlSmallStr::from(*key), column(key)))
                    .collect(),
                aggregates: aggregates.clone(),
            });
            Ok(collect(plan)?.table)
        })
        .map(drop)
    };
    grouped("sum by id1", &["id1"], vec![sum("v1")])?;
    grouped("sum by id3", &["id3"], vec![sum("v1"), sum("v3")])?;
    grouped(
        "sum by id6",
        &["id6"],
        vec![sum("v1"), sum("v2"), sum("v3")],
    )?;
    grouped(
        "sum and count by all six ids",
        &["id1", "id2", "id3", "id4", "id5", "id6"],
        vec![sum("v3"), ("count".into(), Expr::Count)],
    )?;
    step("sum of every row", &mut || {
        let plan = Plan::new(Source::Table(table.clone())).with(Step::Select(vec![
            sum("id4"),
            sum("id6"),
            sum("v1"),
            sum("v3"),
        ]));
        Ok(collect(plan)?.table)
    })?;
    // The loop of the db-benchmark's "update subset" question. What it holds afterwards is the
    // row index of id4, and its peak is that index while it is built: the column it writes is
    // the table's own.
    let mut table = table;
    step("add one to v1 where id4 is each value", &mut || {
        for value in 1..=100 {
            execute(
                &mut table,
                Query {
                    rows: Rows::Filter(binary(
                        BinaryOp::Equal,
                        column("id4"),
                        Expr::Integer(value),
                    )),
                    select: Select::Assign(vec![(
                        "v1".into(),
                        Some(binary(BinaryOp::Add, column("v1"), Expr::Integer(1))),
                    )]),
                    ..Query::default()
                },
            )?;
        }
        Ok(table.fork())
    })?;
    Ok(measured)
}

fn read(path: &Path) -> table::Result<Table> {
    let options = ReadOptions {
        factors: true,
        ..ReadOptions::default()
    };
    read_csv_in_slices(path, &options, SLICE_BYTES)
}

fn write(dir: &Path, name: &str, text: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, text)?;
    Ok(path)
}

/// A fixed sequence of well-mixed numbers (SplitMix64), so every run measures the same tables.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A number from 1 to `count`.
    fn up_to(&mut self, count: usize) -> usize {
        1 + (self.next() % count as u64) as usize
    }

    /// A number below 100 with six decimals.
    fn value(&mut self) -> f64 {
        (self.next() % 100_000_000) as f64 / 1e6
    }
}

/// The benchmark's left-hand join table: three integer keys, the same keys as text, and a value.
fn join_x(small_rows: usize, medium_rows: usize) -> String {
    let mut sequence = Sequence(1);
    let mut text = String::from("id1,id2,id3,id4,id5,id6,v1\n");
    for _ in 0..ROWS {
        let (id1, id2, id3) = (
            sequence.up_to(small_rows),
            sequence.up_to(medium_rows),
            sequence.up_to(ROWS),
        );
        let value = sequence.value();
        text.push_str(&format!(
            "{id1},{id2},{id3},id{id1},id{id2},id{id3},{value}\n"
        ));
    }
    text
}

/// One of the benchmark's right-hand tables, with `keys` key columns of which the last is the
/// join key: nine in ten of its values are distinct keys of the left-hand table, and the tenth
/// lies just above them and matches nothing.
fn join_right(rows: usize, keys: usize) -> String {
    let mut sequence = Sequence(2 + keys as u64);
    let names = ["id1", "id2", "id3"];
    let mut text = String::new();
    for name in &names[..keys] {
        text.push_str(name);
        text.push(',');
    }
    for index in 4..4 + keys {
        text.push_str(&format!("id{index},"));
    }
    text.push_str("v2\n");
    let spaces = [10, ROWS / 1000, ROWS];
    for row in 0..rows {
        let mut ids = [0usize; 3];
        for (key, id) in ids[..keys].iter_mut().enumerate() {
            *id = sequence.up_to(spaces[key]);
        }
        // An odd multiplier walks a power-of-two key space without repeating, and other row
        // counts are too small for a repeated key to matter.
        ids[keys - 1] = if row % 10 == 9 {
            rows + sequence.up_to(rows)
        } else {
            1 + row.wrapping_mul(0x9E37_79B1) % rows
        };
        for id in &ids[..keys] {
            text.push_str(&format!("{id},"));
        }
        for id in &ids[..keys] {
            text.push_str(&format!("id{id},"));
        }
        text.push_str(&format!("{}\n", sequence.value()));
    }
    text
}

/// The benchmark's grouped table: three text keys, three integer keys and three values.
fn groupby() -> String {
    let mut sequence = Sequence(7);
    let mut text = String::from("id1,id2,id3,id4,id5,id6,v1,v2,v3\n");
    for _ in 0..ROWS {
        let line = format!(
            "id{:03},id{:03},id{:010},{},{},{},{},{},{}\n",
            sequence.up_to(100),
            sequence.up_to(100),
            sequence.up_to(ROWS / 100),
            sequence.up_to(100),
            sequence.up_to(100),
            sequence.up_to(ROWS / 100),
            sequence.up_to(5),
            sequence.up_to(15),
            sequence.value(),
        );
        text.push_str(&line);
    }
    text
}
