//! Regression tests for the `catch_unwind` crash guards around untrusted input.
//!
//! The guards only work when panics unwind. Cargo always builds test harnesses
//! with `panic = "unwind"`, so these tests prove the guards themselves; the
//! `compile_error!` in `explorie-native-services` is what stops a release
//! profile with `panic = "abort"` from building the app at all. CI also runs
//! these tests with the optimized `ci` profile (`cargo test --profile ci`).

use explorie_native_services::{ErrorCode, NativeServices, ResourcePaths, ServiceContext};
use std::fs;

#[test]
fn panic_guard_job_runtime_turns_worker_panics_into_errors() {
    let context = ServiceContext::default();
    let error = context
        .spawn_blocking(|| -> explorie_native_services::ServiceResult<()> {
            panic!("simulated decoder bug");
        })
        .wait()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);

    // The runtime stays usable after a worker panic.
    assert_eq!(context.spawn_blocking(|| Ok(7_u8)).wait().unwrap(), 7);
}

/// An 8-bit RGB Photoshop document whose raw image data holds more bytes per
/// channel than the header's pixel count. `psd` 0.3.5 accepts it but panics
/// with an out-of-bounds index when flattening, like a crafted file would.
fn psd_with_oversized_channels(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&3_u16.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&8_u16.to_be_bytes());
    bytes.extend_from_slice(&3_u16.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    let pixels = usize::try_from(width * height).unwrap();
    bytes.extend(std::iter::repeat_n(0x7f, 3 * pixels * 2));
    bytes
}

#[test]
fn panic_guard_psd_decoder_panic_is_a_recoverable_preview_error() {
    let temp = tempfile::tempdir().unwrap();
    let service = NativeServices::new(ResourcePaths::test(temp.path())).previews;
    let source = temp.path().join("crafted.psd");
    fs::write(&source, psd_with_oversized_channels(4, 4)).unwrap();

    // The bundled decoder panics on this file and the guard must turn that
    // into an error. On macOS a Quick Look rendering may then stand in for the
    // failed decode, so only an error, if any, is checked there.
    let preview = service.artifact(source.clone()).wait();
    #[cfg(not(target_os = "macos"))]
    assert_eq!(preview.unwrap_err().code, ErrorCode::InvalidInput);
    #[cfg(target_os = "macos")]
    if let Err(error) = &preview {
        assert_eq!(error.code, ErrorCode::InvalidInput, "{error:?}");
    }

    // Grid-view thumbnails decode the same file; they must return, not abort.
    let thumbnail = service.thumbnail(source, 128).wait();
    #[cfg(not(target_os = "macos"))]
    assert!(thumbnail.is_err());
    #[cfg(target_os = "macos")]
    drop(thumbnail);
}
