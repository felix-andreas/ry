//! Rebinding a package binary that GNU R built on macOS to ry's libR.
//!
//! Mach-O binds every undefined symbol to one named library (the two-level namespace), and a
//! package that GNU R linked records GNU R's libR by its absolute install name, such as
//! `/Library/Frameworks/R.framework/Versions/4.5-arm64/Resources/lib/libR.dylib`. Loaded
//! unchanged, it would pull GNU R's libR into the process and call it instead of ry's runtime,
//! which it cannot share objects with. [`rebind_libr`] rewrites that load command to
//! `@rpath/libR.dylib`, the install name of ry's libR. dyld satisfies a load command from an
//! already loaded image with the same install name before it searches any path, so the package
//! binds to the runtime that is already running.
//!
//! The rewrite happens in place, inside the bytes the load command already occupies, so no
//! offset in the file moves. `@rpath/libR.dylib` is shorter than any absolute path that ends in
//! `/libR.dylib`, which makes it fit. The edit invalidates the binary's code signature, which
//! arm64 macOS enforces, so the caller re-signs the copy it loads (`dynload::rebound_copy`).
//!
//! A load command also records the compatibility version of the library it names, and GNU R
//! links libR with its own version (4.5.0, say). dyld refuses a library whose current version is
//! lower, and ry's libR has none, so the rewrite zeroes the recorded version, which accepts any.
//!
//! The rewritten copy lives in ry's cache, not beside the original, so a dependency the package
//! finds relative to itself (`@loader_path/libdep.dylib`, or an `@loader_path/../lib` rpath) must
//! be reachable from the copy's directory too. [`Rebound::climb`] reports how many directories
//! such a path climbs, which tells the caller how much of the original's surroundings to mirror.

/// The install name of ry's libR (`crates/libr/build.rs` sets it).
pub const RY_LIBR_INSTALL_NAME: &str = "@rpath/libR.dylib";

const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const MACH_HEADER_64_SIZE: usize = 32;

/// The load commands that name a library: `LC_LOAD_DYLIB`, `LC_LOAD_WEAK_DYLIB`,
/// `LC_REEXPORT_DYLIB`, `LC_LAZY_LOAD_DYLIB` and `LC_LOAD_UPWARD_DYLIB`.
const DYLIB_COMMANDS: [u32; 5] = [0xc, 0x8000_0018, 0x8000_001f, 0x20, 0x8000_0023];
/// The offset of `compatibility_version` in a `dylib_command`.
const COMPATIBILITY_VERSION_OFFSET: usize = 20;
const LC_RPATH: u32 = 0x8000_001c;
const LOADER_PATH: &str = "@loader_path/";

/// What [`rebind_libr`] did to an image and what loading it from elsewhere needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rebound {
    /// How many load commands now name ry's runtime. Zero means the binary does not link libR
    /// (ry built it with `-undefined dynamic_lookup`, or it is not a package library) and can
    /// load as it is.
    pub rewritten: usize,
    /// The most `..` components any `@loader_path` library reference or rpath starts with:
    /// `@loader_path/libdep.dylib` climbs 0 and `@loader_path/../lib` climbs 1.
    pub climb: usize,
}

/// Rewrites every load command of `image` that names GNU R's libR to ry's install name, in
/// each 64-bit architecture of a universal binary. A file that is not a 64-bit Mach-O image is
/// left alone for `dlopen` to accept or refuse; a malformed one is an error.
pub fn rebind_libr(image: &mut [u8]) -> Result<Rebound, String> {
    match read_u32_be(image, 0)? {
        FAT_MAGIC | FAT_MAGIC_64 => {
            let wide = read_u32_be(image, 0)? == FAT_MAGIC_64;
            let count = read_u32_be(image, 4)? as usize;
            let entry_size = if wide { 32 } else { 20 };
            let mut rebound = Rebound::default();
            for index in 0..count {
                let entry = 8 + index * entry_size;
                let (offset, size) = if wide {
                    (
                        read_u64_be(image, entry + 8)?,
                        read_u64_be(image, entry + 16)?,
                    )
                } else {
                    (
                        u64::from(read_u32_be(image, entry + 8)?),
                        u64::from(read_u32_be(image, entry + 12)?),
                    )
                };
                let start = usize::try_from(offset).map_err(|_| truncated())?;
                let end = start
                    .checked_add(usize::try_from(size).map_err(|_| truncated())?)
                    .ok_or_else(truncated)?;
                let slice = image.get_mut(start..end).ok_or_else(truncated)?;
                let slice_rebound = rebind_thin(slice)?;
                rebound.rewritten += slice_rebound.rewritten;
                rebound.climb = rebound.climb.max(slice_rebound.climb);
            }
            Ok(rebound)
        }
        _ => rebind_thin(image),
    }
}

/// A slice that is not 64-bit (an old i386 or ppc slice of a universal binary) is skipped,
/// because dyld only ever maps the host's slice.
fn rebind_thin(image: &mut [u8]) -> Result<Rebound, String> {
    let mut rebound = Rebound::default();
    if read_u32_le(image, 0)? != MH_MAGIC_64 {
        return Ok(rebound);
    }
    let command_count = read_u32_le(image, 16)? as usize;
    let mut offset = MACH_HEADER_64_SIZE;
    for _ in 0..command_count {
        let command = read_u32_le(image, offset)?;
        let size = read_u32_le(image, offset + 4)? as usize;
        if size < 8 {
            return Err("a load command has an invalid size".into());
        }
        let end = offset.checked_add(size).ok_or_else(truncated)?;
        let is_dylib = DYLIB_COMMANDS.contains(&command);
        if is_dylib || command == LC_RPATH {
            // Both commands hold their path as an `lc_str` offset right after `cmdsize`.
            let name_start = offset
                .checked_add(read_u32_le(image, offset + 8)? as usize)
                .ok_or_else(truncated)?;
            let field = image.get_mut(name_start..end).ok_or_else(truncated)?;
            let length = field
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(field.len());
            let name = std::str::from_utf8(&field[..length]).unwrap_or("");
            if let Some(relative) = name.strip_prefix(LOADER_PATH) {
                let climb = relative
                    .split('/')
                    .filter(|component| *component != "." && !component.is_empty())
                    .take_while(|component| *component == "..")
                    .count();
                rebound.climb = rebound.climb.max(climb);
            }
            if is_dylib && names_gnu_r_libr(name) {
                let replacement = RY_LIBR_INSTALL_NAME.as_bytes();
                // The name stays NUL-terminated inside its field.
                if replacement.len() >= field.len() {
                    return Err(format!(
                        "the library reference {name} is too short to rewrite to ry's runtime"
                    ));
                }
                field.fill(0);
                field[..replacement.len()].copy_from_slice(replacement);
                let version = offset + COMPATIBILITY_VERSION_OFFSET;
                if version + 4 > name_start {
                    return Err("a library reference overlaps its version fields".into());
                }
                image[version..version + 4].fill(0);
                rebound.rewritten += 1;
            }
        }
        offset = end;
    }
    Ok(rebound)
}

/// GNU R's libR, linked as `-lR` (`.../lib/libR.dylib`) or as `-framework R`, whose binary
/// CRAN's R.framework names `.../Resources/lib/libR.dylib` and older frameworks `.../R`. ry's
/// own install name is left alone, so rewriting is idempotent.
fn names_gnu_r_libr(name: &str) -> bool {
    if name == RY_LIBR_INSTALL_NAME {
        return false;
    }
    let file = name.rsplit('/').next().unwrap_or(name);
    file == "libR.dylib" || (file == "R" && name.contains("R.framework/"))
}

fn truncated() -> String {
    "the library file is truncated".into()
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let field = bytes.get(offset..offset + 4).ok_or_else(truncated)?;
    Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let field = bytes.get(offset..offset + 4).ok_or_else(truncated)?;
    Ok(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_u64_be(bytes: &[u8], offset: usize) -> Result<u64, String> {
    Ok((u64::from(read_u32_be(bytes, offset)?) << 32) | u64::from(read_u32_be(bytes, offset + 4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64-bit Mach-O image with one `LC_LOAD_DYLIB` per name, each name padded to an
    /// eight-byte boundary as the linker pads it.
    fn image(names: &[&str]) -> Vec<u8> {
        let mut commands = Vec::new();
        for name in names {
            let size = (24 + name.len() + 1).div_ceil(8) * 8;
            let mut command = Vec::with_capacity(size);
            for value in [0xc_u32, size as u32, 24, 2, 0x10000, 0x10000] {
                command.extend_from_slice(&value.to_le_bytes());
            }
            command.extend_from_slice(name.as_bytes());
            command.resize(size, 0);
            commands.extend(command);
        }
        let mut image = Vec::new();
        for value in [
            MH_MAGIC_64,
            0x0100_000c,
            0,
            6,
            names.len() as u32,
            commands.len() as u32,
            0,
            0,
        ] {
            image.extend_from_slice(&value.to_le_bytes());
        }
        image.extend(commands);
        image
    }

    fn rewritten(image: &mut [u8]) -> Result<usize, String> {
        rebind_libr(image).map(|rebound| rebound.rewritten)
    }

    /// The library names an image's load commands reference, in order.
    fn names(image: &[u8]) -> Vec<String> {
        let count = read_u32_le(image, 16).unwrap() as usize;
        let mut offset = MACH_HEADER_64_SIZE;
        let mut names = Vec::new();
        for _ in 0..count {
            let command = read_u32_le(image, offset).unwrap();
            let size = read_u32_le(image, offset + 4).unwrap() as usize;
            if DYLIB_COMMANDS.contains(&command) {
                let start = offset + read_u32_le(image, offset + 8).unwrap() as usize;
                let field = &image[start..offset + size];
                let length = field.iter().position(|&byte| byte == 0).unwrap();
                names.push(String::from_utf8(field[..length].to_vec()).unwrap());
            }
            offset += size;
        }
        names
    }

    /// A package library a real linker produced for arm64 macOS, linked with `-lR` against a
    /// libR whose install name is CRAN's, as GNU R links packages.
    #[test]
    fn rewrites_a_linker_built_package() {
        let mut bytes = include_bytes!("../tests/data/macho/gnu_r_package.so").to_vec();
        assert_eq!(
            names(&bytes),
            [
                "/Library/Frameworks/R.framework/Versions/4.5-arm64/Resources/lib/libR.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]
        );
        assert_eq!(rewritten(&mut bytes), Ok(1));
        assert_eq!(
            names(&bytes),
            [RY_LIBR_INSTALL_NAME, "/usr/lib/libSystem.B.dylib"]
        );
    }

    #[test]
    fn rewrites_the_cran_framework_and_homebrew_libr() {
        let mut bytes = image(&[
            "/Library/Frameworks/R.framework/Versions/4.5-arm64/Resources/lib/libR.dylib",
            "/opt/homebrew/opt/r/lib/R/lib/libR.dylib",
            "/Library/Frameworks/R.framework/Versions/4.5-arm64/Resources/lib/libRblas.dylib",
            "/usr/lib/libSystem.B.dylib",
        ]);
        let before = bytes.len();
        assert_eq!(rewritten(&mut bytes), Ok(2));
        assert_eq!(bytes.len(), before);
        assert_eq!(
            names(&bytes),
            [
                RY_LIBR_INSTALL_NAME,
                RY_LIBR_INSTALL_NAME,
                "/Library/Frameworks/R.framework/Versions/4.5-arm64/Resources/lib/libRblas.dylib",
                "/usr/lib/libSystem.B.dylib",
            ]
        );
        assert_eq!(rewritten(&mut bytes), Ok(0), "rewriting is idempotent");
    }

    #[test]
    fn an_old_framework_binary_counts_as_libr() {
        let mut bytes = image(&["/Library/Frameworks/R.framework/Versions/3.6/R"]);
        assert_eq!(rewritten(&mut bytes), Ok(1));
    }

    #[test]
    fn a_binary_without_libr_is_left_alone() {
        let mut bytes = image(&["/usr/lib/libSystem.B.dylib"]);
        let original = bytes.clone();
        assert_eq!(rewritten(&mut bytes), Ok(0));
        assert_eq!(bytes, original);
    }

    #[test]
    fn each_slice_of_a_universal_binary_is_rewritten() {
        let thin = image(&["/Library/Frameworks/R.framework/Resources/lib/libR.dylib"]);
        let mut fat = Vec::new();
        for value in [FAT_MAGIC, 2] {
            fat.extend_from_slice(&value.to_be_bytes());
        }
        let first = 4096_u32;
        let second = first + 4096;
        for offset in [first, second] {
            for value in [0x0100_000c_u32, 0, offset, thin.len() as u32, 12] {
                fat.extend_from_slice(&value.to_be_bytes());
            }
        }
        fat.resize(first as usize, 0);
        fat.extend(&thin);
        fat.resize(second as usize, 0);
        fat.extend(&thin);
        assert_eq!(rewritten(&mut fat), Ok(2));
    }

    /// Mutates valid images at random and checks that the rewriter never panics and never
    /// changes an image's length. `MACHO_FUZZ_SEED` replays a failure.
    #[test]
    fn fuzzed_images_never_panic() {
        let seed = std::env::var("MACHO_FUZZ_SEED")
            .ok()
            .and_then(|seed| seed.parse().ok())
            .unwrap_or(0x5eed_u64);
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let base = image(&[
            "/Library/Frameworks/R.framework/Resources/lib/libR.dylib",
            "/usr/lib/libSystem.B.dylib",
        ]);
        for iteration in 0..2000 {
            let mut bytes = base.clone();
            for _ in 0..1 + next() % 8 {
                let position = (next() as usize) % bytes.len();
                bytes[position] = next() as u8;
            }
            if next() % 4 == 0 {
                bytes.truncate((next() as usize) % bytes.len());
            }
            let length = bytes.len();
            drop(rebind_libr(&mut bytes));
            assert_eq!(
                bytes.len(),
                length,
                "iteration {iteration}; replay with MACHO_FUZZ_SEED={seed}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_mach_o_is_left_for_dlopen() {
        let mut elf = [0x7f, b'E', b'L', b'F', 0, 0, 0, 0];
        assert_eq!(rewritten(&mut elf), Ok(0));
    }

    #[test]
    fn the_recorded_compatibility_version_is_cleared() {
        let mut bytes = image(&["/opt/R/lib/libR.dylib", "/usr/lib/libSystem.B.dylib"]);
        let first = MACH_HEADER_64_SIZE + COMPATIBILITY_VERSION_OFFSET;
        let second = MACH_HEADER_64_SIZE
            + read_u32_le(&bytes, MACH_HEADER_64_SIZE + 4).unwrap() as usize
            + COMPATIBILITY_VERSION_OFFSET;
        assert_eq!(read_u32_le(&bytes, first), Ok(0x10000));
        assert_eq!(rewritten(&mut bytes), Ok(1));
        assert_eq!(read_u32_le(&bytes, first), Ok(0));
        assert_eq!(
            read_u32_le(&bytes, second),
            Ok(0x10000),
            "only libR's is cleared"
        );
    }

    #[test]
    fn loader_path_references_report_how_far_they_climb() {
        let mut bytes = image(&[
            "/opt/R/lib/libR.dylib",
            "@loader_path/libsibling.dylib",
            "@loader_path/./../../lib/libdep.dylib",
        ]);
        assert_eq!(
            rebind_libr(&mut bytes),
            Ok(Rebound {
                rewritten: 1,
                climb: 2
            })
        );
        let mut bytes = image(&["@loader_path/libsibling.dylib"]);
        assert_eq!(rebind_libr(&mut bytes).map(|rebound| rebound.climb), Ok(0));
    }

    #[test]
    fn a_loader_path_rpath_counts_toward_the_climb() {
        let mut bytes = image(&["@rpath/libdep.dylib"]);
        let path = "@loader_path/../lib";
        let size = (12 + path.len() + 1).div_ceil(8) * 8;
        let mut command = Vec::new();
        for value in [LC_RPATH, size as u32, 12] {
            command.extend_from_slice(&value.to_le_bytes());
        }
        command.extend_from_slice(path.as_bytes());
        command.resize(size, 0);
        bytes.extend(&command);
        let count = read_u32_le(&bytes, 16).unwrap() + 1;
        let commands_size = read_u32_le(&bytes, 20).unwrap() + size as u32;
        bytes[16..20].copy_from_slice(&count.to_le_bytes());
        bytes[20..24].copy_from_slice(&commands_size.to_le_bytes());
        assert_eq!(rebind_libr(&mut bytes).map(|rebound| rebound.climb), Ok(1));
    }

    #[test]
    fn malformed_images_are_errors_not_panics() {
        assert!(rebind_libr(&mut []).is_err());
        let mut cut = image(&["/opt/R/lib/libR.dylib"]);
        cut.truncate(40);
        assert!(rebind_libr(&mut cut).is_err());
        let mut lying = image(&["/opt/R/lib/libR.dylib"]);
        lying[16] = 9;
        assert!(rebind_libr(&mut lying).is_err());
    }
}
