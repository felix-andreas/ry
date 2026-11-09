//! Machine code written ahead of time: the section format a code image carries, the linker that
//! builds it from a compiled object, and the loader that maps it.
//!
//! The code never holds an absolute address, because the process that runs it places the
//! runtime, the heap and the code itself at addresses the install could not know. A call between
//! the section's own functions is relative to the caller, and the linker resolves it at install
//! time. Every other address (a runtime helper, a flag the code polls) is read from the section's
//! link table, one 64-bit slot per symbol name, which sits at a fixed distance past the code, so
//! the instructions that read it are resolved at install time too. Loading then only fills the
//! link table: the code pages are mapped straight from the file, read-only, and every process
//! that maps the same file shares them through the page cache.
//!
//! The section layout, all integers little-endian:
//!
//! | Field | Content |
//! |---|---|
//! | magic | `RYNATIVE` |
//! | target | `u32`, [`TARGET_X86_64_V2`] |
//! | hash | `u64`, the FNV-1a hash of every byte after this field, so damaged code never runs |
//! | code offset | `u32`, where the code starts, counted from the section's start |
//! | code length | `u32` |
//! | symbol count | `u32`, then per symbol its name as a `u16` length and the bytes |
//! | function count | `u32`, then per function its name as a `u16` length and the bytes, and the `u32` offset of its entry in the code |
//! | metadata | `u32` length and the bytes: what the front end that compiled the code says about it |
//! | padding | zeros up to the code offset, which the writer places at a multiple of [`FILE_ALIGNMENT`] in the file |
//! | code | the machine code |

use {
    crate::lower::Error,
    object::{
        Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationKind, RelocationTarget,
        SymbolKind,
    },
    rustc_hash::FxHashMap,
    std::path::Path,
};

const MAGIC: &[u8; 8] = b"RYNATIVE";

/// Where the hashed bytes start: past the magic, the target and the hash itself.
const HASHED_FROM: usize = 20;

/// x86-64 code that needs the features of the x86-64-v2 level (SSE4.2, POPCNT, CMPXCHG16B),
/// which every x86-64 processor made since about 2009 has.
pub const TARGET_X86_64_V2: u32 = 1;

/// Where the code starts in the image file. A multiple of every page size the loader may meet
/// lets it map the code from the file rather than copy it.
pub const FILE_ALIGNMENT: u64 = 16 * 1024;

/// How far past the code's start, rounded up, the link table lies in memory. The code reads the
/// table relative to itself, so the distance is fixed when the section is linked, and a multiple
/// of every page size lets the loader protect the code and the table separately.
const TABLE_ALIGNMENT: usize = 64 * 1024;

/// The most symbols or functions one section may hold.
const MAX_NAMES: usize = 1 << 20;

/// Linked machine code: the bytes, the symbol behind each link table slot, each function's
/// name and entry offset, and the front end's metadata, which the section carries opaquely.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Code {
    pub text: Vec<u8>,
    pub symbols: Vec<Box<str>>,
    pub functions: Vec<(Box<str>, u32)>,
    pub metadata: Vec<u8>,
}

/// The ISA builder for code written ahead of time on this host. Only Linux on x86-64 has a
/// linker and a loader here.
pub(crate) fn isa_builder() -> Result<cranelift_codegen::isa::Builder, Error> {
    use cranelift_codegen::settings::Configurable;
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return Err(Error(
            "code written ahead of time exists only for Linux on x86-64",
        ));
    }
    let mut builder = cranelift_codegen::isa::lookup_by_name("x86_64-unknown-linux-gnu")
        .map_err(|_| Error("Cranelift has no x86-64 backend"))?;
    builder
        .enable("x86-64-v2")
        .map_err(|_| Error("Cranelift rejected the x86-64-v2 level"))?;
    Ok(builder)
}

/// Whether this processor runs code for `target`.
fn host_runs(target: u32) -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        target == TARGET_X86_64_V2
            && std::arch::is_x86_feature_detected!("sse4.2")
            && std::arch::is_x86_feature_detected!("popcnt")
            && std::arch::is_x86_feature_detected!("cmpxchg16b")
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        let _ = target;
        false
    }
}

/// Links the relocatable ELF object `object`, whose text defines the functions `names`, into one
/// block of code with its link table. Every symbol the code reads from the table must be `known`
/// to the loader, and every function in `names` must be defined.
pub fn link(object: &[u8], names: &[Box<str>], known: &dyn Fn(&str) -> bool) -> Result<Code, Error> {
    let file = object::File::parse(object).map_err(|_| Error("the object does not parse"))?;
    let text = file
        .section_by_name(".text")
        .ok_or(Error("the object has no code"))?;
    let text_index = text.index();
    let mut code = text
        .data()
        .map_err(|_| Error("the object's code does not read"))?
        .to_vec();
    if file.sections().any(|section| {
        section.index() != text_index
            && section.relocations().next().is_some()
            && section.name().is_ok_and(|name| !name.starts_with(".rela.eh") && name != ".eh_frame")
    }) {
        return Err(Error("the object relocates more than its code"));
    }
    let table_start = code.len().div_ceil(TABLE_ALIGNMENT) * TABLE_ALIGNMENT;
    let mut symbols: Vec<Box<str>> = Vec::new();
    let mut slots: FxHashMap<Box<str>, usize> = FxHashMap::default();
    let mut defined: FxHashMap<&str, u64> = FxHashMap::default();
    for symbol in file.symbols() {
        if symbol.section_index() == Some(text_index)
            && symbol.kind() == SymbolKind::Text
            && let Ok(name) = symbol.name()
        {
            defined.insert(name, symbol.address());
        }
    }
    for (offset, relocation) in text.relocations() {
        let RelocationTarget::Symbol(index) = relocation.target() else {
            return Err(Error("a relocation that names no symbol"));
        };
        let symbol = file
            .symbol_by_index(index)
            .map_err(|_| Error("a relocation names a missing symbol"))?;
        let place = usize::try_from(offset).map_err(|_| Error("a relocation past the code"))?;
        if place.checked_add(4).is_none_or(|end| end > code.len()) {
            return Err(Error("a relocation past the code"));
        }
        let size_ok = match relocation.flags() {
            RelocationFlags::Generic { size, .. } => size == 32,
            RelocationFlags::Elf { .. } => relocation.size() == 32,
            _ => false,
        };
        if !size_ok {
            return Err(Error("a relocation of a size the linker lacks"));
        }
        let target: i64 = match relocation.kind() {
            RelocationKind::GotRelative => {
                let name = symbol.name().map_err(|_| Error("a symbol without a name"))?;
                if !symbol.is_undefined() || !known(name) {
                    return Err(Error("the code reads a symbol the loader cannot resolve"));
                }
                let slot = match slots.get(name) {
                    Some(&slot) => slot,
                    None => {
                        symbols.push(name.into());
                        slots.insert(name.into(), symbols.len() - 1);
                        symbols.len() - 1
                    }
                };
                (table_start + slot * 8) as i64
            }
            RelocationKind::Relative | RelocationKind::PltRelative => {
                if symbol.is_undefined() {
                    return Err(Error("the code calls a function outside itself"));
                }
                if symbol.section_index() != Some(text_index) {
                    return Err(Error("the code refers to data outside itself"));
                }
                symbol.address() as i64
            }
            _ => return Err(Error("a relocation of a kind the linker lacks")),
        };
        let displacement = target + relocation.addend() - place as i64;
        let displacement =
            i32::try_from(displacement).map_err(|_| Error("a relocation out of range"))?;
        code[place..place + 4].copy_from_slice(&displacement.to_le_bytes());
    }
    let mut functions = Vec::with_capacity(names.len());
    for name in names {
        let offset = *defined
            .get(&**name)
            .ok_or(Error("a declared function was never defined"))?;
        let offset = u32::try_from(offset).map_err(|_| Error("the code is larger than 4GB"))?;
        functions.push((name.clone(), offset));
    }
    if code.len() > u32::MAX as usize {
        return Err(Error("the code is larger than 4GB"));
    }
    Ok(Code {
        text: code,
        symbols,
        functions,
        metadata: Vec::new(),
    })
}

/// The bytes of `code` as a section that will be written at offset `base` of its file, with the
/// code at a multiple of [`FILE_ALIGNMENT`] in the file.
pub fn encode(code: &Code, base: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&TARGET_X86_64_V2.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    let code_offset_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(code.text.len() as u32).to_le_bytes());
    let names = |out: &mut Vec<u8>, name: &str| {
        let bytes = &name.as_bytes()[..name.len().min(u16::MAX as usize)];
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
    };
    out.extend_from_slice(&(code.symbols.len() as u32).to_le_bytes());
    for symbol in &code.symbols {
        names(&mut out, symbol);
    }
    out.extend_from_slice(&(code.functions.len() as u32).to_le_bytes());
    for (name, offset) in &code.functions {
        names(&mut out, name);
        out.extend_from_slice(&offset.to_le_bytes());
    }
    out.extend_from_slice(&(code.metadata.len() as u32).to_le_bytes());
    out.extend_from_slice(&code.metadata);
    let absolute = base + out.len() as u64;
    let padded = absolute.div_ceil(FILE_ALIGNMENT) * FILE_ALIGNMENT;
    out.resize(out.len() + (padded - absolute) as usize, 0);
    let code_offset = out.len() as u32;
    out[code_offset_at..code_offset_at + 4].copy_from_slice(&code_offset.to_le_bytes());
    out.extend_from_slice(&code.text);
    let hash = fnv1a(&out[HASHED_FROM..]);
    out[HASHED_FROM - 8..HASHED_FROM].copy_from_slice(&hash.to_le_bytes());
    out
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// A section's header, read and checked against its bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct Header<'a> {
    pub target: u32,
    pub hash: u64,
    pub code_offset: usize,
    pub code: &'a [u8],
    pub symbols: Vec<&'a str>,
    pub functions: Vec<(&'a str, u32)>,
    pub metadata: &'a [u8],
}

/// Reads the header of `section`. Every offset and length is checked, so any bytes give a header
/// or an error, never a read out of bounds.
pub fn parse(section: &[u8]) -> Result<Header<'_>, Error> {
    let mut input = Input {
        bytes: section,
        position: 0,
    };
    if input.take(MAGIC.len())? != MAGIC {
        return Err(Error("the native section has no magic"));
    }
    let target = input.u32()?;
    let hash = input.u64()?;
    let code_offset = input.u32()? as usize;
    let code_length = input.u32()? as usize;
    let symbol_count = input.u32()? as usize;
    if symbol_count > MAX_NAMES || symbol_count > section.len() / 2 {
        return Err(Error("the native section names too many symbols"));
    }
    let mut symbols = Vec::with_capacity(symbol_count);
    for _ in 0..symbol_count {
        symbols.push(input.name()?);
    }
    let function_count = input.u32()? as usize;
    if function_count > MAX_NAMES || function_count > section.len() / 6 {
        return Err(Error("the native section names too many functions"));
    }
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let name = input.name()?;
        let offset = input.u32()?;
        if offset as usize >= code_length {
            return Err(Error("a function starts past the code"));
        }
        functions.push((name, offset));
    }
    let metadata_length = input.u32()? as usize;
    let metadata = input.take(metadata_length)?;
    if code_offset < input.position {
        return Err(Error("the code overlaps the header"));
    }
    let code = section
        .get(code_offset..code_offset.saturating_add(code_length))
        .filter(|code| code.len() == code_length)
        .ok_or(Error("the code lies outside the section"))?;
    Ok(Header {
        target,
        hash,
        code_offset,
        code,
        symbols,
        functions,
        metadata,
    })
}

struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Error("the native section ends early"))?;
        let taken = &self.bytes[self.position..end];
        self.position = end;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn name(&mut self) -> Result<&'a str, Error> {
        let length = self.take(2)?;
        let length = u16::from_le_bytes([length[0], length[1]]) as usize;
        std::str::from_utf8(self.take(length)?).map_err(|_| Error("a name that is not UTF-8"))
    }
}

/// Code a process mapped: each function's entry address by name. The mapping lives until the
/// process exits, like all compiled code, because nothing tracks which frames still run in it.
#[derive(Debug, Default)]
pub struct Loaded {
    pub functions: FxHashMap<Box<str>, usize>,
    pub metadata: Vec<u8>,
}

/// Maps the code of `section`, which starts at byte `offset` of the file `path`, and fills its
/// link table with `resolve`'s address for each symbol. The code is mapped from the file when its
/// offset in the file is a multiple of the page size, and copied otherwise. `Ok(None)` when this
/// processor cannot run the code.
///
/// # Safety
/// `resolve` must give, for each name, the address the code generator meant by it: running the
/// code jumps to and reads from those addresses.
pub unsafe fn load(
    path: &Path,
    offset: u64,
    section: &[u8],
    resolve: &dyn Fn(&str) -> Option<usize>,
) -> Result<Option<Loaded>, Error> {
    let header = parse(section)?;
    if !host_runs(header.target) {
        return Ok(None);
    }
    if section.get(HASHED_FROM..).map(fnv1a) != Some(header.hash) {
        return Err(Error("the native section does not match its hash"));
    }
    let mut table = Vec::with_capacity(header.symbols.len());
    for symbol in &header.symbols {
        table.push(resolve(symbol).ok_or(Error("the code reads a symbol this build lacks"))?);
    }
    let base = unsafe { map(path, offset + header.code_offset as u64, header.code, &table)? };
    let functions = header
        .functions
        .iter()
        .map(|&(name, entry)| (Box::from(name), base + entry as usize))
        .collect();
    Ok(Some(Loaded {
        functions,
        metadata: header.metadata.to_vec(),
    }))
}

/// Maps `code`, found at `file_offset` of `path`, executable and read-only, with the link table
/// `table` past it, and returns where the code starts.
#[cfg(unix)]
unsafe fn map(path: &Path, file_offset: u64, code: &[u8], table: &[usize]) -> Result<usize, Error> {
    use std::os::fd::AsRawFd;
    if code.is_empty() {
        return Err(Error("the native section holds no code"));
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = usize::try_from(page).map_err(|_| Error("the page size is unknown"))?;
    if page == 0 || TABLE_ALIGNMENT % page != 0 {
        return Err(Error("the page size does not divide the table's alignment"));
    }
    let table_start = code.len().div_ceil(TABLE_ALIGNMENT) * TABLE_ALIGNMENT;
    let table_bytes = (table.len() * 8).max(1).div_ceil(page) * page;
    let total = table_start + table_bytes;
    // SAFETY: a fresh anonymous reservation that nothing else uses.
    let start = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            total,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if start == libc::MAP_FAILED {
        return Err(Error("cannot reserve memory for the code"));
    }
    let release = |message: &'static str| {
        // SAFETY: the reservation made above, which nothing has handed out yet.
        unsafe { libc::munmap(start, total) };
        Error(message)
    };
    let code_bytes = code.len().div_ceil(page) * page;
    let mapped = file_offset % page as u64 == 0
        && std::fs::File::open(path).is_ok_and(|file| {
            // A shorter file would fault on the first read past its end.
            if file
                .metadata()
                .map_or(true, |metadata| metadata.len() < file_offset + code.len() as u64)
            {
                return false;
            }
            // SAFETY: replaces the start of the reservation with a read-only private mapping of
            // the file's code. Image files are replaced by rename, never rewritten in place.
            let mapped = unsafe {
                libc::mmap(
                    start,
                    code.len(),
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_FIXED,
                    file.as_raw_fd(),
                    file_offset as libc::off_t,
                )
            };
            mapped == start
        });
    // The file is opened again to map it, so it may have been replaced since `code` was read
    // and hashed: a mapping that differs from those bytes is overwritten with them below, where
    // the private mapping takes the writes as its own copy.
    // SAFETY: the mapping made `code.len()` readable bytes at `start`.
    let mapped_from_file = mapped
        && unsafe { std::slice::from_raw_parts(start.cast::<u8>(), code.len()) } == code;
    if !mapped_from_file {
        // SAFETY: the reservation's first `code_bytes` bytes, made writable, filled and then
        // made executable.
        unsafe {
            if libc::mprotect(start, code_bytes, libc::PROT_READ | libc::PROT_WRITE) != 0 {
                return Err(release("cannot make memory for the code writable"));
            }
            std::ptr::copy_nonoverlapping(code.as_ptr(), start.cast::<u8>(), code.len());
            if libc::mprotect(start, code_bytes, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                return Err(release("cannot make the code executable"));
            }
        }
    }
    // SAFETY: the table pages lie inside the reservation, past the code.
    unsafe {
        let table_address = start.cast::<u8>().add(table_start);
        let table_pointer = table_address.cast::<libc::c_void>();
        if libc::mprotect(table_pointer, table_bytes, libc::PROT_READ | libc::PROT_WRITE) != 0 {
            return Err(release("cannot make the link table writable"));
        }
        std::ptr::copy_nonoverlapping(table.as_ptr(), table_address.cast::<usize>(), table.len());
        if libc::mprotect(table_pointer, table_bytes, libc::PROT_READ) != 0 {
            return Err(release("cannot protect the link table"));
        }
    }
    Ok(start as usize)
}

#[cfg(not(unix))]
unsafe fn map(_: &Path, _: u64, _: &[u8], _: &[usize]) -> Result<usize, Error> {
    Err(Error("code written ahead of time is mapped only on Unix"))
}
