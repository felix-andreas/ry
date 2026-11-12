use {
    interp::image::{MAGIC, is_current},
    std::path::Path,
};

/// The bytes `is_current` reads: the stamp, the header's length, then the header, which opens
/// with the writer's identity as a length and its bytes.
fn image_prefix(magic: &[u8], identity: &str) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend_from_slice(&(8 + identity.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(identity.len() as u64).to_le_bytes());
    bytes.extend_from_slice(identity.as_bytes());
    bytes
}

fn write(directory: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = directory.join(name);
    if let Err(error) = std::fs::write(&path, bytes) {
        panic!("cannot write {}: {error}", path.display());
    }
    path
}

#[test]
fn an_image_of_an_older_format_is_stale() {
    let directory = std::env::temp_dir().join(format!("ry-test-image-{}", std::process::id()));
    if let Err(error) = std::fs::create_dir_all(&directory) {
        panic!("cannot create {}: {error}", directory.display());
    }
    let identity = "runtime-build";
    let current = write(&directory, "current.image", &image_prefix(MAGIC, identity));
    assert!(is_current(&current, identity));
    assert!(!is_current(&current, "another-runtime-build"));
    // The format before the start image carried base R's compiled closures and was laid out
    // in touch order.
    let older = write(
        &directory,
        "older.image",
        &image_prefix(b"RYSTART\x05", identity),
    );
    assert!(!is_current(&older, identity));
    // The format before a symbol's lookup stamp share and its S3 method row shared one word.
    let before_method_rows = write(
        &directory,
        "before-method-rows.image",
        &image_prefix(b"RYSTART\x06", identity),
    );
    assert!(!is_current(&before_method_rows, identity));
    let foreign = write(
        &directory,
        "foreign.image",
        &image_prefix(b"RYHEAP\x00\x02", identity),
    );
    assert!(!is_current(&foreign, identity));
    if let Err(error) = std::fs::remove_dir_all(&directory) {
        panic!("cannot remove {}: {error}", directory.display());
    }
}
