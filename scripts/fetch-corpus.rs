#!/usr/bin/env -S cargo +nightly -Zscript
---
[package]
edition = "2024"
---

//! Rebuilds the gitignored corpus/ and scripts/corpus-manifest.txt; run with `cargo +nightly
//! -Zscript scripts/fetch-corpus.rs`. tree-sitter-r comes through jsDelivr because GitHub may be
//! unreachable; shelling out to `curl` and `tar` honors the proxy environment.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const TSR_REF: &str = "main";

/// Minimum writable space (KiB) to keep before adding another CRAN package.
const MIN_FREE_KB: u64 = 2_000_000;

const CRAN_PACKAGES: &[&str] = &[
    "data.table", "dplyr", "ggplot2", "rlang", "purrr", "tibble", "stringr", "tidyr", "shiny",
    "jsonlite", "curl", "R6", "cli", "glue", "magrittr", "lubridate", "readr", "forcats",
    "scales", "testthat", "knitr", "withr", "fs", "processx", "callr", "crayon", "digest",
    "htmltools", "httpuv", "mime", "rmarkdown", "xfun", "yaml", "Rcpp", "stringi", "vctrs",
    "pillar", "lifecycle", "generics", "backports", "checkmate", "Matrix", "MASS", "mgcv",
    "survival", "lattice", "nlme", "Hmisc", "caret", "plotly", "sf", "raster", "zoo", "xts",
    "forecast", "devtools", "usethis", "roxygen2", "pkgdown", "httr", "httr2", "xml2", "rvest",
    "dbplyr", "DBI", "RSQLite", "broom", "future", "R.utils",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let script_path = std::env::args().next().ok_or("cannot determine the script path")?;
    let script_dir = fs::canonicalize(
        Path::new(&script_path)
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    )?;
    let repo_root = script_dir.join("..");
    let corpus_dir = fs::canonicalize(&repo_root)?.join("corpus");
    let manifest_path = script_dir.join("corpus-manifest.txt");
    let work_dir = WorkDir::create()?;
    let work = work_dir.path();

    println!("==> corpus root: {}", corpus_dir.display());

    println!("==> [1/3] tree-sitter-r corpus (ref: {TSR_REF})");
    let tsr_dir = corpus_dir.join("tree-sitter-r");
    remove_dir_all_if_exists(&tsr_dir)?;
    fs::create_dir_all(&tsr_dir)?;
    let listing_path = work.join("tsr-listing.json");
    fetch_required(
        &format!("https://data.jsdelivr.com/v1/packages/gh/r-lib/tree-sitter-r@{TSR_REF}?structure=flat"),
        &listing_path,
    )?;
    let listing = String::from_utf8_lossy(&fs::read(&listing_path)?).into_owned();
    let mut tsr_files: Vec<String> = Vec::new();
    for corpus_path in json_name_values(&listing)?
        .iter()
        .filter(|name| name.starts_with("/test/corpus/"))
    {
        let base = corpus_path.rsplit('/').next().unwrap_or(corpus_path).to_string();
        fetch_required(
            &format!("https://cdn.jsdelivr.net/gh/r-lib/tree-sitter-r@{TSR_REF}{corpus_path}"),
            &tsr_dir.join(&base),
        )?;
        println!("    fetched {base}");
        tsr_files.push(base);
    }

    println!("==> [2/3] R base library sources");
    let r_base_dir = corpus_dir.join("r-base");
    remove_dir_all_if_exists(&r_base_dir)?;
    fs::create_dir_all(&r_base_dir)?;
    let index_path = work.join("r-index.html");
    fetch_required("https://cran.r-project.org/src/base/R-4/", &index_path)?;
    let index_html = String::from_utf8_lossy(&fs::read(&index_path)?).into_owned();
    let r_version =
        newest_r4_version(&index_html).ok_or("no R-4.x.y.tar.gz link in the CRAN index")?;
    let r_tarball = format!("R-{r_version}.tar.gz");
    println!("    newest: {r_tarball}");
    let r_tar = work.join(&r_tarball);
    fetch_required(&format!("https://cran.r-project.org/src/base/R-4/{r_tarball}"), &r_tar)?;
    let members: Vec<String> = tar_list(&r_tar)?
        .into_iter()
        .filter(|member| r_base_member(member).is_some())
        .collect();
    let extract_dir = work.join("r-extract");
    fs::create_dir_all(&extract_dir)?;
    tar_extract(&r_tar, &extract_dir, &members, &work.join("r-members.txt"))?;
    for member in &members {
        let Some((library, file)) = r_base_member(member) else {
            continue;
        };
        let library_dir = r_base_dir.join(library);
        fs::create_dir_all(&library_dir)?;
        fs::copy(extract_dir.join(member), library_dir.join(file))?;
    }
    fs::remove_file(&r_tar)?;
    let r_base_libraries = count_directories(&r_base_dir)?;
    let r_base_files = count_files(&r_base_dir, |name| name.ends_with(".R"))?;
    println!("    extracted {r_base_files} R files across {r_base_libraries} libraries");

    println!("==> [3/3] CRAN package sources");
    let cran_dir = corpus_dir.join("cran");
    remove_dir_all_if_exists(&cran_dir)?;
    fs::create_dir_all(&cran_dir)?;
    let packages_path = work.join("PACKAGES");
    fetch_required("https://cran.r-project.org/src/contrib/PACKAGES", &packages_path)?;
    let packages_index = String::from_utf8_lossy(&fs::read(&packages_path)?).into_owned();

    let mut cran_results: Vec<(&str, String, &str)> = Vec::new();
    for &package in CRAN_PACKAGES {
        if let Some(free_kb) = available_kb(&corpus_dir)
            && free_kb < MIN_FREE_KB
        {
            println!("    SKIP {package} (low disk: {free_kb} KiB free)");
            cran_results.push((package, "-".to_string(), "skipped-low-disk"));
            continue;
        }
        let Some(version) =
            resolve_version(&packages_index, package).filter(|version| !version.is_empty())
        else {
            println!("    MISS {package} (not in PACKAGES)");
            cran_results.push((package, "-".to_string(), "not-found"));
            continue;
        };
        let tarball = format!("{package}_{version}.tar.gz");
        let package_tar = work.join(&tarball);
        if !fetch(&format!("https://cran.r-project.org/src/contrib/{tarball}"), &package_tar)? {
            println!("    MISS {package} ({tarball} 404)");
            cran_results.push((package, version, "download-failed"));
            continue;
        }
        // R CMD install accepts .R/.r/.S/.s/.q sources (mgcv ships .r, Hmisc .s).
        let members: Vec<String> = tar_list(&package_tar)?
            .into_iter()
            .filter(|member| is_cran_member(member, package))
            .collect();
        if members.is_empty() {
            println!("    note {package} has no R/*.R files");
            fs::remove_file(&package_tar)?;
            cran_results.push((package, version, "no-R-files"));
            continue;
        }
        let package_extract = work.join("pkg-extract");
        remove_dir_all_if_exists(&package_extract)?;
        fs::create_dir_all(&package_extract)?;
        tar_extract(&package_tar, &package_extract, &members, &work.join("pkg-members.txt"))?;
        let package_dir = cran_dir.join(package);
        fs::create_dir_all(&package_dir)?;
        for member in &members {
            let file = member.rsplit('/').next().unwrap_or(member);
            fs::copy(package_extract.join(member), package_dir.join(file))?;
        }
        fs::remove_file(&package_tar)?;
        let count = count_files(&package_dir, |_| true)?;
        println!("    fetched {package} {version} ({count} R files)");
        cran_results.push((package, version, "ok"));
    }

    let cran_ok = cran_results.iter().filter(|(_, _, status)| *status == "ok").count();
    let total_r_files =
        count_files(&r_base_dir, |_| true)? + count_files(&cran_dir, |_| true)?;

    let mut manifest = String::new();
    manifest.push_str("# ry parser corpus — pinned inventory\n");
    manifest.push_str(&format!("# Generated by scripts/fetch-corpus.rs on {}.\n", utc_timestamp()?));
    manifest.push_str("# corpus/ itself is gitignored; re-run scripts/fetch-corpus.rs to repopulate it.\n");
    manifest.push('\n');
    manifest.push_str("[tree-sitter-r]\n");
    manifest.push_str(&format!("ref = {TSR_REF}\n"));
    manifest.push_str("# jsDelivr exposes no branch commit SHA; nearest published tag is 1.3.0 (matches the workspace tree-sitter-r dep).\n");
    manifest.push_str(&format!("corpus_files = {}\n", tsr_files.join(" ")));
    manifest.push('\n');
    manifest.push_str("[r-base]\n");
    manifest.push_str(&format!("version = {r_version}\n"));
    manifest.push_str(&format!(
        "source = https://cran.r-project.org/src/base/R-4/R-{r_version}.tar.gz\n"
    ));
    manifest.push_str(&format!("libraries = {r_base_libraries}\n"));
    manifest.push_str(&format!("r_files = {r_base_files}\n"));
    manifest.push('\n');
    manifest.push_str("[cran]\n");
    manifest.push_str(
        "# package<TAB>version<TAB>status  (ok | not-found | download-failed | no-R-files | skipped-low-disk)\n",
    );
    manifest.push_str(&format!("resolved_ok = {cran_ok} / {}\n", CRAN_PACKAGES.len()));
    for (package, version, status) in &cran_results {
        manifest.push_str(&format!("{package}\t{version}\t{status}\n"));
    }
    manifest.push('\n');
    manifest.push_str("[totals]\n");
    manifest.push_str(&format!(
        "parseable_r_files = {total_r_files}  (r-base + cran, round-trip + acceptance corpus)\n"
    ));
    fs::write(&manifest_path, &manifest)?;

    println!("==> wrote manifest: {}", manifest_path.display());
    println!("==> total parseable R files (r-base + cran): {total_r_files}");
    println!("==> corpus size: {}", human_size(directory_size(&corpus_dir)?));
    Ok(())
}

/// `--fail` turns HTTP errors into non-zero exits so a 404 is visible rather than
/// silently written to disk.
fn fetch(url: &str, output: &Path) -> Result<bool, Box<dyn Error>> {
    let status = Command::new("curl")
        .args(["-sS", "--fail", "--location", "--retry", "4", "--retry-delay", "2"])
        .args(["--max-time", "600"])
        .arg(url)
        .arg("-o")
        .arg(output)
        .status()?;
    Ok(status.success())
}

fn fetch_required(url: &str, output: &Path) -> Result<(), Box<dyn Error>> {
    if fetch(url, output)? {
        Ok(())
    } else {
        Err(format!("download failed: {url}").into())
    }
}

fn tar_list(tarball: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let output = Command::new("tar").arg("tzf").arg(tarball).output()?;
    if !output.status.success() {
        return Err(format!("tar tzf failed for {}", tarball.display()).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn tar_extract(
    tarball: &Path,
    destination: &Path,
    members: &[String],
    members_file: &Path,
) -> Result<(), Box<dyn Error>> {
    if members.is_empty() {
        return Ok(());
    }
    fs::write(members_file, members.join("\n") + "\n")?;
    let status = Command::new("tar")
        .arg("xzf")
        .arg(tarball)
        .arg("-C")
        .arg(destination)
        .arg("-T")
        .arg(members_file)
        .status()?;
    if !status.success() {
        return Err(format!("tar extraction failed for {}", tarball.display()).into());
    }
    Ok(())
}

/// The jsDelivr flat listing carries file paths only under `"name"` keys.
fn json_name_values(json: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let characters: Vec<char> = json.chars().collect();
    let mut names = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != '"' {
            index += 1;
            continue;
        }
        let (string, after_string) = parse_json_string(&characters, index)?;
        index = after_string;
        // A string is a key exactly when the next non-whitespace character is a colon.
        let mut lookahead = index;
        while lookahead < characters.len() && characters[lookahead].is_whitespace() {
            lookahead += 1;
        }
        if lookahead >= characters.len() || characters[lookahead] != ':' || string != "name" {
            continue;
        }
        let mut value_start = lookahead + 1;
        while value_start < characters.len() && characters[value_start].is_whitespace() {
            value_start += 1;
        }
        if value_start < characters.len() && characters[value_start] == '"' {
            let (value, after_value) = parse_json_string(&characters, value_start)?;
            names.push(value);
            index = after_value;
        }
    }
    Ok(names)
}

/// Returns the decoded string and the index just past its closing quote.
fn parse_json_string(characters: &[char], start: usize) -> Result<(String, usize), Box<dyn Error>> {
    let mut value = String::new();
    let mut index = start + 1;
    while index < characters.len() {
        match characters[index] {
            '"' => return Ok((value, index + 1)),
            '\\' => {
                let escape = *characters.get(index + 1).ok_or("unterminated JSON escape")?;
                index += 2;
                match escape {
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    '/' => value.push('/'),
                    'b' => value.push('\u{8}'),
                    'f' => value.push('\u{c}'),
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'u' => {
                        let high = hex4(characters, index)?;
                        index += 4;
                        let code_point = if (0xD800..0xDC00).contains(&high) {
                            if characters.get(index) != Some(&'\\')
                                || characters.get(index + 1) != Some(&'u')
                            {
                                return Err("unpaired JSON surrogate escape".into());
                            }
                            let low = hex4(characters, index + 2)?;
                            index += 6;
                            if !(0xDC00..0xE000).contains(&low) {
                                return Err("unpaired JSON surrogate escape".into());
                            }
                            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                        } else {
                            high
                        };
                        value.push(char::from_u32(code_point).ok_or("invalid JSON \\u escape")?);
                    }
                    other => return Err(format!("invalid JSON escape \\{other}").into()),
                }
            }
            character => {
                value.push(character);
                index += 1;
            }
        }
    }
    Err("unterminated JSON string".into())
}

fn hex4(characters: &[char], start: usize) -> Result<u32, Box<dyn Error>> {
    let mut code = 0u32;
    for offset in 0..4 {
        let digit = characters
            .get(start + offset)
            .and_then(|character| character.to_digit(16))
            .ok_or("invalid JSON \\u escape")?;
        code = code * 16 + digit;
    }
    Ok(code)
}

fn newest_r4_version(index_html: &str) -> Option<String> {
    let mut best: Option<(u64, u64)> = None;
    for (position, _) in index_html.match_indices("R-4.") {
        let rest = &index_html[position + 4..];
        let Some((minor, rest)) = leading_number(rest) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('.') else {
            continue;
        };
        let Some((patch, rest)) = leading_number(rest) else {
            continue;
        };
        if !rest.starts_with(".tar.gz") {
            continue;
        }
        let candidate = (minor, patch);
        if best.is_none_or(|current| candidate > current) {
            best = Some(candidate);
        }
    }
    best.map(|(minor, patch)| format!("4.{minor}.{patch}"))
}

fn leading_number(text: &str) -> Option<(u64, &str)> {
    let end = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text.get(..end)?.parse().ok()?;
    Some((number, text.get(end..)?))
}

/// Match `^[^/]+/src/library/[^/]+/R/[^/]+\.R$`, returning `(library, file)`.
fn r_base_member(member: &str) -> Option<(&str, &str)> {
    let components: Vec<&str> = member.split('/').collect();
    let [top, src, library_literal, library, r, file] = components[..] else {
        return None;
    };
    let matches = !top.is_empty()
        && src == "src"
        && library_literal == "library"
        && !library.is_empty()
        && r == "R"
        && file.len() > ".R".len()
        && file.ends_with(".R");
    matches.then_some((library, file))
}

/// Match `^<package>/R/[^/]+\.[RrSsQq]$`.
fn is_cran_member(member: &str, package: &str) -> bool {
    let components: Vec<&str> = member.split('/').collect();
    let [top, r, file] = components[..] else {
        return false;
    };
    top == package
        && r == "R"
        && file.len() > 2
        && file
            .rfind('.')
            .is_some_and(|dot| dot >= 1 && dot == file.len() - 2)
        && file.ends_with(['R', 'r', 'S', 's', 'Q', 'q'])
}

/// PACKAGES records are paragraphs separated by blank lines.
fn resolve_version(packages_index: &str, package: &str) -> Option<String> {
    let mut name = String::new();
    let mut version = String::new();
    let mut in_record = false;
    for line in packages_index.split('\n').chain(Some("")) {
        if line.is_empty() {
            if in_record {
                if name == package {
                    return Some(version);
                }
                name.clear();
                version.clear();
                in_record = false;
            }
            continue;
        }
        in_record = true;
        let line = line.replace('\r', "");
        if let Some(rest) = line.strip_prefix("Package: ") {
            name = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("Version: ") {
            version = rest.to_string();
        }
    }
    None
}

/// `None` when `df` is unavailable, which skips the low-disk guard.
fn available_kb(path: &Path) -> Option<u64> {
    let output = Command::new("df").arg("-Pk").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn count_directories(root: &Path) -> io::Result<u64> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            count += 1;
        }
    }
    Ok(count)
}

fn count_files(root: &Path, matches: fn(&str) -> bool) -> io::Result<u64> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() && matches(&entry.file_name().to_string_lossy()) {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn directory_size(root: &Path) -> io::Result<u64> {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// Formats like `du -h`, but from apparent file sizes rather than disk blocks.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        let scaled = (value * 10.0).ceil() / 10.0;
        if scaled >= 10.0 {
            format!("{scaled:.0}{}", UNITS[unit])
        } else {
            format!("{scaled:.1}{}", UNITS[unit])
        }
    } else {
        format!("{:.0}{}", value.ceil(), UNITS[unit])
    }
}

fn utc_timestamp() -> Result<String, Box<dyn Error>> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let second_of_day = seconds % 86_400;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    ))
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's civil-from-days.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u64;
    let month = (if month_prime < 10 { month_prime + 3 } else { month_prime - 9 }) as u64;
    (year + i64::from(month <= 2), month, day)
}

struct WorkDir(PathBuf);

impl WorkDir {
    fn create() -> io::Result<WorkDir> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .subsec_nanos();
        for attempt in 0..1_000 {
            let candidate = std::env::temp_dir()
                .join(format!("fetch-corpus-{}-{nanos}-{attempt}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => return Ok(WorkDir(candidate)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("could not create a unique work directory"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("warning: failed to remove work dir {}: {error}", self.0.display());
        }
    }
}
