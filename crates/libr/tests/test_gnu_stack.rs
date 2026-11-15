//! The built `libR.so` carries a non-executable `PT_GNU_STACK` header. Without one, glibc 2.41
//! and later refuse to `dlopen` the library ("cannot enable executable stack").

#[cfg(target_os = "linux")]
mod harness;

#[cfg(target_os = "linux")]
#[test]
fn library_has_a_non_executable_stack_header() {
    const PT_GNU_STACK: u32 = 0x6474_e551;
    const PF_X: u32 = 1;
    let path = harness::library_path();
    let bytes = std::fs::read(&path).expect("libR.so is readable");
    let field = |offset: usize, length: usize| -> u64 {
        let slice = bytes
            .get(offset..offset + length)
            .expect("inside the ELF file");
        slice
            .iter()
            .rev()
            .fold(0, |value, byte| value << 8 | u64::from(*byte))
    };
    assert_eq!(&bytes[..4], b"\x7fELF");
    let table = field(0x20, 8) as usize;
    let size = field(0x36, 2) as usize;
    let count = field(0x38, 2) as usize;
    let flags = (0..count)
        .map(|index| table + index * size)
        .find(|entry| field(*entry, 4) == u64::from(PT_GNU_STACK))
        .map(|entry| field(entry + 4, 4) as u32)
        .unwrap_or_else(|| panic!("{} has no PT_GNU_STACK header", path.display()));
    assert_eq!(
        flags & PF_X,
        0,
        "{} has an executable stack",
        path.display()
    );
}
