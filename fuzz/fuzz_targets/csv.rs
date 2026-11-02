#![no_main]

use libfuzzer_sys::fuzz_target;

// The first four bytes choose the read's options and slice size, and the rest is the text. Bit 0
// of the flags reads factors, bit 1 reads only the first rows, bit 2 reads the sniffed columns
// the mask selects, bit 3 lists them backwards, and bit 4 adds a column the text lacks.
fuzz_target!(|data: &[u8]| {
    let [flags, slice, rows, mask, text @ ..] = data else {
        table::io::check_csv_input(data, &table::io::ReadOptions::default(), 16);
        return;
    };
    let columns = (flags & 4 != 0).then(|| {
        let mut names: Vec<table::polars::prelude::PlSmallStr> = sniff::sniff(text)
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
    let options = table::io::ReadOptions {
        columns,
        rows: (flags & 2 != 0).then_some(usize::from(*rows)),
        factors: flags & 1 != 0,
    };
    table::io::check_csv_input(text, &options, 1 + usize::from(*slice));
});
