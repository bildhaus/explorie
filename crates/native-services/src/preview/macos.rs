//! ImageIO downsampled decodes, Quick Look thumbnails and NSWorkspace icons
//! through `apps/desktop/native-assets/macos/PreviewImageBridge.m`.
//!
//! Every call blocks the calling (background) thread until the PNG is written
//! or its timeout passes; the system work itself runs on its own queue.

use std::ffi::{CString, c_char};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

unsafe extern "C" {
    fn explorie_imageio_thumbnail(
        path: *const c_char,
        max_pixels: u32,
        max_source_dimension: u32,
        max_source_pixels: u64,
        timeout_seconds: f64,
        output: *const c_char,
        generation: *const u64,
        ticket: u64,
    ) -> i32;
    fn explorie_quicklook_thumbnail(
        path: *const c_char,
        max_pixels: u32,
        timeout_seconds: f64,
        output: *const c_char,
        generation: *const u64,
        ticket: u64,
    ) -> i32;
    fn explorie_workspace_file_icon(
        path: *const c_char,
        pixels: u32,
        timeout_seconds: f64,
        output: *const c_char,
    ) -> i32;
    fn explorie_workspace_type_icon(
        extension: *const c_char,
        container: i32,
        pixels: u32,
        timeout_seconds: f64,
        output: *const c_char,
    ) -> i32;
    fn explorie_icon_appearance_is_dark() -> i32;
}

/// Why the bridge produced no image. The discriminants match the bridge's
/// result codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeImageError {
    /// The system has no thumbnail or icon for this item.
    Unavailable = 1,
    TimedOut = 2,
    /// A newer preview superseded the request.
    Cancelled = 3,
    WriteFailed = 4,
    InvalidInput = 5,
    /// The image's header declares more pixels than the caller allows.
    TooLarge = 6,
}

fn result_from_code(code: i32) -> Result<(), NativeImageError> {
    match code {
        0 => Ok(()),
        2 => Err(NativeImageError::TimedOut),
        3 => Err(NativeImageError::Cancelled),
        4 => Err(NativeImageError::WriteFailed),
        5 => Err(NativeImageError::InvalidInput),
        6 => Err(NativeImageError::TooLarge),
        _ => Err(NativeImageError::Unavailable),
    }
}

/// What kind of item a shared "kind" icon stands for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IconContainer {
    File = 0,
    Folder = 1,
    Package = 2,
}

fn c_path(path: &Path) -> Result<CString, NativeImageError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| NativeImageError::InvalidInput)
}

/// Pixel limits checked against an image's header before ImageIO decodes it.
#[derive(Clone, Copy, Debug)]
pub(super) struct SourceLimits {
    pub max_dimension: u32,
    pub max_pixels: u64,
}

/// Decode the primary image of `path` with ImageIO, fitted within
/// `max_pixels` square, upright and never enlarged, and write it to `output`
/// as an sRGB PNG. JPEG and HEIC decode directly at the reduced size, so a
/// large photo is never held at full resolution. With `cancellation`, the wait
/// ends early once the generation counter moves past the ticket.
pub(super) fn imageio_thumbnail(
    path: &Path,
    max_pixels: u32,
    limits: SourceLimits,
    timeout: Duration,
    output: &Path,
    cancellation: Option<(&AtomicU64, u64)>,
) -> Result<(), NativeImageError> {
    let path = c_path(path)?;
    let output = c_path(output)?;
    let (generation, ticket) = cancellation_pointer(cancellation);
    // SAFETY: Both strings are NUL-terminated and outlive the call. The bridge
    // only reads `generation` with atomic loads, on the calling thread and only
    // until it returns, while the borrowed AtomicU64 is alive.
    let code = unsafe {
        explorie_imageio_thumbnail(
            path.as_ptr(),
            max_pixels,
            limits.max_dimension,
            limits.max_pixels,
            timeout.as_secs_f64(),
            output.as_ptr(),
            generation,
            ticket,
        )
    };
    result_from_code(code)
}

fn cancellation_pointer(cancellation: Option<(&AtomicU64, u64)>) -> (*const u64, u64) {
    cancellation.map_or((std::ptr::null(), 0), |(generation, ticket)| {
        (generation.as_ptr().cast_const(), ticket)
    })
}

/// Write Quick Look's thumbnail of `path`, fitted within `max_pixels` square
/// and never enlarged, to `output` as PNG. With `cancellation`, the wait ends
/// early once the generation counter moves past the ticket.
pub(super) fn quicklook_thumbnail(
    path: &Path,
    max_pixels: u32,
    timeout: Duration,
    output: &Path,
    cancellation: Option<(&AtomicU64, u64)>,
) -> Result<(), NativeImageError> {
    let path = c_path(path)?;
    let output = c_path(output)?;
    let (generation, ticket) = cancellation_pointer(cancellation);
    // SAFETY: Both strings are NUL-terminated and outlive the call. The bridge
    // only reads `generation` with atomic loads, and only until it returns,
    // while the borrowed AtomicU64 is alive.
    let code = unsafe {
        explorie_quicklook_thumbnail(
            path.as_ptr(),
            max_pixels,
            timeout.as_secs_f64(),
            output.as_ptr(),
            generation,
            ticket,
        )
    };
    result_from_code(code)
}

/// Write the icon Finder shows for the item at `path` as a `pixels` square PNG.
pub(super) fn file_icon(
    path: &Path,
    pixels: u32,
    timeout: Duration,
    output: &Path,
) -> Result<(), NativeImageError> {
    let path = c_path(path)?;
    let output = c_path(output)?;
    // SAFETY: Both strings are NUL-terminated and outlive the call.
    let code = unsafe {
        explorie_workspace_file_icon(
            path.as_ptr(),
            pixels,
            timeout.as_secs_f64(),
            output.as_ptr(),
        )
    };
    result_from_code(code)
}

/// Write the icon every item of a kind shares — a file or package with this
/// filename extension, or a plain folder — as a `pixels` square PNG.
pub(super) fn type_icon(
    extension: &str,
    container: IconContainer,
    pixels: u32,
    timeout: Duration,
    output: &Path,
) -> Result<(), NativeImageError> {
    let extension = CString::new(extension).map_err(|_| NativeImageError::InvalidInput)?;
    let output = c_path(output)?;
    // SAFETY: Both strings are NUL-terminated and outlive the call.
    let code = unsafe {
        explorie_workspace_type_icon(
            extension.as_ptr(),
            container as i32,
            pixels,
            timeout.as_secs_f64(),
            output.as_ptr(),
        )
    };
    result_from_code(code)
}

/// Whether icons currently render in a dark appearance. System icons follow
/// the appearance, so cached renderings are keyed by it.
pub(super) fn icons_draw_dark() -> bool {
    // SAFETY: The bridge function takes no arguments and only reads state.
    unsafe { explorie_icon_appearance_is_dark() != 0 }
}
