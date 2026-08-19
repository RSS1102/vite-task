#[cfg(target_os = "linux")]
#[test]
fn embedded_payload_has_a_supported_layout() {
    let payload = include_bytes!(env!("CARGO_BIN_FILE_FSPY_PRELOAD_LINUX"));
    let blob = fspy_blob::Blob::from_elf(payload).unwrap();

    assert_ne!(blob.image_len(), 0);
    assert!(blob.entry() < blob.image_len());
}
