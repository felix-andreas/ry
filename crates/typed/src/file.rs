//! Typed code files: the machine code an install compiles for a package's typed closures, which
//! a later session maps instead of compiling.
//!
//! `ry install` runs [`write`] in the process that records the package's code image. It compiles
//! every typed closure of the package's database for the argument types its annotations
//! declare, and every typed function of the same database those call, and writes the code
//! beside the database as `R/<package>.<build>.ryt` (`rcmd::images::typed_code`). The code is
//! position independent: every word that differs between processes (a runtime helper's address,
//! a shape's id, an R call site, a typed function of another namespace) is loaded from an offset
//! table the loader fills, which sits just below the code.
//!
//! A session's first call of a typed closure read from such a database ([`attach`]) finds the
//! version compiled for its argument types, checks that every lookup the code assumed still
//! finds the same kind of function in the same binding, builds the R call sites, links the
//! typed functions it calls, and uses the code as if it had compiled it. Anything that does not
//! check out leaves the closure to compile as usual, so a stale or foreign file costs at most
//! the time to find that out.

use {
    crate::{
        COMPILED, Compiled, Key, Linker, Refusal, compiled_or_refusal, earliest, lower_settled,
        optimize,
        backend::{
            self, Callee, Code, Entry, Helper, Slot, Slots, declare, file_frees, helper_address,
            libcall_helper,
        },
        ir::{Item, Type},
        lower::{self, Conversion, Lowered, RArgument, RCall, RResult, Speculation},
        shapes,
    },
    heap::{
        object::{Sexp, SexpType},
        records::{Fields, Layout},
    },
    interp::{
        annotations,
        code_image::{self, DatabaseStamp, Note},
        values as rvalues,
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    ssa::{FuncId, Relocatable},
    std::{
        cell::{Cell, RefCell},
        collections::{BTreeMap, VecDeque},
        io::{Read, Write as _},
        path::Path,
        sync::OnceLock,
    },
};

const MAGIC: &[u8; 8] = b"RYTYPED\0";
const VERSION: u32 = 1;
/// The bytes of the fixed header.
const HEADER_SIZE: usize = 8 + 4 + 8 * 13;
/// The alignment of the code in the file and of the offset table's reservation, which is the
/// largest page size a supported system uses, so the code maps straight from the file.
const ALIGN: u64 = 65536;

/// How an install's [`write`] went: how many typed closures the package has, how many versions
/// were written, and how many were left out for each reason.
#[derive(Default, Debug)]
pub struct Written {
    pub closures: usize,
    pub written: usize,
    pub skipped: BTreeMap<&'static str, usize>,
}

pub const BUILTINS: &[interp::builtins::BuiltinDef] =
    &[interp::builtins::internal("ryTypedCodeWrite", 1, |call| unsafe { do_write(call) })];

/// `ryTypedCodeWrite(package)`: writes the typed code file of the package whose database the
/// install under way recorded. Returns how many typed closures it has and how many versions
/// were written and left out, by reason.
unsafe fn do_write(call: &interp::builtins::Call<'_>) -> interp::unwind::R<Sexp> {
    let package = rvalues::first_string(call.arg(0))
        .ok_or_else(|| interp::unwind::error("the package must be a string"))?
        .into_owned();
    let written = write(&package).map_err(interp::unwind::error)?;
    let mut items = vec![
        ("closures", written.closures),
        ("written", written.written),
    ];
    for (reason, count) in &written.skipped {
        items.push((reason, *count));
    }
    let names: Vec<&str> = items.iter().map(|(name, _)| *name).collect();
    let counts: Vec<i32> = items
        .iter()
        .map(|(_, count)| i32::try_from(*count).unwrap_or(i32::MAX))
        .collect();
    let counts = rvalues::integer_vector(&counts)?;
    interp::heap().root(counts);
    rvalues::set_attr(
        counts,
        rvalues::symbols().names,
        rvalues::string_vector(&names)?,
    )?;
    Ok(counts)
}

/// Compiles the typed closures of `package`'s recorded database and writes its typed code file.
pub unsafe fn write(package: &str) -> Result<Written, String> {
    let build = interp::interp().code_image.build;
    if build.is_empty() {
        return Err(String::from(
            "this runtime has no build identity to stamp a typed code file with",
        ));
    }
    let file_name = format!("{package}.rdb");
    let Some(database) = code_image::recorded_databases().into_iter().find(|database| {
        let path = Path::new(database);
        path.file_name().is_some_and(|name| *name == *file_name)
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "R")
    }) else {
        return Ok(Written::default());
    };
    let Some(stamp) = DatabaseStamp::of(&database)? else {
        return Ok(Written::default());
    };
    let mut written = Written::default();
    let mut own: FxHashMap<Sexp, (u64, u32)> = FxHashMap::default();
    let mut roots: Vec<Key> = Vec::new();
    for (offset, position, closure) in code_image::recorded_closures(&database) {
        if !annotations::is_typed(closure) || own.contains_key(&closure) {
            continue;
        }
        own.insert(closure, (offset, position));
        written.closures += 1;
        match crate::ahead::declared_params(closure) {
            Ok(params) => match compiled_or_refusal(closure, &params) {
                Ok(_) => roots.push((closure, params)),
                Err(_) => *written.skipped.entry("does not compile").or_default() += 1,
            },
            Err(_) => *written.skipped.entry("compiles with its caller").or_default() += 1,
        }
    }
    // As for code compiled ahead of a script: a later function may confirm a shape that a
    // `match` in an earlier one must test for.
    for _ in 0..4 {
        let epoch = shapes::epoch();
        for (closure, params) in &roots {
            if compiled_or_refusal(*closure, params).is_err() {
                continue;
            }
        }
        if shapes::epoch() == epoch {
            break;
        }
    }
    let names = binding_names(package);
    let mut versions = collect(&roots, &own, &mut written);
    versions.sort_by_cached_key(|version| {
        let (offset, position) = own.get(&version.key.0).copied().unwrap_or_default();
        (offset, position, format!("{:?}", version.key.1))
    });
    let result = build_file(
        &versions, &own, &names, package, stamp, build, &database, &mut written,
    );
    let heap = interp::heap();
    for version in &versions {
        for &object in &version.preserved {
            heap.release(object);
        }
    }
    result?;
    Ok(written)
}

/// A version to write: a closure lowered for one list of argument types.
struct Pending {
    key: Key,
    lowered: Lowered,
    linker: Linker,
    ir: crate::ir::Function,
    /// The R objects the lowering made that nothing else keeps alive until the file is written.
    preserved: Vec<Sexp>,
}

/// Lowers every version the roots reach through calls of typed closures of the same database.
unsafe fn collect(
    roots: &[Key],
    own: &FxHashMap<Sexp, (u64, u32)>,
    written: &mut Written,
) -> Vec<Pending> {
    let heap = interp::heap();
    let mut seen: FxHashSet<Key> = FxHashSet::default();
    let mut queue: VecDeque<Key> = roots.iter().cloned().collect();
    let mut versions = Vec::new();
    while let Some(key) = queue.pop_front() {
        if !seen.insert(key.clone()) {
            continue;
        }
        if compiled_or_refusal(key.0, &key.1).is_err() {
            *written.skipped.entry("does not compile").or_default() += 1;
            continue;
        }
        let Ok((lowered, linker)) = lower_settled(key.0, &key.1) else {
            *written.skipped.entry("does not compile").or_default() += 1;
            continue;
        };
        // Which shapes a `match` over any tag tests for depends on the session that runs it.
        if earliest(lowered.epoch, linker.epoch).is_some() {
            *written.skipped.entry("matches over any tag").or_default() += 1;
            continue;
        }
        let mut preserved = Vec::new();
        for site in &lowered.r_calls {
            preserved.push(site.call);
            for argument in &site.arguments {
                preserved.push(argument.code);
                if let Some(conversion) = argument.conversion {
                    preserved.push(conversion.call);
                }
            }
            if let RResult::Converted(conversion, _) = site.result {
                preserved.push(conversion.call);
            }
        }
        for &object in &preserved {
            heap.preserve(object);
        }
        for called in &linker.called {
            if own.contains_key(&called.0) {
                queue.push_back(called.clone());
            }
        }
        let mut ir = lowered.function.clone();
        optimize(&mut ir);
        versions.push(Pending {
            key,
            lowered,
            linker,
            ir,
            preserved,
        });
    }
    versions
}

/// The name each closure of the package's namespace is bound to, for the log.
unsafe fn binding_names(package: &str) -> FxHashMap<Sexp, String> {
    let mut names = FxHashMap::default();
    let Some(namespace) = rvalues::install(package)
        .ok()
        .and_then(|symbol| loaded_namespace(symbol))
    else {
        return names;
    };
    let heap = interp::heap();
    for symbol in heap.frame_symbols(namespace) {
        if let Some(cell) = heap.frame_cell(namespace, symbol) {
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                value = value.car();
            }
            if value.kind() == SexpType::Closure {
                names
                    .entry(value)
                    .or_insert_with(|| rvalues::symbol_name(symbol).into_owned());
            }
        }
    }
    names
}

#[allow(clippy::too_many_arguments)]
unsafe fn build_file(
    versions: &[Pending],
    own: &FxHashMap<Sexp, (u64, u32)>,
    names: &FxHashMap<Sexp, String>,
    package: &str,
    stamp: DatabaseStamp,
    build: &str,
    database: &str,
    written: &mut Written,
) -> Result<(), String> {
    let refused = |refusal: Refusal| refusal.0.into_owned();
    let mut object = Relocatable::new().map_err(|error| error.0.to_owned())?;
    let target = object.target();
    let mut ids: Vec<(FuncId, FuncId)> = Vec::with_capacity(versions.len());
    for version in versions {
        let ir = &version.ir;
        ids.push(declare(&mut object, &ir.locals[..ir.params], ir.result).map_err(refused)?);
    }
    let mut slots = Slots::default();
    let frees = file_frees(&mut object, &mut slots).map_err(refused)?;
    let index: FxHashMap<&Key, usize> = versions
        .iter()
        .enumerate()
        .map(|(position, version)| (&version.key, position))
        .collect();
    let mut encoder = Encoder {
        own,
        package,
        stamp,
        shapes: ShapeWriter::default(),
        lookups: Vec::new(),
        sites: Vec::new(),
        callees: Vec::new(),
    };
    let mut records: Vec<Option<VersionRecord>> = Vec::with_capacity(versions.len());
    for (position, version) in versions.iter().enumerate() {
        let record = match encoder.version(version, &index, names) {
            Ok(record) => record,
            Err(reason) => {
                *written.skipped.entry(reason).or_default() += 1;
                records.push(None);
                continue;
            }
        };
        let callees: Vec<Callee> = record
            .callees
            .iter()
            .map(|&callee| {
                let entry = &encoder.callees[callee as usize];
                Callee {
                    entry: match entry.target {
                        Target::Direct(target) => Entry::Function(ids[target as usize].0),
                        Target::Slot => Entry::Callee(callee),
                    },
                    params: entry.params.clone(),
                    result: entry.result,
                }
            })
            .collect();
        let sites: Vec<usize> = record.sites.iter().map(|&site| site as usize).collect();
        // The session-side types of the callees, which the emitter needs, not the file's.
        let callees: Vec<Callee> = callees
            .into_iter()
            .zip(&version.linker.callees)
            .map(|(mut callee, session)| {
                callee.params = session.params.clone();
                callee.result = session.result;
                callee
            })
            .collect();
        if backend::compile_into(
            &mut object,
            &mut slots,
            frees,
            &version.ir,
            &callees,
            &sites,
            ids[position],
        )
        .is_err()
        {
            *written.skipped.entry("does not compile into a file").or_default() += 1;
            records.push(None);
            continue;
        }
        records.push(Some(record));
    }
    let linked = object.finish(&mut |name| {
        libcall_helper(name).map(|helper| slots.number(Slot::Helper(helper)))
    });
    let free_ids = frees.ids();
    let mut shared_start = linked.code.len() as u64;
    for id in free_ids {
        match linked.functions.get(id.as_u32() as usize) {
            Some(Ok(offset)) => shared_start = shared_start.min(u64::from(*offset)),
            _ => return Err(String::from("the free entries of a typed code file do not compile")),
        }
    }
    let offset_of = |id: FuncId| -> Option<u64> {
        linked
            .functions
            .get(id.as_u32() as usize)
            .and_then(|found| found.as_ref().ok())
            .map(|&offset| u64::from(offset))
    };
    // A version is kept when its code and its trampoline linked and every version of the file
    // it calls directly is kept too.
    let mut kept: Vec<bool> = records
        .iter()
        .zip(&ids)
        .map(|(record, &(body, trampoline))| {
            record.is_some() && offset_of(body).is_some() && offset_of(trampoline).is_some()
        })
        .collect();
    loop {
        let mut changed = false;
        for position in 0..records.len() {
            if !kept[position] {
                continue;
            }
            let Some(record) = &records[position] else {
                continue;
            };
            let calls_dropped = record.callees.iter().any(|&callee| {
                matches!(encoder.callees[callee as usize].target,
                    Target::Direct(target) if !kept[target as usize])
            });
            if calls_dropped {
                kept[position] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (position, record) in records.iter().enumerate() {
        if record.is_some() && !kept[position] {
            *written.skipped.entry("calls code that did not link").or_default() += 1;
        }
    }
    let mut boundaries: Vec<u64> = linked
        .functions
        .iter()
        .filter_map(|found| found.as_ref().ok().map(|&offset| u64::from(offset)))
        .collect();
    boundaries.push(linked.code.len() as u64);
    boundaries.sort_unstable();
    let mut renumbered: Vec<Option<u32>> = vec![None; records.len()];
    let mut next = 0u32;
    for (position, keep) in kept.iter().enumerate() {
        if *keep {
            renumbered[position] = Some(next);
            next += 1;
        }
    }
    let mut functions: Vec<Version> = Vec::new();
    for (position, record) in records.into_iter().enumerate() {
        let Some(record) = record.filter(|_| kept[position]) else {
            continue;
        };
        let (body, trampoline) = ids[position];
        let (Some(body), Some(trampoline)) = (offset_of(body), offset_of(trampoline)) else {
            continue;
        };
        let last = body.max(trampoline);
        let end = boundaries
            .iter()
            .copied()
            .find(|&boundary| boundary > last)
            .unwrap_or(linked.code.len() as u64);
        let code = linked
            .code
            .get(body as usize..end as usize)
            .ok_or_else(|| String::from("a function outside the code"))?;
        functions.push(Version {
            body,
            trampoline,
            end,
            hash: code_image::hash(code),
            ..record.version
        });
        written.written += 1;
    }
    for callee in &mut encoder.callees {
        if let Target::Direct(target) = callee.target {
            // A dropped target leaves only dropped callers, whose records are gone.
            callee.target = Target::Direct(renumbered[target as usize].unwrap_or(u32::MAX));
        }
    }
    let mut file_slots = Vec::with_capacity(slots.slots.len());
    for &slot in &slots.slots {
        file_slots.push(match slot {
            Slot::Shape(shape) => Slot::Shape(encoder.shapes.shape(shape)?),
            Slot::Constant(shape) => Slot::Constant(encoder.shapes.shape(shape)?),
            slot => slot,
        });
    }
    let metadata = Parsed {
        header: Header::default(),
        strings: std::mem::take(&mut encoder.shapes.strings.list),
        shapes: std::mem::take(&mut encoder.shapes.entries),
        slots: file_slots,
        lookups: std::mem::take(&mut encoder.lookups),
        sites: std::mem::take(&mut encoder.sites),
        callees: std::mem::take(&mut encoder.callees),
        functions,
    };
    let metadata = encode_metadata(&metadata);
    let code_offset = ((HEADER_SIZE + metadata.len()) as u64).next_multiple_of(ALIGN);
    let mut tail = linked.code.clone();
    tail.resize(tail.len().next_multiple_of(8), 0);
    tail.extend_from_slice(&slots.data);
    let header = Header {
        build: code_image::hash(build.as_bytes()),
        stamp,
        target: code_image::hash(target.as_bytes()),
        metadata_length: metadata.len() as u64,
        slots: slots.slots.len() as u64,
        code_offset,
        code_length: linked.code.len() as u64,
        data_length: slots.data.len() as u64,
        shared_start,
        metadata_hash: code_image::stream_hash(&metadata),
        shared_hash: code_image::stream_hash(tail.get(shared_start as usize..).unwrap_or_default()),
    };
    let mut bytes = Vec::with_capacity(code_offset as usize + tail.len());
    encode_header(&header, &mut bytes);
    bytes.extend_from_slice(&metadata);
    bytes.resize(code_offset as usize, 0);
    bytes.extend_from_slice(&tail);
    let path = rcmd::images::typed_code(Path::new(database), build);
    let temporary = path.with_extension("ryt.partial");
    let mut file = std::fs::File::create(&temporary)
        .map_err(|failure| format!("cannot write {}: {failure}", temporary.display()))?;
    file.write_all(&bytes)
        .map_err(|failure| format!("cannot write {}: {failure}", temporary.display()))?;
    drop(file);
    std::fs::rename(&temporary, &path)
        .map_err(|failure| format!("cannot write {}: {failure}", path.display()))?;
    Ok(())
}

// The file's metadata, as the loader reads it. Every index is checked against its table when the
// file is parsed, so the loader indexes without a second check.

/// A parsed typed code file's header and metadata.
#[derive(Default, Debug, PartialEq)]
pub struct Parsed {
    pub header: Header,
    pub strings: Vec<String>,
    pub shapes: Vec<ShapeEntry>,
    pub slots: Vec<Slot>,
    pub lookups: Vec<Lookup>,
    pub sites: Vec<Site>,
    pub callees: Vec<FileCallee>,
    pub functions: Vec<Version>,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The hash of the build that wrote the file, which alone knows its helper numbers.
    pub build: u64,
    pub stamp: DatabaseStamp,
    /// The hash of the code's target, its triple and backend flags.
    pub target: u64,
    pub metadata_length: u64,
    pub slots: u64,
    pub code_offset: u64,
    pub code_length: u64,
    pub data_length: u64,
    /// Where the code that every function shares starts, which the loader checks at once.
    pub shared_start: u64,
    pub metadata_hash: u64,
    pub shared_hash: u64,
}

/// A shape, a union of shapes or a list item type the file's types name, by its place in the
/// file. An entry names only entries before it.
#[derive(Debug, Clone, PartialEq)]
pub enum ShapeEntry {
    Shape {
        tag: Option<u32>,
        fields: FieldNames,
        items: Vec<Item>,
    },
    Union(Vec<u32>),
    Array(Item),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldNames {
    Positional(u32),
    Named(Vec<u32>),
    Dict,
    Set,
}

/// A function or value lookup the code assumes: of `symbol` from `start`, finding a binding in
/// `found` whose value is of `class`.
#[derive(Debug, Clone, PartialEq)]
pub struct Lookup {
    pub symbol: u32,
    pub function: bool,
    pub start: EnvRef,
    pub found: EnvRef,
    pub class: Class,
}

/// An environment a lookup starts from or finds its binding in, named so a later session finds
/// its own.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvRef {
    Global,
    BaseEnv,
    BaseNamespace,
    StandardLibrary,
    Namespace(u32),
    Imports(u32),
    Package(u32),
}

/// What the value a lookup found is, which a later session checks its own value against.
#[derive(Debug, Clone, PartialEq)]
pub enum Class {
    /// One of base R's functions under its own name.
    Base,
    /// One of the table API's functions that gives a table value.
    Table,
    /// The standard library's own binding of the name.
    Library,
    /// A typed closure read from `package`'s database, at that record and position.
    File {
        package: u32,
        offset: u64,
        position: u32,
        stamp: DatabaseStamp,
    },
    /// Any function that is not typed, which the code calls through R.
    Other,
}

/// R code an R call site holds, rebuilt by a later session.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Nil,
    Missing,
    Symbol(u32),
    /// A call: the function and each argument, with its tag.
    Call(Vec<(Option<u32>, Expr)>),
    Str(u32),
    Dbl(u64),
    Int(i32),
    Lgl(i32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub env: EnvRef,
    pub call: Expr,
    pub function: u32,
    pub arguments: Vec<Argument>,
    pub result: SiteResult,
    pub diverges: bool,
    pub effect_free: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Argument {
    pub tag: Option<u32>,
    pub code: Expr,
    pub ty: Type,
    pub conversion: Option<FileConversion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileConversion {
    pub call: Expr,
    pub function: u32,
    pub checked: Option<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SiteResult {
    Unused,
    Converted(FileConversion, Type),
    Table,
}

/// A typed function a version calls, found by `lookup`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileCallee {
    pub lookup: u32,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    pub target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// A version of this file, called relative to the caller.
    Direct(u32),
    /// The code a session has for the function, which the loader writes into the slot of
    /// [`Slot::Callee`] with this callee's index.
    Slot,
}

/// One closure's code for one list of argument types.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub name: u32,
    pub offset: u64,
    pub position: u32,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    pub body: u64,
    pub trampoline: u64,
    /// Where the version's code ends, and the hash of its bytes from `body` up to here.
    pub end: u64,
    pub hash: u64,
    pub calls_r: bool,
    pub lookups: Vec<u32>,
    pub sites: Vec<u32>,
    pub callees: Vec<u32>,
    /// The shapes the code constructs, which a session confirms before it runs the code.
    pub confirms: Vec<u32>,
}

/// A version's metadata before its code is placed.
struct VersionRecord {
    version: Version,
    sites: Vec<u32>,
    callees: Vec<u32>,
}

#[derive(Default)]
struct Strings {
    list: Vec<String>,
    ids: FxHashMap<String, u32>,
}

impl Strings {
    fn id(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = self.list.len() as u32;
        self.list.push(text.to_owned());
        self.ids.insert(text.to_owned(), id);
        id
    }
}

/// The file's shape entries, by the session's ids.
#[derive(Default)]
struct ShapeWriter {
    entries: Vec<ShapeEntry>,
    shapes: FxHashMap<u32, u32>,
    unions: FxHashMap<u32, u32>,
    arrays: FxHashMap<u32, u32>,
    visiting: FxHashSet<(u8, u32)>,
    strings: Strings,
}

impl ShapeWriter {
    fn push(&mut self, entry: ShapeEntry) -> u32 {
        self.entries.push(entry);
        self.entries.len() as u32 - 1
    }

    unsafe fn shape(&mut self, id: u32) -> Result<u32, String> {
        if let Some(&entry) = self.shapes.get(&id) {
            return Ok(entry);
        }
        if !self.visiting.insert((0, id)) {
            return Err(String::from("a shape that contains itself"));
        }
        let (layout, items) = shapes::fields(id);
        let heap = interp::heap();
        let Some(layout) = heap.layouts.get(layout).cloned() else {
            return Err(String::from("a shape without a layout"));
        };
        let tag = match layout.tag {
            Some(tag) => {
                let symbol = heap
                    .layouts
                    .tag_name(tag)
                    .ok_or_else(|| String::from("a tag without a name"))?;
                Some(self.strings.id(&rvalues::symbol_name(symbol)))
            }
            None => None,
        };
        let fields = match &layout.fields {
            Fields::Positional(count) => FieldNames::Positional(*count),
            Fields::Named(names) => FieldNames::Named(
                names
                    .iter()
                    .map(|&name| self.strings.id(&rvalues::symbol_name(name)))
                    .collect(),
            ),
            Fields::Dict => FieldNames::Dict,
            Fields::Set => FieldNames::Set,
        };
        let mut file_items = Vec::with_capacity(items.len());
        for &item in &items {
            file_items.push(self.item(item)?);
        }
        let entry = self.push(ShapeEntry::Shape {
            tag,
            fields,
            items: file_items,
        });
        self.visiting.remove(&(0, id));
        self.shapes.insert(id, entry);
        Ok(entry)
    }

    unsafe fn union(&mut self, id: u32) -> Result<u32, String> {
        if id == shapes::ANY {
            return Ok(shapes::ANY);
        }
        if let Some(&entry) = self.unions.get(&id) {
            return Ok(entry);
        }
        let members = shapes::members(id).unwrap_or_default();
        let mut entries = Vec::with_capacity(members.len());
        for &member in &members {
            entries.push(self.shape(member)?);
        }
        let entry = self.push(ShapeEntry::Union(entries));
        self.unions.insert(id, entry);
        Ok(entry)
    }

    unsafe fn array(&mut self, id: u32) -> Result<u32, String> {
        if let Some(&entry) = self.arrays.get(&id) {
            return Ok(entry);
        }
        if !self.visiting.insert((1, id)) {
            return Err(String::from("a list type that contains itself"));
        }
        let item = self.item(shapes::array_item(id))?;
        let entry = self.push(ShapeEntry::Array(item));
        self.visiting.remove(&(1, id));
        self.arrays.insert(id, entry);
        Ok(entry)
    }

    unsafe fn item(&mut self, item: Item) -> Result<Item, String> {
        Ok(match item {
            Item::Record(shape) => Item::Record(self.shape(shape)?),
            Item::Tag(union) => Item::Tag(self.union(union)?),
            Item::Array(id) => Item::Array(self.array(id)?),
            item => item,
        })
    }

    unsafe fn ty(&mut self, ty: Type) -> Result<Type, String> {
        Ok(match ty {
            Type::Vector { item, array } => Type::Vector {
                item: self.item(item)?,
                array,
            },
            Type::List(item) => Type::List(self.item(item)?),
            Type::Set(item) => Type::Set(self.item(item)?),
            Type::Dict { key, value } => Type::Dict {
                key: self.item(key)?,
                value: self.item(value)?,
            },
            Type::Record(shape) => Type::Record(self.shape(shape)?),
            Type::Tag(union) => Type::Tag(self.union(union)?),
            ty => ty,
        })
    }
}

/// Encodes the parts of each version that name things of the session.
struct Encoder<'a> {
    own: &'a FxHashMap<Sexp, (u64, u32)>,
    package: &'a str,
    stamp: DatabaseStamp,
    shapes: ShapeWriter,
    lookups: Vec<Lookup>,
    sites: Vec<Site>,
    callees: Vec<FileCallee>,
}

const UNNAMED_LOOKUP: &str = "assumes a lookup a session cannot check";
const UNNAMED_CODE: &str = "calls R with code a session cannot rebuild";
const UNNAMED_FUNCTION: &str = "calls a function it cannot name";
const UNNAMED_SHAPE: &str = "has a type a file cannot name";

impl Encoder<'_> {
    unsafe fn version(
        &mut self,
        version: &Pending,
        index: &FxHashMap<&Key, usize>,
        names: &FxHashMap<Sexp, String>,
    ) -> Result<VersionRecord, &'static str> {
        let lowered = &version.lowered;
        let (offset, position) = self
            .own
            .get(&version.key.0)
            .copied()
            .ok_or(UNNAMED_FUNCTION)?;
        let mut lookups = Vec::with_capacity(lowered.speculations.len());
        for speculation in &lowered.speculations {
            lookups.push(self.lookup(speculation).ok_or(UNNAMED_LOOKUP)?);
        }
        // A function the code calls is named by the lookup that found it.
        let identify = |function: Sexp| -> Option<u32> {
            lowered
                .speculations
                .iter()
                .position(|speculation| speculation.function == function)
                .and_then(|found| lookups.get(found).copied())
        };
        let mut sites = Vec::with_capacity(lowered.r_calls.len());
        for site in &lowered.r_calls {
            let encoded = self.site(site, &identify)?;
            self.sites.push(encoded);
            sites.push(self.sites.len() as u32 - 1);
        }
        let mut callees = Vec::with_capacity(version.linker.callees.len());
        for (position, session) in version.linker.callees.iter().enumerate() {
            let key = version.linker.called.get(position).ok_or(UNNAMED_FUNCTION)?;
            let lookup = identify(key.0).ok_or(UNNAMED_FUNCTION)?;
            let mut params = SmallVec::new();
            for &param in &session.params {
                params.push(self.shapes.ty(param).map_err(|_| UNNAMED_SHAPE)?);
            }
            let result = self.shapes.ty(session.result).map_err(|_| UNNAMED_SHAPE)?;
            let target = match index.get(key) {
                Some(&target) => Target::Direct(target as u32),
                None => Target::Slot,
            };
            self.callees.push(FileCallee {
                lookup,
                params,
                result,
                target,
            });
            callees.push(self.callees.len() as u32 - 1);
        }
        let mut params = SmallVec::new();
        for &param in &version.key.1 {
            params.push(self.shapes.ty(param).map_err(|_| UNNAMED_SHAPE)?);
        }
        let result = self.shapes.ty(version.ir.result).map_err(|_| UNNAMED_SHAPE)?;
        let mut confirms = Vec::with_capacity(lowered.constructed.len());
        for &shape in &lowered.constructed {
            confirms.push(self.shapes.shape(shape).map_err(|_| UNNAMED_SHAPE)?);
        }
        let name = names
            .get(&version.key.0)
            .map_or("<anonymous>", String::as_str);
        let calls_r = version.linker.calls_r
            || lowered
                .r_calls
                .iter()
                .any(|site| !site.diverges && !site.effect_free);
        Ok(VersionRecord {
            version: Version {
                name: self.shapes.strings.id(name),
                offset,
                position,
                params,
                result,
                body: 0,
                trampoline: 0,
                end: 0,
                hash: 0,
                calls_r,
                lookups,
                sites: sites.clone(),
                callees: callees.clone(),
                confirms,
            },
            sites,
            callees,
        })
    }

    /// The file's lookup for `speculation`, or `None` when a later session could not check it.
    unsafe fn lookup(&mut self, speculation: &Speculation) -> Option<u32> {
        let heap = interp::heap();
        let symbol = speculation.symbol;
        let empty = heap.known().empty_env;
        let mut env = speculation.env;
        let found = loop {
            if env == empty {
                return None;
            }
            if let Some(cell) = heap.frame_cell(env, symbol) {
                if cell != speculation.cell {
                    return None;
                }
                break env;
            }
            env = env.enclosure();
        };
        let function = speculation.function;
        let library = interp::standard_library::builtins_env()
            .and_then(|env| heap.frame_cell(env, symbol))
            .map(|cell| cell.cdr());
        let class = if library == Some(function) {
            Class::Library
        } else if !rvalues::is_function(function) {
            return None;
        } else if lower::is_base_function(speculation) {
            Class::Base
        } else if lower::is_table_function(speculation) {
            Class::Table
        } else if annotations::is_typed(function) {
            if let Some(&(offset, position)) = self.own.get(&function) {
                Class::File {
                    package: self.shapes.strings.id(self.package),
                    offset,
                    position,
                    stamp: self.stamp,
                }
            } else {
                let note = code_image::typed_note(function)?;
                let path = code_image::database_path(note.database);
                let package = Path::new(&*path).file_stem()?.to_str()?;
                Class::File {
                    package: self.shapes.strings.id(package),
                    offset: note.offset,
                    position: note.position,
                    stamp: note.stamp,
                }
            }
        } else {
            Class::Other
        };
        let lookup = Lookup {
            symbol: self.shapes.strings.id(&rvalues::symbol_name(symbol)),
            function: rvalues::is_function(function),
            start: self.env(speculation.env)?,
            found: self.env(found)?,
            class,
        };
        self.lookups.push(lookup);
        Some(self.lookups.len() as u32 - 1)
    }

    unsafe fn env(&mut self, env: Sexp) -> Option<EnvRef> {
        if env == rvalues::global_env() {
            return Some(EnvRef::Global);
        }
        if env == rvalues::base_env() {
            return Some(EnvRef::BaseEnv);
        }
        if env == rvalues::base_namespace() {
            return Some(EnvRef::BaseNamespace);
        }
        if interp::standard_library::builtins_env() == Some(env) {
            return Some(EnvRef::StandardLibrary);
        }
        if interp::envir::is_namespace(env) {
            let name = interp::envir::namespace_name(env)?;
            return Some(EnvRef::Namespace(self.shapes.strings.id(&name)));
        }
        let name = rvalues::first_string(rvalues::get_attr(env, rvalues::symbols().name))?;
        if let Some(package) = name.strip_prefix("imports:") {
            return Some(EnvRef::Imports(self.shapes.strings.id(package)));
        }
        if name.starts_with("package:") {
            return Some(EnvRef::Package(self.shapes.strings.id(&name)));
        }
        None
    }

    unsafe fn site(
        &mut self,
        site: &RCall,
        identify: &dyn Fn(Sexp) -> Option<u32>,
    ) -> Result<Site, &'static str> {
        let mut arguments = Vec::with_capacity(site.arguments.len());
        for argument in &site.arguments {
            let tag = if argument.tag == rvalues::nil() {
                None
            } else {
                Some(self.shapes.strings.id(&rvalues::symbol_name(argument.tag)))
            };
            let conversion = match argument.conversion {
                Some(conversion) => Some(self.conversion(conversion, identify)?),
                None => None,
            };
            arguments.push(Argument {
                tag,
                code: self.expr(argument.code).ok_or(UNNAMED_CODE)?,
                ty: self.shapes.ty(argument.ty).map_err(|_| UNNAMED_SHAPE)?,
                conversion,
            });
        }
        let result = match site.result {
            RResult::Unused => SiteResult::Unused,
            RResult::Converted(conversion, ty) => SiteResult::Converted(
                self.conversion(conversion, identify)?,
                self.shapes.ty(ty).map_err(|_| UNNAMED_SHAPE)?,
            ),
            RResult::Table => SiteResult::Table,
        };
        Ok(Site {
            env: self.env(site.env).ok_or(UNNAMED_CODE)?,
            call: self.expr(site.call).ok_or(UNNAMED_CODE)?,
            function: identify(site.function).ok_or(UNNAMED_FUNCTION)?,
            arguments,
            result,
            diverges: site.diverges,
            effect_free: site.effect_free,
        })
    }

    unsafe fn conversion(
        &mut self,
        conversion: Conversion,
        identify: &dyn Fn(Sexp) -> Option<u32>,
    ) -> Result<FileConversion, &'static str> {
        Ok(FileConversion {
            call: self.expr(conversion.call).ok_or(UNNAMED_CODE)?,
            function: identify(conversion.function).ok_or(UNNAMED_FUNCTION)?,
            checked: match conversion.checked {
                Some(item) => Some(self.shapes.item(item).map_err(|_| UNNAMED_SHAPE)?),
                None => None,
            },
        })
    }

    /// The code `value` as a later session rebuilds it. Attributes, such as source references,
    /// are left out: the code serves `substitute()`, `sys.call()` and error messages.
    unsafe fn expr(&mut self, value: Sexp) -> Option<Expr> {
        let scalar = || rvalues::length(value) == 1 && value.attrib() == rvalues::nil();
        Some(match value.kind() {
            SexpType::Nil => Expr::Nil,
            SexpType::Symbol if value == rvalues::missing_arg() => Expr::Missing,
            SexpType::Symbol => Expr::Symbol(self.shapes.strings.id(&rvalues::symbol_name(value))),
            SexpType::Language => {
                let mut items = Vec::new();
                let mut cell = value;
                while cell != rvalues::nil() {
                    if !matches!(cell.kind(), SexpType::Language | SexpType::Pairlist) {
                        return None;
                    }
                    let tag = cell.tag();
                    let tag = if tag == rvalues::nil() {
                        None
                    } else {
                        Some(self.shapes.strings.id(&rvalues::symbol_name(tag)))
                    };
                    items.push((tag, self.expr(cell.car())?));
                    cell = cell.cdr();
                }
                Expr::Call(items)
            }
            SexpType::String if scalar() => {
                Expr::Str(self.shapes.strings.id(&rvalues::first_string(value)?))
            }
            SexpType::Real if scalar() => Expr::Dbl(rvalues::real_elt(value, 0).ok()?.to_bits()),
            SexpType::Integer if scalar() => Expr::Int(rvalues::int_elt(value, 0).ok()?),
            SexpType::Logical if scalar() => Expr::Lgl(value.slice::<i32>().first().copied()?),
            _ => return None,
        })
    }
}

// The metadata's encoding: unsigned LEB128 numbers, with an optional index stored as one more
// than the index and 0 for none.

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn number(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }

    fn option(&mut self, value: Option<u32>) {
        self.number(value.map_or(0, |value| u64::from(value) + 1));
    }

    fn list(&mut self, values: &[u32]) {
        self.number(values.len() as u64);
        for &value in values {
            self.number(u64::from(value));
        }
    }

    fn item(&mut self, item: Item) {
        match item {
            Item::Int => self.number(0),
            Item::Dbl => self.number(1),
            Item::Lgl => self.number(2),
            Item::Str => self.number(3),
            Item::I64 => self.number(4),
            Item::Record(entry) => {
                self.number(5);
                self.number(u64::from(entry));
            }
            Item::Tag(entry) => {
                self.number(6);
                self.number(u64::from(entry));
            }
            Item::Array(entry) => {
                self.number(7);
                self.number(u64::from(entry));
            }
        }
    }

    fn ty(&mut self, ty: Type) {
        match ty {
            Type::Int => self.number(0),
            Type::Dbl => self.number(1),
            Type::Lgl => self.number(2),
            Type::Bool => self.number(3),
            Type::Str => self.number(4),
            Type::I64 => self.number(5),
            Type::Null => self.number(6),
            Type::Vector { item, array } => {
                self.number(7);
                self.item(item);
                self.number(u64::from(array));
            }
            Type::List(item) => {
                self.number(8);
                self.item(item);
            }
            Type::Set(item) => {
                self.number(9);
                self.item(item);
            }
            Type::Dict { key, value } => {
                self.number(10);
                self.item(key);
                self.item(value);
            }
            Type::Record(entry) => {
                self.number(11);
                self.number(u64::from(entry));
            }
            Type::Tag(entry) => {
                self.number(12);
                self.number(u64::from(entry));
            }
            Type::Table => self.number(13),
        }
    }

    fn types(&mut self, types: &[Type]) {
        self.number(types.len() as u64);
        for &ty in types {
            self.ty(ty);
        }
    }

    fn env(&mut self, env: &EnvRef) {
        match env {
            EnvRef::Global => self.number(0),
            EnvRef::BaseEnv => self.number(1),
            EnvRef::BaseNamespace => self.number(2),
            EnvRef::StandardLibrary => self.number(3),
            EnvRef::Namespace(name) => {
                self.number(4);
                self.number(u64::from(*name));
            }
            EnvRef::Imports(name) => {
                self.number(5);
                self.number(u64::from(*name));
            }
            EnvRef::Package(name) => {
                self.number(6);
                self.number(u64::from(*name));
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Nil => self.number(0),
            Expr::Missing => self.number(1),
            Expr::Symbol(name) => {
                self.number(2);
                self.number(u64::from(*name));
            }
            Expr::Call(items) => {
                self.number(3);
                self.number(items.len() as u64);
                for (tag, item) in items {
                    self.option(*tag);
                    self.expr(item);
                }
            }
            Expr::Str(text) => {
                self.number(4);
                self.number(u64::from(*text));
            }
            Expr::Dbl(bits) => {
                self.number(5);
                self.number(*bits);
            }
            Expr::Int(value) => {
                self.number(6);
                self.number(u64::from(*value as u32));
            }
            Expr::Lgl(value) => {
                self.number(7);
                self.number(u64::from(*value as u32));
            }
        }
    }

    fn conversion(&mut self, conversion: &FileConversion) {
        self.expr(&conversion.call);
        self.number(u64::from(conversion.function));
        match conversion.checked {
            Some(item) => {
                self.number(1);
                self.item(item);
            }
            None => self.number(0),
        }
    }
}

fn encode_header(header: &Header, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    for word in [
        header.build,
        header.stamp.length,
        header.stamp.modified,
        header.target,
        header.metadata_length,
        header.slots,
        header.code_offset,
        header.code_length,
        header.data_length,
        header.shared_start,
        header.metadata_hash,
        header.shared_hash,
        0,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
}

fn encode_metadata(parsed: &Parsed) -> Vec<u8> {
    let mut out = Out::default();
    out.number(parsed.strings.len() as u64);
    for text in &parsed.strings {
        out.number(text.len() as u64);
        out.0.extend_from_slice(text.as_bytes());
    }
    out.number(parsed.shapes.len() as u64);
    for entry in &parsed.shapes {
        match entry {
            ShapeEntry::Shape { tag, fields, items } => {
                out.number(0);
                out.option(*tag);
                match fields {
                    FieldNames::Positional(count) => {
                        out.number(0);
                        out.number(u64::from(*count));
                    }
                    FieldNames::Named(names) => {
                        out.number(1);
                        out.list(names);
                    }
                    FieldNames::Dict => out.number(2),
                    FieldNames::Set => out.number(3),
                }
                out.number(items.len() as u64);
                for &item in items {
                    out.item(item);
                }
            }
            ShapeEntry::Union(members) => {
                out.number(1);
                out.list(members);
            }
            ShapeEntry::Array(item) => {
                out.number(2);
                out.item(*item);
            }
        }
    }
    out.number(parsed.slots.len() as u64);
    for slot in &parsed.slots {
        match *slot {
            Slot::Helper(helper) => {
                out.number(0);
                out.number(helper as u64);
            }
            Slot::Interrupt => out.number(1),
            Slot::Shape(entry) => {
                out.number(2);
                out.number(u64::from(entry));
            }
            Slot::Constant(entry) => {
                out.number(3);
                out.number(u64::from(entry));
            }
            Slot::Data(offset) => {
                out.number(4);
                out.number(u64::from(offset));
            }
            Slot::Site(site) => {
                out.number(5);
                out.number(u64::from(site));
            }
            Slot::Callee(callee) => {
                out.number(6);
                out.number(u64::from(callee));
            }
        }
    }
    out.number(parsed.lookups.len() as u64);
    for lookup in &parsed.lookups {
        out.number(u64::from(lookup.symbol));
        out.number(u64::from(lookup.function));
        out.env(&lookup.start);
        out.env(&lookup.found);
        match &lookup.class {
            Class::Base => out.number(0),
            Class::Table => out.number(1),
            Class::Library => out.number(2),
            Class::File {
                package,
                offset,
                position,
                stamp,
            } => {
                out.number(3);
                out.number(u64::from(*package));
                out.number(*offset);
                out.number(u64::from(*position));
                out.number(stamp.length);
                out.number(stamp.modified);
            }
            Class::Other => out.number(4),
        }
    }
    out.number(parsed.sites.len() as u64);
    for site in &parsed.sites {
        out.env(&site.env);
        out.expr(&site.call);
        out.number(u64::from(site.function));
        out.number(site.arguments.len() as u64);
        for argument in &site.arguments {
            out.option(argument.tag);
            out.expr(&argument.code);
            out.ty(argument.ty);
            match &argument.conversion {
                Some(conversion) => {
                    out.number(1);
                    out.conversion(conversion);
                }
                None => out.number(0),
            }
        }
        match &site.result {
            SiteResult::Unused => out.number(0),
            SiteResult::Converted(conversion, ty) => {
                out.number(1);
                out.conversion(conversion);
                out.ty(*ty);
            }
            SiteResult::Table => out.number(2),
        }
        out.number(u64::from(site.diverges) | u64::from(site.effect_free) << 1);
    }
    out.number(parsed.callees.len() as u64);
    for callee in &parsed.callees {
        out.number(u64::from(callee.lookup));
        out.types(&callee.params);
        out.ty(callee.result);
        match callee.target {
            Target::Direct(version) => {
                out.number(0);
                out.number(u64::from(version));
            }
            Target::Slot => out.number(1),
        }
    }
    out.number(parsed.functions.len() as u64);
    for version in &parsed.functions {
        out.number(u64::from(version.name));
        out.number(version.offset);
        out.number(u64::from(version.position));
        out.types(&version.params);
        out.ty(version.result);
        out.number(version.body);
        out.number(version.trampoline);
        out.number(version.end);
        out.number(version.hash);
        out.number(u64::from(version.calls_r));
        out.list(&version.lookups);
        out.list(&version.sites);
        out.list(&version.callees);
        out.list(&version.confirms);
    }
    out.0
}

/// Reads the metadata, checking every index against the table it names as it goes.
struct In<'a> {
    bytes: &'a [u8],
    at: usize,
}

type Malformed = &'static str;
const MALFORMED: Malformed = "the typed code file is malformed";

impl In<'_> {
    fn number(&mut self) -> Result<u64, Malformed> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.at).ok_or(MALFORMED)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MALFORMED)
    }

    fn index(&mut self, bound: usize) -> Result<u32, Malformed> {
        let value = self.number()?;
        if value >= bound as u64 {
            return Err(MALFORMED);
        }
        Ok(value as u32)
    }

    fn word(&mut self) -> Result<u32, Malformed> {
        u32::try_from(self.number()?).map_err(|_| MALFORMED)
    }

    /// A count of items each at least one byte long, so a corrupt count cannot ask for more
    /// memory than the file has bytes.
    fn count(&mut self) -> Result<usize, Malformed> {
        let count = self.number()?;
        if count > (self.bytes.len() - self.at) as u64 {
            return Err(MALFORMED);
        }
        Ok(count as usize)
    }

    fn option(&mut self, bound: usize) -> Result<Option<u32>, Malformed> {
        match self.number()? {
            0 => Ok(None),
            value if value <= bound as u64 => Ok(Some(value as u32 - 1)),
            _ => Err(MALFORMED),
        }
    }

    fn list(&mut self, bound: usize) -> Result<Vec<u32>, Malformed> {
        let count = self.count()?;
        (0..count).map(|_| self.index(bound)).collect()
    }

    fn flag(&mut self) -> Result<bool, Malformed> {
        match self.number()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MALFORMED),
        }
    }

    /// An item whose shape entries are below `shapes`; a shape's field holds only any tag.
    fn item(&mut self, shapes: &[ShapeEntry], field: bool) -> Result<Item, Malformed> {
        let entry = |found: u32, wanted: fn(&ShapeEntry) -> bool| match shapes.get(found as usize) {
            Some(entry) if wanted(entry) => Ok(found),
            _ => Err(MALFORMED),
        };
        Ok(match self.number()? {
            0 => Item::Int,
            1 => Item::Dbl,
            2 => Item::Lgl,
            3 => Item::Str,
            4 => Item::I64,
            5 => Item::Record(entry(self.word()?, |entry| {
                matches!(entry, ShapeEntry::Shape { .. })
            })?),
            6 => match self.word()? {
                shapes::ANY => Item::Tag(shapes::ANY),
                _ if field => return Err(MALFORMED),
                union => Item::Tag(entry(union, |entry| matches!(entry, ShapeEntry::Union(_)))?),
            },
            7 => Item::Array(entry(self.word()?, |entry| {
                matches!(entry, ShapeEntry::Array(_))
            })?),
            _ => return Err(MALFORMED),
        })
    }

    fn ty(&mut self, shapes: &[ShapeEntry]) -> Result<Type, Malformed> {
        Ok(match self.number()? {
            0 => Type::Int,
            1 => Type::Dbl,
            2 => Type::Lgl,
            3 => Type::Bool,
            4 => Type::Str,
            5 => Type::I64,
            6 => Type::Null,
            7 => Type::Vector {
                item: self.item(shapes, false)?,
                array: self.flag()?,
            },
            8 => Type::List(self.item(shapes, false)?),
            9 => Type::Set(self.item(shapes, false)?),
            10 => Type::Dict {
                key: self.item(shapes, false)?,
                value: self.item(shapes, false)?,
            },
            11 => match self.item_record(shapes)? {
                Item::Record(entry) => Type::Record(entry),
                _ => return Err(MALFORMED),
            },
            12 => match self.word()? {
                shapes::ANY => Type::Tag(shapes::ANY),
                union if matches!(shapes.get(union as usize), Some(ShapeEntry::Union(_))) => {
                    Type::Tag(union)
                }
                _ => return Err(MALFORMED),
            },
            13 => Type::Table,
            _ => return Err(MALFORMED),
        })
    }

    fn item_record(&mut self, shapes: &[ShapeEntry]) -> Result<Item, Malformed> {
        let entry = self.word()?;
        match shapes.get(entry as usize) {
            Some(ShapeEntry::Shape { .. }) => Ok(Item::Record(entry)),
            _ => Err(MALFORMED),
        }
    }

    fn types(&mut self, shapes: &[ShapeEntry]) -> Result<SmallVec<[Type; 4]>, Malformed> {
        let count = self.count()?;
        (0..count).map(|_| self.ty(shapes)).collect()
    }

    fn env(&mut self, strings: usize) -> Result<EnvRef, Malformed> {
        Ok(match self.number()? {
            0 => EnvRef::Global,
            1 => EnvRef::BaseEnv,
            2 => EnvRef::BaseNamespace,
            3 => EnvRef::StandardLibrary,
            4 => EnvRef::Namespace(self.index(strings)?),
            5 => EnvRef::Imports(self.index(strings)?),
            6 => EnvRef::Package(self.index(strings)?),
            _ => return Err(MALFORMED),
        })
    }

    fn expr(&mut self, strings: usize, depth: usize) -> Result<Expr, Malformed> {
        if depth > MAX_EXPR_DEPTH {
            return Err(MALFORMED);
        }
        Ok(match self.number()? {
            0 => Expr::Nil,
            1 => Expr::Missing,
            2 => Expr::Symbol(self.index(strings)?),
            3 => {
                let count = self.count()?;
                if count == 0 {
                    return Err(MALFORMED);
                }
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    let tag = self.option(strings)?;
                    items.push((tag, self.expr(strings, depth + 1)?));
                }
                Expr::Call(items)
            }
            4 => Expr::Str(self.index(strings)?),
            5 => Expr::Dbl(self.number()?),
            6 => Expr::Int(self.word()? as i32),
            7 => Expr::Lgl(self.word()? as i32),
            _ => return Err(MALFORMED),
        })
    }

    fn conversion(
        &mut self,
        strings: usize,
        shapes: &[ShapeEntry],
        lookups: usize,
    ) -> Result<FileConversion, Malformed> {
        Ok(FileConversion {
            call: self.expr(strings, 0)?,
            function: self.index(lookups)?,
            checked: if self.flag()? {
                Some(self.item(shapes, false)?)
            } else {
                None
            },
        })
    }
}

/// How deeply the code of an R call site may nest, which bounds the parser's recursion.
const MAX_EXPR_DEPTH: usize = 256;

/// Parses a typed code file's header and metadata from `bytes`, the file up to its code. Every
/// index is checked, so the result is safe to use as long as the code it describes matches
/// the header. This checks nothing about which process may use the file.
pub fn parse(bytes: &[u8]) -> Result<Parsed, Malformed> {
    let header = bytes.get(..HEADER_SIZE).ok_or(MALFORMED)?;
    if header.get(..8) != Some(MAGIC.as_slice()) {
        return Err(MALFORMED);
    }
    if header.get(8..12) != Some(VERSION.to_le_bytes().as_slice()) {
        return Err("the typed code file has another format version");
    }
    let word = |index: usize| -> u64 {
        let start = 12 + 8 * index;
        header
            .get(start..start + 8)
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(0, u64::from_le_bytes)
    };
    let header = Header {
        build: word(0),
        stamp: DatabaseStamp {
            length: word(1),
            modified: word(2),
        },
        target: word(3),
        metadata_length: word(4),
        slots: word(5),
        code_offset: word(6),
        code_length: word(7),
        data_length: word(8),
        shared_start: word(9),
        metadata_hash: word(10),
        shared_hash: word(11),
    };
    let metadata_end = (HEADER_SIZE as u64)
        .checked_add(header.metadata_length)
        .ok_or(MALFORMED)?;
    if header.code_offset % ALIGN != 0
        || header.code_offset < metadata_end
        || header.shared_start > header.code_length
        || header.code_length > u64::from(u32::MAX)
        || header.data_length > u64::from(u32::MAX)
        || header.slots > u64::from(u32::MAX) / 8
    {
        return Err(MALFORMED);
    }
    let metadata = bytes
        .get(HEADER_SIZE..metadata_end as usize)
        .ok_or(MALFORMED)?;
    if code_image::stream_hash(metadata) != header.metadata_hash {
        return Err(MALFORMED);
    }
    let mut input = In {
        bytes: metadata,
        at: 0,
    };
    let mut parsed = Parsed {
        header,
        ..Parsed::default()
    };
    let count = input.count()?;
    for _ in 0..count {
        let length = input.count()?;
        let text = input
            .bytes
            .get(input.at..input.at + length)
            .ok_or(MALFORMED)?;
        input.at += length;
        parsed
            .strings
            .push(String::from_utf8(text.to_vec()).map_err(|_| MALFORMED)?);
    }
    let strings = parsed.strings.len();
    let count = input.count()?;
    for _ in 0..count {
        let entry = match input.number()? {
            0 => {
                let tag = input.option(strings)?;
                let fields = match input.number()? {
                    0 => FieldNames::Positional(input.word()?),
                    1 => FieldNames::Named(input.list(strings)?),
                    2 => FieldNames::Dict,
                    3 => FieldNames::Set,
                    _ => return Err(MALFORMED),
                };
                let length = input.count()?;
                let mut items = Vec::with_capacity(length);
                for _ in 0..length {
                    items.push(input.item(&parsed.shapes, true)?);
                }
                ShapeEntry::Shape { tag, fields, items }
            }
            1 => {
                let members = input.list(parsed.shapes.len())?;
                if members.iter().any(|&member| {
                    !matches!(
                        parsed.shapes.get(member as usize),
                        Some(ShapeEntry::Shape { .. })
                    )
                }) {
                    return Err(MALFORMED);
                }
                ShapeEntry::Union(members)
            }
            2 => ShapeEntry::Array(input.item(&parsed.shapes, false)?),
            _ => return Err(MALFORMED),
        };
        parsed.shapes.push(entry);
    }
    let shapes = &parsed.shapes;
    let is_shape = |entry: u32| matches!(shapes.get(entry as usize), Some(ShapeEntry::Shape { .. }));
    let count = input.count()?;
    if count as u64 != header.slots {
        return Err(MALFORMED);
    }
    let mut slots = Vec::with_capacity(count);
    let (mut sites_named, mut callees_named) = (0u32, 0u32);
    for _ in 0..count {
        slots.push(match input.number()? {
            0 => Slot::Helper(
                *Helper::ALL
                    .get(input.number()? as usize)
                    .ok_or(MALFORMED)?,
            ),
            1 => Slot::Interrupt,
            2 => Slot::Shape(input.word().and_then(|entry| {
                is_shape(entry).then_some(entry).ok_or(MALFORMED)
            })?),
            3 => Slot::Constant(input.word().and_then(|entry| {
                is_shape(entry).then_some(entry).ok_or(MALFORMED)
            })?),
            4 => Slot::Data(input.index(header.data_length as usize)?),
            5 => {
                let site = input.word()?;
                sites_named = sites_named.max(site.saturating_add(1));
                Slot::Site(site)
            }
            6 => {
                let callee = input.word()?;
                callees_named = callees_named.max(callee.saturating_add(1));
                Slot::Callee(callee)
            }
            _ => return Err(MALFORMED),
        });
    }
    parsed.slots = slots;
    let count = input.count()?;
    for _ in 0..count {
        let symbol = input.index(strings)?;
        let function = input.flag()?;
        let start = input.env(strings)?;
        let found = input.env(strings)?;
        let class = match input.number()? {
            0 => Class::Base,
            1 => Class::Table,
            2 => Class::Library,
            3 => Class::File {
                package: input.index(strings)?,
                offset: input.number()?,
                position: input.word()?,
                stamp: DatabaseStamp {
                    length: input.number()?,
                    modified: input.number()?,
                },
            },
            4 => Class::Other,
            _ => return Err(MALFORMED),
        };
        parsed.lookups.push(Lookup {
            symbol,
            function,
            start,
            found,
            class,
        });
    }
    let lookups = parsed.lookups.len();
    let count = input.count()?;
    for _ in 0..count {
        let env = input.env(strings)?;
        let call = input.expr(strings, 0)?;
        let function = input.index(lookups)?;
        let length = input.count()?;
        let mut arguments = Vec::with_capacity(length);
        for _ in 0..length {
            let tag = input.option(strings)?;
            let code = input.expr(strings, 0)?;
            let ty = input.ty(&parsed.shapes)?;
            let conversion = if input.flag()? {
                Some(input.conversion(strings, &parsed.shapes, lookups)?)
            } else {
                None
            };
            arguments.push(Argument {
                tag,
                code,
                ty,
                conversion,
            });
        }
        let result = match input.number()? {
            0 => SiteResult::Unused,
            1 => {
                let conversion = input.conversion(strings, &parsed.shapes, lookups)?;
                SiteResult::Converted(conversion, input.ty(&parsed.shapes)?)
            }
            2 => SiteResult::Table,
            _ => return Err(MALFORMED),
        };
        let flags = input.number()?;
        if flags > 3 {
            return Err(MALFORMED);
        }
        parsed.sites.push(Site {
            env,
            call,
            function,
            arguments,
            result,
            diverges: flags & 1 != 0,
            effect_free: flags & 2 != 0,
        });
    }
    let count = input.count()?;
    let mut directs = Vec::new();
    for _ in 0..count {
        let lookup = input.index(lookups)?;
        let params = input.types(&parsed.shapes)?;
        let result = input.ty(&parsed.shapes)?;
        let target = match input.number()? {
            0 => {
                let version = input.word()?;
                directs.push(version);
                Target::Direct(version)
            }
            1 => Target::Slot,
            _ => return Err(MALFORMED),
        };
        parsed.callees.push(FileCallee {
            lookup,
            params,
            result,
            target,
        });
    }
    let count = input.count()?;
    for _ in 0..count {
        let name = input.index(strings)?;
        let offset = input.number()?;
        let position = input.word()?;
        let params = input.types(&parsed.shapes)?;
        let result = input.ty(&parsed.shapes)?;
        let body = input.number()?;
        let trampoline = input.number()?;
        let end = input.number()?;
        let hash = input.number()?;
        let calls_r = input.flag()?;
        let lookups = input.list(lookups)?;
        let sites = input.list(parsed.sites.len())?;
        let callees = input.list(parsed.callees.len())?;
        let confirms = input.list(parsed.shapes.len())?;
        if body >= end
            || trampoline >= end
            || end > header.shared_start
            || confirms.iter().any(|&shape| !is_shape(shape))
        {
            return Err(MALFORMED);
        }
        parsed.functions.push(Version {
            name,
            offset,
            position,
            params,
            result,
            body,
            trampoline,
            end,
            hash,
            calls_r,
            lookups,
            sites,
            callees,
            confirms,
        });
    }
    if input.at != metadata.len()
        || sites_named as usize > parsed.sites.len()
        || callees_named as usize > parsed.callees.len()
        || directs
            .iter()
            .any(|&version| version as usize >= parsed.functions.len())
    {
        return Err(MALFORMED);
    }
    Ok(parsed)
}

// Loading.

thread_local! {
    /// The typed code file of each database this thread attached from, with the stamp the
    /// database was read with. A file that cannot be used is kept as `None`, so it is looked at
    /// once.
    static FILES: RefCell<FxHashMap<u32, (DatabaseStamp, Option<&'static Mapped>)>> =
        RefCell::new(FxHashMap::default());
    /// How many versions this thread attached, which the limit on compiled closures leaves out,
    /// since attaching one costs no compilation.
    static ATTACHED: Cell<usize> = const { Cell::new(0) };
}

/// How many versions this thread attached from typed code files.
pub(crate) fn attached() -> usize {
    ATTACHED.get()
}

/// A typed code file mapped into this process.
struct Mapped {
    parsed: Parsed,
    package: String,
    database: u32,
    /// Where the code starts. The offset table lies below it.
    base: usize,
    /// Per shape entry, this process's id of the shape, union or list item type.
    live: Vec<u32>,
    /// Per version, its argument types and result type in this process.
    params: Vec<SmallVec<[Type; 4]>>,
    results: Vec<Type>,
    /// Per callee, its argument types and result type in this process.
    callee_params: Vec<SmallVec<[Type; 4]>>,
    callee_results: Vec<Type>,
    /// The versions of each closure, by record offset and position.
    by_closure: FxHashMap<(u64, u32), SmallVec<[u32; 2]>>,
    /// The slot of each site and of each callee, `None` when no code reads it.
    site_slots: Vec<Option<u32>>,
    callee_slots: Vec<Option<u32>>,
    states: RefCell<Vec<State>>,
}

#[derive(Clone, Copy)]
enum State {
    Unattached,
    /// Being attached, so a call back into it, which mutual recursion makes, misses.
    Attaching,
    Attached(Sexp, &'static Compiled),
    Failed,
}

/// The code the typed code file of `function`'s database holds for calling it with arguments
/// of types `params`, attached on first use. `None` sends the closure to compile as usual: it
/// was not read from a database with a usable file, the file has no version for these types,
/// or one of the version's assumptions does not hold in this session.
pub(crate) unsafe fn attach(function: Sexp, params: &[Type]) -> Option<&'static Compiled> {
    let note = code_image::typed_note(function)?;
    if misses() {
        return None;
    }
    let mapped = open(note)?;
    let index = mapped
        .by_closure
        .get(&(note.offset, note.position))?
        .iter()
        .copied()
        .find(|&index| mapped.params[index as usize][..] == *params)?;
    attach_version(mapped, index, function)
}

/// Whether `RY_AHEAD_MISS` asks that no code compiled ahead is used, which tests of the
/// fallback use.
fn misses() -> bool {
    static MISSES: OnceLock<bool> = OnceLock::new();
    *MISSES.get_or_init(|| std::env::var_os("RY_AHEAD_MISS").is_some())
}

unsafe fn open(note: Note) -> Option<&'static Mapped> {
    let known = FILES.with_borrow(|files| {
        files
            .get(&note.database)
            .filter(|(stamp, _)| *stamp == note.stamp)
            .map(|(_, mapped)| *mapped)
    });
    if let Some(mapped) = known {
        return mapped;
    }
    let path = code_image::database_path(note.database);
    let package = Path::new(&*path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    let mapped = match open_file(&path, note, package.clone()) {
        Ok(mapped) => Some(&*Box::leak(Box::new(mapped))),
        Err(reason) => {
            if annotations::typed_logging() {
                eprintln!("typed: {package}'s typed code file is not used: {reason}");
            }
            None
        }
    };
    FILES.with_borrow_mut(|files| files.insert(note.database, (note.stamp, mapped)));
    mapped
}

/// The hash of this process's code target, which a file must have been compiled for.
fn target_hash() -> Option<u64> {
    static TARGET: OnceLock<Option<u64>> = OnceLock::new();
    *TARGET.get_or_init(|| {
        Relocatable::new()
            .ok()
            .map(|object| code_image::hash(object.target().as_bytes()))
    })
}

unsafe fn open_file(database: &str, note: Note, package: String) -> Result<Mapped, String> {
    let build = interp::interp().code_image.build;
    if build.is_empty() {
        return Err(String::from("the runtime has no build identity"));
    }
    let path = rcmd::images::typed_code(Path::new(database), build);
    let mut file = std::fs::File::open(&path)
        .map_err(|failure| format!("cannot open {}: {failure}", path.display()))?;
    let length = file
        .metadata()
        .map_err(|failure| format!("cannot read {}: {failure}", path.display()))?
        .len();
    let mut bytes = vec![0; HEADER_SIZE];
    file.read_exact(&mut bytes).map_err(|_| MALFORMED)?;
    let metadata_length = bytes
        .get(12 + 8 * 4..12 + 8 * 5)
        .and_then(|word| word.try_into().ok())
        .map_or(u64::MAX, u64::from_le_bytes);
    if metadata_length > length {
        return Err(String::from(MALFORMED));
    }
    bytes.resize(HEADER_SIZE + metadata_length as usize, 0);
    file.read_exact(&mut bytes[HEADER_SIZE..])
        .map_err(|_| MALFORMED)?;
    let parsed = parse(&bytes)?;
    let header = parsed.header;
    if header.build != code_image::hash(build.as_bytes()) {
        return Err(String::from("another build wrote it"));
    }
    if Some(header.target) != target_hash() {
        return Err(String::from("its code is for another processor"));
    }
    if header.stamp != note.stamp {
        return Err(String::from("it was written for another copy of the package"));
    }
    let tail = header.code_length.next_multiple_of(8) + header.data_length;
    if header.code_offset.checked_add(tail) != Some(length) {
        return Err(String::from(MALFORMED));
    }
    if parsed.functions.is_empty() {
        return Err(String::from("it holds no code"));
    }
    let live = live_shapes(&parsed)?;
    let base = map(&file, &header)?;
    // SAFETY: `map` mapped `tail` bytes of the file at `base`.
    let shared = std::slice::from_raw_parts(
        (base + header.shared_start as usize) as *const u8,
        (tail - header.shared_start) as usize,
    );
    if code_image::stream_hash(shared) != header.shared_hash {
        return Err(String::from(MALFORMED));
    }
    let data = base + header.code_length.next_multiple_of(8) as usize;
    let mut site_slots = vec![None; parsed.sites.len()];
    let mut callee_slots = vec![None; parsed.callees.len()];
    for (number, slot) in parsed.slots.iter().enumerate() {
        let number = number as u32;
        let word = match *slot {
            Slot::Helper(helper) => helper_address(helper) as u64,
            Slot::Interrupt => {
                &interp::unwind::INTERRUPT_REQUESTED as *const std::sync::atomic::AtomicBool as u64
            }
            Slot::Shape(entry) => u64::from(live[entry as usize]),
            Slot::Constant(entry) => shapes::constant(live[entry as usize]),
            Slot::Data(offset) => (data + offset as usize) as u64,
            Slot::Site(site) => {
                site_slots[site as usize] = Some(number);
                0
            }
            Slot::Callee(callee) => {
                callee_slots[callee as usize] = Some(number);
                0
            }
        };
        write_slot(base, number, word);
    }
    let types = |types: &[Type]| -> SmallVec<[Type; 4]> {
        types.iter().map(|&ty| live_type(ty, &live)).collect()
    };
    let params = parsed
        .functions
        .iter()
        .map(|version| types(&version.params))
        .collect();
    let results = parsed
        .functions
        .iter()
        .map(|version| live_type(version.result, &live))
        .collect();
    let callee_params = parsed
        .callees
        .iter()
        .map(|callee| types(&callee.params))
        .collect();
    let callee_results = parsed
        .callees
        .iter()
        .map(|callee| live_type(callee.result, &live))
        .collect();
    let mut by_closure: FxHashMap<(u64, u32), SmallVec<[u32; 2]>> = FxHashMap::default();
    for (index, version) in parsed.functions.iter().enumerate() {
        by_closure
            .entry((version.offset, version.position))
            .or_default()
            .push(index as u32);
    }
    let states = RefCell::new(vec![State::Unattached; parsed.functions.len()]);
    Ok(Mapped {
        parsed,
        package,
        database: note.database,
        base,
        live,
        params,
        results,
        callee_params,
        callee_results,
        by_closure,
        site_slots,
        callee_slots,
        states,
    })
}

/// Maps the file's code and data at the address it returns, below which it reserves a
/// writable offset table of the file's slots. The mapping lives as long as the process, since
/// attached code may run until it ends.
#[cfg(target_os = "linux")]
unsafe fn map(file: &std::fs::File, header: &Header) -> Result<usize, String> {
    use std::os::fd::AsRawFd;
    let table = (header.slots * 8).next_multiple_of(ALIGN) as usize;
    let tail = (header.code_length.next_multiple_of(8) + header.data_length) as usize;
    let span = table + tail.next_multiple_of(ALIGN as usize);
    let failed = |what: &str| format!("cannot map its {what}: {}", std::io::Error::last_os_error());
    let reserved = libc::mmap(
        std::ptr::null_mut(),
        span,
        libc::PROT_NONE,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        -1,
        0,
    );
    if reserved == libc::MAP_FAILED {
        return Err(failed("code"));
    }
    if table > 0 && libc::mprotect(reserved, table, libc::PROT_READ | libc::PROT_WRITE) != 0 {
        let error = failed("offset table");
        libc::munmap(reserved, span);
        return Err(error);
    }
    let code = reserved.cast::<u8>().add(table);
    let mapped = libc::mmap(
        code.cast(),
        tail,
        libc::PROT_READ | libc::PROT_EXEC,
        libc::MAP_PRIVATE | libc::MAP_FIXED,
        file.as_raw_fd(),
        header.code_offset as libc::off_t,
    );
    if mapped == libc::MAP_FAILED {
        let error = failed("code");
        libc::munmap(reserved, span);
        return Err(error);
    }
    Ok(code as usize)
}

#[cfg(not(target_os = "linux"))]
unsafe fn map(_: &std::fs::File, _: &Header) -> Result<usize, String> {
    Err(String::from("this system does not map typed code files"))
}

/// Writes slot `number` of the offset table below `base`.
unsafe fn write_slot(base: usize, number: u32, word: u64) {
    // SAFETY: `map` reserved a writable table below `base` with room for every slot the file
    // names.
    (base as *mut u64).sub(number as usize + 1).write(word);
}

/// This process's id of each of the file's shape entries, interning the ones it has not seen.
unsafe fn live_shapes(parsed: &Parsed) -> Result<Vec<u32>, String> {
    let heap = interp::heap();
    let symbol = |index: u32| {
        rvalues::install(&parsed.strings[index as usize])
            .map_err(|_| String::from("cannot name a field"))
    };
    let mut live: Vec<u32> = Vec::with_capacity(parsed.shapes.len());
    for entry in &parsed.shapes {
        let id = match entry {
            ShapeEntry::Shape { tag, fields, items } => {
                let tag = match tag {
                    Some(name) => Some(heap.layouts.intern_tag(symbol(*name)?)),
                    None => None,
                };
                let fields = match fields {
                    FieldNames::Positional(count) => Fields::Positional(*count),
                    FieldNames::Named(names) => {
                        // A layout keeps its names in the order of their bytes.
                        if names.windows(2).any(|pair| {
                            parsed.strings[pair[0] as usize] >= parsed.strings[pair[1] as usize]
                        }) {
                            return Err(String::from(MALFORMED));
                        }
                        let mut symbols = Vec::with_capacity(names.len());
                        for &name in names {
                            symbols.push(symbol(name)?);
                        }
                        Fields::Named(symbols.into_boxed_slice())
                    }
                    FieldNames::Dict => Fields::Dict,
                    FieldNames::Set => Fields::Set,
                };
                let layout = heap.layouts.intern(Layout { tag, fields });
                let items: SmallVec<[Item; 4]> =
                    items.iter().map(|&item| live_item(item, &live)).collect();
                shapes::intern(layout, &items)
                    .ok_or_else(|| String::from("a shape too large for a box"))?
            }
            ShapeEntry::Union(members) => {
                let members: SmallVec<[u32; 4]> =
                    members.iter().map(|&member| live[member as usize]).collect();
                shapes::union_of(&members)
            }
            ShapeEntry::Array(item) => shapes::array_of(live_item(*item, &live)),
        };
        live.push(id);
    }
    Ok(live)
}

fn live_item(item: Item, live: &[u32]) -> Item {
    match item {
        Item::Record(entry) => Item::Record(live[entry as usize]),
        Item::Tag(shapes::ANY) => Item::Tag(shapes::ANY),
        Item::Tag(entry) => Item::Tag(live[entry as usize]),
        Item::Array(entry) => Item::Array(live[entry as usize]),
        item => item,
    }
}

fn live_type(ty: Type, live: &[u32]) -> Type {
    match ty {
        Type::Vector { item, array } => Type::Vector {
            item: live_item(item, live),
            array,
        },
        Type::List(item) => Type::List(live_item(item, live)),
        Type::Set(item) => Type::Set(live_item(item, live)),
        Type::Dict { key, value } => Type::Dict {
            key: live_item(key, live),
            value: live_item(value, live),
        },
        Type::Record(entry) => Type::Record(live[entry as usize]),
        Type::Tag(shapes::ANY) => Type::Tag(shapes::ANY),
        Type::Tag(entry) => Type::Tag(live[entry as usize]),
        ty => ty,
    }
}

unsafe fn attach_version(
    mapped: &'static Mapped,
    index: u32,
    closure: Sexp,
) -> Option<&'static Compiled> {
    let state = mapped.states.borrow()[index as usize];
    match state {
        State::Attached(attached, compiled) => {
            return (attached == closure && crate::still_resolves(compiled)).then_some(compiled);
        }
        State::Attaching | State::Failed => return None,
        State::Unattached => {}
    }
    mapped.states.borrow_mut()[index as usize] = State::Attaching;
    let attached = attach_inner(mapped, index);
    let version = &mapped.parsed.functions[index as usize];
    let name = &mapped.parsed.strings[version.name as usize];
    let params = &mapped.params[index as usize];
    let compiled = match attached {
        Ok(compiled) => compiled,
        Err(reason) => {
            mapped.states.borrow_mut()[index as usize] = State::Failed;
            if annotations::typed_logging() {
                eprintln!(
                    "typed: `{name}` for {params:?} is not attached from {}'s code file: {reason}",
                    mapped.package
                );
            }
            return None;
        }
    };
    mapped.states.borrow_mut()[index as usize] = State::Attached(closure, compiled);
    interp::heap().add_global_root(closure);
    COMPILED.with_borrow_mut(|table| table.insert((closure, params.clone()), Ok(compiled)));
    ATTACHED.set(ATTACHED.get() + 1);
    if annotations::typed_logging() {
        eprintln!(
            "typed: attached `{name}` for {params:?} -> {:?} from {}'s code file",
            compiled.result, mapped.package
        );
    }
    Some(compiled)
}

unsafe fn attach_inner(mapped: &'static Mapped, index: u32) -> Result<&'static Compiled, &'static str> {
    let version = &mapped.parsed.functions[index as usize];
    // SAFETY: parsing checked that the version's code lies inside the mapped code.
    let code = std::slice::from_raw_parts(
        (mapped.base + version.body as usize) as *const u8,
        (version.end - version.body) as usize,
    );
    if code_image::hash(code) != version.hash {
        return Err("its code does not match the file");
    }
    let mut resolved: FxHashMap<u32, Speculation> = FxHashMap::default();
    let mut speculations = Vec::with_capacity(version.lookups.len());
    for &lookup in &version.lookups {
        speculations.push(found(mapped, &mut resolved, lookup)?);
    }
    for &site in &version.sites {
        let call = build_site(mapped, &mut resolved, site)?;
        if let Some(slot) = mapped.site_slots[site as usize] {
            write_slot(mapped.base, slot, call as *const RCall as u64);
        }
    }
    let mut epoch = None;
    let mut calls_r = version.calls_r;
    for &callee in &version.callees {
        let entry = &mapped.parsed.callees[callee as usize];
        let function = found(mapped, &mut resolved, entry.lookup)?.function;
        let params = &mapped.callee_params[callee as usize];
        let compiled = match entry.target {
            Target::Direct(target) => {
                let wanted = &mapped.parsed.functions[target as usize];
                let same = code_image::typed_note(function).is_some_and(|note| {
                    note.database == mapped.database
                        && note.offset == wanted.offset
                        && note.position == wanted.position
                });
                if !same || mapped.params[target as usize] != *params {
                    return Err("a function it calls is bound to another function");
                }
                attach_version(mapped, target, function)
                    .ok_or("a function it calls is not attached")?
            }
            Target::Slot => {
                let compiled = compiled_or_refusal(function, params)
                    .map_err(|_| "a function it calls does not compile")?;
                if let Some(slot) = mapped.callee_slots[callee as usize] {
                    write_slot(mapped.base, slot, compiled.code.address as u64);
                }
                compiled
            }
        };
        if compiled.result != mapped.callee_results[callee as usize] {
            return Err("a function it calls returns another type");
        }
        speculations.extend_from_slice(&compiled.speculations);
        epoch = earliest(epoch, compiled.epoch);
        calls_r |= compiled.calls_r;
    }
    for &shape in &version.confirms {
        shapes::confirm(mapped.live[shape as usize]);
    }
    Ok(Box::leak(Box::new(Compiled {
        code: Code {
            address: mapped.base + version.body as usize,
            trampoline: mapped.base + version.trampoline as usize,
        },
        params: mapped.params[index as usize].clone(),
        result: mapped.results[index as usize],
        speculations,
        epoch,
        calls_r,
    })))
}

/// The binding lookup `index` finds in this session, when it finds what the file assumed.
unsafe fn found(
    mapped: &Mapped,
    resolved: &mut FxHashMap<u32, Speculation>,
    index: u32,
) -> Result<Speculation, &'static str> {
    if let Some(&speculation) = resolved.get(&index) {
        return Ok(speculation);
    }
    let speculation = resolve(mapped, index).ok_or("a lookup it assumes finds something else")?;
    resolved.insert(index, speculation);
    Ok(speculation)
}

unsafe fn resolve(mapped: &Mapped, index: u32) -> Option<Speculation> {
    let lookup = &mapped.parsed.lookups[index as usize];
    let strings = &mapped.parsed.strings;
    let symbol = rvalues::install(&strings[lookup.symbol as usize]).ok()?;
    let start = live_env(mapped, &lookup.start)?;
    let speculation = if lookup.function {
        lower::resolve_function(symbol, start)?
    } else {
        lower::resolve_value(symbol, start)?
    };
    let found = live_env(mapped, &lookup.found)?;
    let heap = interp::heap();
    if heap.frame_cell(found, symbol) != Some(speculation.cell) {
        return None;
    }
    let function = speculation.function;
    let holds = match &lookup.class {
        Class::Library => {
            interp::standard_library::builtins_env()
                .and_then(|env| heap.frame_cell(env, symbol))
                .map(|cell| cell.cdr())
                == Some(function)
        }
        Class::Base => lower::is_base_function(&speculation),
        Class::Table => lower::is_table_function(&speculation),
        Class::File {
            package,
            offset,
            position,
            stamp,
        } => {
            annotations::is_typed(function)
                && code_image::typed_note(function).is_some_and(|note| {
                    let path = code_image::database_path(note.database);
                    note.offset == *offset
                        && note.position == *position
                        && note.stamp == *stamp
                        && Path::new(&*path)
                            .file_stem()
                            .is_some_and(|stem| *stem == *strings[*package as usize])
                })
        }
        Class::Other => rvalues::is_function(function) && !annotations::is_typed(function),
    };
    holds.then_some(speculation)
}

/// This session's environment for `env`.
unsafe fn live_env(mapped: &Mapped, env: &EnvRef) -> Option<Sexp> {
    let name = |index: &u32| mapped.parsed.strings[*index as usize].as_str();
    let named = |env: Sexp, wanted: &str| {
        rvalues::first_string(rvalues::get_attr(env, rvalues::symbols().name))
            .is_some_and(|name| name == wanted)
    };
    Some(match env {
        EnvRef::Global => rvalues::global_env(),
        EnvRef::BaseEnv => rvalues::base_env(),
        EnvRef::BaseNamespace => rvalues::base_namespace(),
        EnvRef::StandardLibrary => interp::standard_library::builtins_env()?,
        EnvRef::Namespace(package) => loaded_namespace(rvalues::install(name(package)).ok()?)?,
        EnvRef::Imports(package) => {
            let imports =
                loaded_namespace(rvalues::install(name(package)).ok()?)?.enclosure();
            if !named(imports, &format!("imports:{}", name(package))) {
                return None;
            }
            imports
        }
        EnvRef::Package(wanted) => {
            let empty = interp::heap().known().empty_env;
            let mut env = rvalues::global_env().enclosure();
            while env != empty {
                if named(env, name(wanted)) {
                    return Some(env);
                }
                env = env.enclosure();
            }
            return None;
        }
    })
}

/// The namespace of the package named `package`, when it is loaded.
unsafe fn loaded_namespace(package: Sexp) -> Option<Sexp> {
    let registry = interp::heap().known().namespace_registry;
    let namespace = interp::eval::find_var_in_frame(package, registry).ok()?;
    (namespace.kind() == SexpType::Environment).then_some(namespace)
}

/// The R call site `site` of the file, built for this session and kept for the rest of it.
unsafe fn build_site(
    mapped: &Mapped,
    resolved: &mut FxHashMap<u32, Speculation>,
    site: u32,
) -> Result<&'static RCall, &'static str> {
    let site = &mapped.parsed.sites[site as usize];
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let built = build_site_inner(mapped, resolved, site);
    heap.release_handles(mark);
    built.map(|call| &*Box::leak(Box::new(call)))
}

unsafe fn build_site_inner(
    mapped: &Mapped,
    resolved: &mut FxHashMap<u32, Speculation>,
    site: &Site,
) -> Result<RCall, &'static str> {
    const UNBUILT: &str = "the session cannot build an R call it makes";
    let heap = interp::heap();
    let strings = &mapped.parsed.strings;
    // Everything the site holds lives as long as the code that may use it, which is the session.
    let mut keep = |value: Sexp| {
        heap.add_global_root(value);
        value
    };
    let conversion = |conversion: &FileConversion,
                          resolved: &mut FxHashMap<u32, Speculation>|
     -> Result<Conversion, &'static str> {
        let function = found(mapped, resolved, conversion.function)?.function;
        Ok(Conversion {
            call: build_expr(strings, &conversion.call).ok_or(UNBUILT)?,
            function,
            checked: conversion.checked.map(|item| live_item(item, &mapped.live)),
        })
    };
    let mut arguments: SmallVec<[RArgument; 4]> = SmallVec::new();
    for argument in &site.arguments {
        let tag = match argument.tag {
            Some(tag) => rvalues::install(&strings[tag as usize]).map_err(|_| UNBUILT)?,
            None => rvalues::nil(),
        };
        let converted = match &argument.conversion {
            Some(converted) => {
                let converted = conversion(converted, resolved)?;
                keep(converted.call);
                Some(converted)
            }
            None => None,
        };
        arguments.push(RArgument {
            tag,
            code: keep(build_expr(strings, &argument.code).ok_or(UNBUILT)?),
            ty: live_type(argument.ty, &mapped.live),
            conversion: converted,
        });
    }
    let result = match &site.result {
        SiteResult::Unused => RResult::Unused,
        SiteResult::Converted(converted, ty) => {
            let converted = conversion(converted, resolved)?;
            keep(converted.call);
            RResult::Converted(converted, live_type(*ty, &mapped.live))
        }
        SiteResult::Table => RResult::Table,
    };
    Ok(RCall {
        function: found(mapped, resolved, site.function)?.function,
        call: keep(build_expr(strings, &site.call).ok_or(UNBUILT)?),
        env: live_env(mapped, &site.env).ok_or("an R call it makes runs in a missing environment")?,
        arguments,
        result,
        diverges: site.diverges,
        effect_free: site.effect_free,
    })
}

/// The R code `expr` stands for, rooted in the handle scope under way.
unsafe fn build_expr(strings: &[String], expr: &Expr) -> Option<Sexp> {
    let heap = interp::heap();
    let value = match expr {
        Expr::Nil => return Some(rvalues::nil()),
        Expr::Missing => return Some(rvalues::missing_arg()),
        Expr::Symbol(name) => return rvalues::install(&strings[*name as usize]).ok(),
        Expr::Str(text) => rvalues::scalar_string(&strings[*text as usize]).ok()?,
        Expr::Dbl(bits) => rvalues::scalar_real(f64::from_bits(*bits)).ok()?,
        Expr::Int(value) => rvalues::scalar_integer(*value).ok()?,
        Expr::Lgl(value) => rvalues::scalar_logical(*value).ok()?,
        Expr::Call(items) => {
            let mut list = rvalues::nil();
            for (position, (tag, item)) in items.iter().enumerate().rev() {
                let value = build_expr(strings, item)?;
                let tag = match tag {
                    Some(tag) => rvalues::install(&strings[*tag as usize]).ok()?,
                    None => rvalues::nil(),
                };
                list = if position == 0 {
                    let call = rvalues::lcons(value, list).ok()?;
                    if tag != rvalues::nil() {
                        heap.set_tag(call, tag);
                    }
                    call
                } else {
                    rvalues::cons_tagged(value, list, tag).ok()?
                };
                heap.root(list);
            }
            list
        }
    };
    heap.root(value);
    Some(value)
}

/// Checks of the file format for the fuzz tests and fuzz targets: random valid metadata must
/// read back as written, and corrupt bytes must parse or fail with [`MALFORMED`], never panic.
pub mod testing {
    use {
        super::{
            ALIGN, Argument, Class, EnvRef, Expr, FieldNames, FileCallee, FileConversion,
            HEADER_SIZE, Header, Item, Lookup, Parsed, ShapeEntry, Site, SiteResult, Target,
            Version, encode_header, encode_metadata, parse,
        },
        crate::{
            backend::{Helper, Slot},
            ir::Type,
            shapes,
        },
        interp::code_image::{self, DatabaseStamp},
        syntax::testing::Entropy,
    };

    /// Writes random valid metadata, parses it back, then parses corruptions of it.
    pub fn check_round_trip(entropy: &mut impl Entropy) {
        let parsed = random_parsed(entropy);
        let bytes = file_bytes(&parsed);
        match parse(&bytes) {
            Ok(read) => assert_eq!(read, parsed, "metadata reads back as written"),
            Err(message) => panic!("written metadata must parse: {message}\n{parsed:#?}"),
        }
        for _ in 0..1 + entropy.below(4) {
            let mut corrupt = bytes.clone();
            match entropy.below(3) {
                0 => corrupt.truncate(entropy.below(bytes.len())),
                1 => {
                    let at = entropy.below(corrupt.len());
                    corrupt[at] ^= 1 << entropy.below(8);
                }
                _ => {
                    // Corrupt the metadata and fix its hash, so the parser reads the bytes.
                    let end = HEADER_SIZE + parsed.header.metadata_length as usize;
                    if end > HEADER_SIZE {
                        let at = HEADER_SIZE + entropy.below(end - HEADER_SIZE);
                        corrupt[at] = entropy.below(256) as u8;
                        let hash = code_image::stream_hash(&corrupt[HEADER_SIZE..end]);
                        let word = 12 + 8 * 10;
                        corrupt[word..word + 8].copy_from_slice(&hash.to_le_bytes());
                    }
                }
            }
            check_parse_input(&corrupt);
        }
    }

    /// Parses arbitrary bytes, which must give metadata or an error.
    pub fn check_parse_input(bytes: &[u8]) {
        if let Ok(parsed) = parse(bytes) {
            // Every index the loader follows is in range.
            let in_range = |indices: &[u32], length: usize| {
                indices.iter().all(|&index| (index as usize) < length)
            };
            for version in &parsed.functions {
                assert!(in_range(&version.sites, parsed.sites.len()));
                assert!(in_range(&version.callees, parsed.callees.len()));
                assert!(in_range(&version.lookups, parsed.lookups.len()));
                assert!(version.body < version.end && version.end <= parsed.header.shared_start);
            }
        }
    }

    /// The file's bytes up to its code: the header and the metadata, padded to the code.
    fn file_bytes(parsed: &Parsed) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode_header(&parsed.header, &mut bytes);
        bytes.extend_from_slice(&encode_metadata(parsed));
        bytes
    }

    fn random_parsed(entropy: &mut impl Entropy) -> Parsed {
        let mut parsed = Parsed::default();
        for index in 0..1 + entropy.below(6) {
            parsed.strings.push(format!("name{index}"));
        }
        let strings = parsed.strings.len();
        for _ in 0..entropy.below(6) {
            let shapes = &parsed.shapes;
            let records: Vec<u32> = indices(shapes, |entry| matches!(entry, ShapeEntry::Shape { .. }));
            let entry = match entropy.below(3) {
                1 if !records.is_empty() => {
                    let count = 1 + entropy.below(records.len());
                    ShapeEntry::Union(
                        (0..count)
                            .map(|_| records[entropy.below(records.len())])
                            .collect(),
                    )
                }
                2 => ShapeEntry::Array(random_item(entropy, shapes)),
                _ => {
                    let fields = match entropy.below(4) {
                        0 => FieldNames::Positional(entropy.below(4) as u32),
                        1 => FieldNames::Named(
                            (0..entropy.below(3))
                                .map(|_| entropy.below(strings) as u32)
                                .collect(),
                        ),
                        2 => FieldNames::Dict,
                        _ => FieldNames::Set,
                    };
                    let items = (0..entropy.below(3))
                        .map(|_| match random_item(entropy, shapes) {
                            Item::Tag(_) => Item::Tag(shapes::ANY),
                            item => item,
                        })
                        .collect();
                    ShapeEntry::Shape {
                        tag: entropy.chance(1, 2).then(|| entropy.below(strings) as u32),
                        fields,
                        items,
                    }
                }
            };
            parsed.shapes.push(entry);
        }
        let records = indices(&parsed.shapes, |entry| {
            matches!(entry, ShapeEntry::Shape { .. })
        });
        for _ in 0..entropy.below(5) {
            let lookup = Lookup {
                symbol: entropy.below(strings) as u32,
                function: entropy.chance(1, 2),
                start: random_env(entropy, strings),
                found: random_env(entropy, strings),
                class: match entropy.below(5) {
                    0 => Class::Base,
                    1 => Class::Table,
                    2 => Class::Library,
                    3 => Class::File {
                        package: entropy.below(strings) as u32,
                        offset: entropy.next_u64(),
                        position: entropy.below(9) as u32,
                        stamp: DatabaseStamp {
                            length: entropy.next_u64(),
                            modified: entropy.next_u64(),
                        },
                    },
                    _ => Class::Other,
                },
            };
            parsed.lookups.push(lookup);
        }
        let lookups = parsed.lookups.len();
        if lookups > 0 {
            for _ in 0..entropy.below(4) {
                let conversion = |entropy: &mut dyn FnMut(usize) -> usize| FileConversion {
                    call: Expr::Symbol(entropy(strings) as u32),
                    function: entropy(lookups) as u32,
                    checked: None,
                };
                let mut below = |bound: usize| entropy.below(bound);
                let arguments = (0..below(3))
                    .map(|_| Argument {
                        tag: (below(2) == 0).then(|| below(strings) as u32),
                        code: Expr::Symbol(below(strings) as u32),
                        ty: Type::Dbl,
                        conversion: (below(2) == 0).then(|| conversion(&mut below)),
                    })
                    .collect();
                let result = match below(3) {
                    0 => SiteResult::Unused,
                    1 => SiteResult::Converted(conversion(&mut below), Type::I64),
                    _ => SiteResult::Table,
                };
                let site = Site {
                    env: random_env(entropy, strings),
                    call: random_expr(entropy, strings, 0),
                    function: entropy.below(lookups) as u32,
                    arguments,
                    result,
                    diverges: entropy.chance(1, 2),
                    effect_free: entropy.chance(1, 2),
                };
                parsed.sites.push(site);
            }
        }
        let data_length = entropy.below(64) as u64;
        let slot_count = entropy.below(8);
        for _ in 0..slot_count {
            let slot = match entropy.below(7) {
                0 => Slot::Helper(Helper::ALL[entropy.below(Helper::ALL.len())]),
                2 if !records.is_empty() => Slot::Shape(records[entropy.below(records.len())]),
                3 if !records.is_empty() => {
                    Slot::Constant(records[entropy.below(records.len())])
                }
                4 if data_length > 0 => Slot::Data(entropy.below(data_length as usize) as u32),
                5 if !parsed.sites.is_empty() => {
                    Slot::Site(entropy.below(parsed.sites.len()) as u32)
                }
                _ => Slot::Interrupt,
            };
            parsed.slots.push(slot);
        }
        let function_count = entropy.below(4);
        if lookups > 0 {
            for _ in 0..entropy.below(3) {
                let target = if function_count > 0 && entropy.chance(1, 2) {
                    Target::Direct(entropy.below(function_count) as u32)
                } else {
                    Target::Slot
                };
                parsed.callees.push(FileCallee {
                    lookup: entropy.below(lookups) as u32,
                    params: (0..entropy.below(3))
                        .map(|_| random_type(entropy, &parsed.shapes))
                        .collect(),
                    result: random_type(entropy, &parsed.shapes),
                    target,
                });
            }
        }
        if !parsed.callees.is_empty() && entropy.chance(1, 2) {
            parsed
                .slots
                .push(Slot::Callee(entropy.below(parsed.callees.len()) as u32));
        }
        let code_length = 64 + entropy.below(4096) as u64;
        let shared_start = 32 + entropy.below(code_length as usize - 32) as u64;
        for _ in 0..function_count {
            let end = 2 + entropy.below(shared_start as usize - 1) as u64;
            let pick = |entropy: &mut dyn FnMut(usize) -> usize, length: usize| -> Vec<u32> {
                if length == 0 {
                    return Vec::new();
                }
                (0..entropy(3)).map(|_| entropy(length) as u32).collect()
            };
            let mut below = |bound: usize| entropy.below(bound);
            let version = Version {
                name: below(strings) as u32,
                offset: below(1 << 20) as u64,
                position: below(4) as u32,
                params: SmallVec::new(),
                result: Type::Dbl,
                body: below(end as usize - 1) as u64,
                trampoline: below(end as usize - 1) as u64,
                end,
                hash: below(usize::MAX) as u64,
                calls_r: below(2) == 0,
                lookups: pick(&mut below, lookups),
                sites: pick(&mut below, parsed.sites.len()),
                callees: pick(&mut below, parsed.callees.len()),
                confirms: if records.is_empty() {
                    Vec::new()
                } else {
                    vec![records[below(records.len())]]
                },
            };
            parsed.functions.push(version);
        }
        let metadata = encode_metadata(&parsed);
        let metadata_end = (HEADER_SIZE + metadata.len()) as u64;
        parsed.header = Header {
            build: entropy.next_u64(),
            stamp: DatabaseStamp {
                length: entropy.next_u64(),
                modified: entropy.next_u64(),
            },
            target: entropy.next_u64(),
            metadata_length: metadata.len() as u64,
            slots: parsed.slots.len() as u64,
            code_offset: metadata_end.div_ceil(ALIGN) * ALIGN,
            code_length,
            data_length,
            shared_start,
            metadata_hash: code_image::stream_hash(&metadata),
            shared_hash: entropy.next_u64(),
        };
        parsed
    }

    use smallvec::SmallVec;

    fn indices(shapes: &[ShapeEntry], wanted: impl Fn(&ShapeEntry) -> bool) -> Vec<u32> {
        (0..shapes.len() as u32)
            .filter(|&index| wanted(&shapes[index as usize]))
            .collect()
    }

    fn random_item(entropy: &mut impl Entropy, shapes: &[ShapeEntry]) -> Item {
        let pick = |entropy: &mut dyn FnMut(usize) -> usize, wanted: fn(&ShapeEntry) -> bool| {
            let found = indices(shapes, wanted);
            (!found.is_empty()).then(|| found[entropy(found.len())])
        };
        let mut below = |bound: usize| entropy.below(bound);
        match below(8) {
            0 => Item::Int,
            1 => Item::Dbl,
            2 => Item::Lgl,
            3 => Item::Str,
            5 => pick(&mut below, |entry| matches!(entry, ShapeEntry::Shape { .. }))
                .map_or(Item::I64, Item::Record),
            6 => pick(&mut below, |entry| matches!(entry, ShapeEntry::Union(_)))
                .map_or(Item::Tag(shapes::ANY), Item::Tag),
            7 => pick(&mut below, |entry| matches!(entry, ShapeEntry::Array(_)))
                .map_or(Item::I64, Item::Array),
            _ => Item::I64,
        }
    }

    fn random_type(entropy: &mut impl Entropy, shapes: &[ShapeEntry]) -> Type {
        match entropy.below(14) {
            0 => Type::Int,
            1 => Type::Dbl,
            2 => Type::Lgl,
            3 => Type::Bool,
            4 => Type::Str,
            6 => Type::Null,
            7 => Type::Vector {
                item: random_item(entropy, shapes),
                array: entropy.chance(1, 2),
            },
            8 => Type::List(random_item(entropy, shapes)),
            9 => Type::Set(random_item(entropy, shapes)),
            10 => Type::Dict {
                key: random_item(entropy, shapes),
                value: random_item(entropy, shapes),
            },
            11 => match random_item(entropy, shapes) {
                Item::Record(entry) => Type::Record(entry),
                _ => Type::Table,
            },
            12 => match random_item(entropy, shapes) {
                Item::Tag(entry) => Type::Tag(entry),
                _ => Type::Tag(shapes::ANY),
            },
            13 => Type::Table,
            _ => Type::I64,
        }
    }

    fn random_env(entropy: &mut impl Entropy, strings: usize) -> EnvRef {
        match entropy.below(7) {
            0 => EnvRef::Global,
            1 => EnvRef::BaseEnv,
            2 => EnvRef::BaseNamespace,
            3 => EnvRef::StandardLibrary,
            4 => EnvRef::Namespace(entropy.below(strings) as u32),
            5 => EnvRef::Imports(entropy.below(strings) as u32),
            _ => EnvRef::Package(entropy.below(strings) as u32),
        }
    }

    fn random_expr(entropy: &mut impl Entropy, strings: usize, depth: usize) -> Expr {
        match entropy.below(if depth < 3 { 8 } else { 7 }) {
            0 => Expr::Nil,
            1 => Expr::Missing,
            2 => Expr::Symbol(entropy.below(strings) as u32),
            3 => Expr::Str(entropy.below(strings) as u32),
            4 => Expr::Dbl(entropy.next_u64()),
            5 => Expr::Int(entropy.next_u64() as i32),
            6 => Expr::Lgl(entropy.below(3) as i32 - 1),
            _ => Expr::Call(
                (0..1 + entropy.below(3))
                    .map(|_| {
                        (
                            entropy.chance(1, 3).then(|| entropy.below(strings) as u32),
                            random_expr(entropy, strings, depth + 1),
                        )
                    })
                    .collect(),
            ),
        }
    }
}
