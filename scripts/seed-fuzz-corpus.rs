#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Seeds the fuzz corpora with every fixture case source and shipped stub.
//! Run `cargo +nightly -Zscript scripts/seed-fuzz-corpus.rs` before `cargo fuzz run`.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const FUZZ_TARGETS: [&str; 3] = ["parse", "format", "semantics"];

fn main() -> Result<(), Box<dyn Error>> {
    let repo_root = repo_root()?;
    for target in FUZZ_TARGETS {
        fs::create_dir_all(repo_root.join("fuzz/corpus").join(target))?;
    }

    let mut sources: Vec<String> = Vec::new();
    let crates_root = repo_root.join("crates");
    for path in files_with_suffix(&crates_root, ".test")? {
        let text = read_text_lossy(&path)?;
        // A fixture case's source is the lines between its `#----` header and
        // the `#++++` expectation marker.
        let mut case: Vec<&str> = Vec::new();
        let mut collecting = false;
        for line in split_lines(&text) {
            if line.starts_with("#----") {
                collecting = true;
                case.clear();
            } else if line.starts_with("#++++") {
                if !case.is_empty() {
                    sources.push(case.join("\n") + "\n");
                }
                collecting = false;
            } else if collecting {
                case.push(line);
            }
        }
    }
    for path in files_with_suffix(&crates_root, ".Rtypes")? {
        sources.push(read_text_lossy(&path)?);
    }

    for target in FUZZ_TARGETS {
        let corpus_dir = repo_root.join("fuzz/corpus").join(target);
        for source in &sources {
            fs::write(corpus_dir.join(sha1_hex(source.as_bytes())), source)?;
        }
    }
    println!(
        "seeded {} case sources into fuzz/corpus/{{parse,format,semantics}}",
        sources.len()
    );
    Ok(())
}

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    let script_path = std::env::args().next().ok_or("cannot determine the script path")?;
    let script_dir = Path::new(&script_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    Ok(fs::canonicalize(script_dir.join(".."))?)
}

fn files_with_suffix(root: &Path, suffix: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name().to_string_lossy().ends_with(suffix) {
                found.push(entry.path());
            }
        }
    }
    Ok(found)
}

fn read_text_lossy(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8_lossy(&fs::read(path)?).into_owned())
}

fn split_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                lines.push(&text[start..index]);
                index += 1;
                start = index;
            }
            b'\r' => {
                lines.push(&text[start..index]);
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn sha1_hex(data: &[u8]) -> String {
    let mut state: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let bit_length = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut schedule = [0u32; 80];
        for (word_index, word) in block.chunks_exact(4).enumerate() {
            schedule[word_index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..80 {
            schedule[index] = (schedule[index - 3]
                ^ schedule[index - 8]
                ^ schedule[index - 14]
                ^ schedule[index - 16])
                .rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, &word) in schedule.iter().enumerate() {
            let (mix, constant) = match index {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(mix)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut hex = String::with_capacity(40);
    for word in state {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}
