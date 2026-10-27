//! The system file clipboard.
//!
//! Copy, cut and paste of files go through the platform clipboard so they
//! work between explorie windows and with Finder or Explorer: file URLs on
//! the macOS general pasteboard (plus a private marker for explorie's cut),
//! and `CF_HDROP` with a "Preferred DropEffect" on Windows.
//!
//! macOS 15.4 and later ask the user before an app reads what another app
//! copied, unless the read is part of a paste the user started from Edit ▸
//! Paste or its key equivalent. [`peek`] never asks, so it is what keeps
//! Paste affordances current; [`read_files`] is for the paste itself.

use crate::{ErrorCode, ServiceError, ServiceResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Files found on the system clipboard.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClipboardFiles {
    pub paths: Vec<PathBuf>,
    /// True when the files were cut (a paste should move them). Finder never
    /// marks a copy as cut; Explorer's cut sets `DROPEFFECT_MOVE`.
    pub cut: bool,
}

/// What the clipboard holds, learned without reading the files themselves.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClipboardSummary {
    /// Changes whenever any app replaces the clipboard contents.
    pub change_count: i64,
    /// How many files the clipboard holds.
    pub file_count: usize,
    /// True when the files are explorie's (or Explorer's) cut.
    pub cut: bool,
}

/// How macOS treats this app's reads of what other apps copied
/// (`NSPasteboard.accessBehavior`, macOS 15.4 and later).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteAccess {
    /// Never asked yet: the first read outside a user paste asks.
    Default,
    /// Reads outside a user paste ask each time.
    Ask,
    AlwaysAllow,
    /// Reads outside a user paste are refused without asking.
    AlwaysDeny,
}

impl PasteAccess {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn from_raw(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Default),
            1 => Some(Self::Ask),
            2 => Some(Self::AlwaysAllow),
            3 => Some(Self::AlwaysDeny),
            _ => None,
        }
    }
}

/// The error for a clipboard that lists files explorie could not read.
/// macOS withholds them when the user declined (or denied in System
/// Settings) a read that was not part of a paste they started from Edit ▸
/// Paste or its shortcut; that paste is always allowed, so the message points
/// there first.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn unreadable_files_error(access: Option<PasteAccess>) -> ServiceError {
    const SETTINGS: &str = "System Settings ▸ Privacy & Security ▸ Paste from Other Apps";
    match access {
        Some(PasteAccess::AlwaysDeny) => ServiceError::new(
            ErrorCode::PermissionDenied,
            format!(
                "macOS blocked explorie from reading the files copied in another app. Paste with Edit ▸ Paste or its shortcut, or allow explorie in {SETTINGS}."
            ),
        ),
        Some(PasteAccess::Default | PasteAccess::Ask) => ServiceError::new(
            ErrorCode::PermissionDenied,
            format!(
                "macOS didn't let explorie read the files copied in another app. Paste with Edit ▸ Paste or its shortcut, choose Allow Paste when macOS asks, or allow explorie in {SETTINGS}."
            ),
        ),
        Some(PasteAccess::AlwaysAllow) | None => ServiceError::new(
            ErrorCode::Io,
            "The clipboard lists files, but their locations could not be read",
        ),
    }
    .operation("clipboard_read")
}

/// Replace the clipboard contents with `paths`, marked as cut when `cut`.
pub fn write_files(paths: &[PathBuf], cut: bool) -> ServiceResult<()> {
    if paths.is_empty() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "No files to place on the clipboard",
        ));
    }
    platform::write_files(None, paths, cut)
}

/// The files on the clipboard, or `None` when it holds no files. Fails with
/// [`ErrorCode::PermissionDenied`] and guidance for the user when macOS
/// withholds files another app copied.
pub fn read_files() -> ServiceResult<Option<ClipboardFiles>> {
    platform::read_files(None)
}

/// Describe the clipboard without reading the files, which on macOS never
/// asks the user for permission.
pub fn peek() -> ServiceResult<ClipboardSummary> {
    platform::peek(None)
}

/// Empty the clipboard, as after the files of a cut have been moved.
pub fn clear() -> ServiceResult<()> {
    platform::clear(None)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ClipboardFiles, ClipboardSummary, PasteAccess, unreadable_files_error};
    use crate::{ErrorCode, ServiceError, ServiceResult};
    use std::ffi::{CStr, CString, OsStr};
    use std::os::raw::c_char;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    unsafe extern "C" {
        fn explorie_clipboard_write_files(
            pasteboard_name: *const c_char,
            paths: *const *const c_char,
            count: usize,
            cut: i32,
        ) -> *mut c_char;
        fn explorie_clipboard_read_files(
            pasteboard_name: *const c_char,
            out_paths: *mut *mut c_char,
            out_len: *mut usize,
            out_cut: *mut i32,
            out_access: *mut i32,
        ) -> i32;
        fn explorie_clipboard_peek(
            pasteboard_name: *const c_char,
            out_change_count: *mut i64,
            out_file_count: *mut usize,
            out_cut: *mut i32,
        ) -> i32;
        fn explorie_clipboard_clear(pasteboard_name: *const c_char) -> *mut c_char;
        fn explorie_clipboard_free(value: *mut c_char);
    }

    /// Turn a bridge error string (null on success) into a result.
    fn bridge_result(error: *mut c_char, operation: &'static str) -> ServiceResult<()> {
        if error.is_null() {
            return Ok(());
        }
        // SAFETY: A non-null bridge result is a valid NUL-terminated allocation.
        let message = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: The pointer came from the bridge and has not been released yet.
        unsafe { explorie_clipboard_free(error) };
        Err(ServiceError::new(ErrorCode::Io, message).operation(operation))
    }

    fn pasteboard_name(pasteboard: Option<&str>) -> ServiceResult<Option<CString>> {
        pasteboard
            .map(|name| {
                CString::new(name)
                    .map_err(|_| ServiceError::new(ErrorCode::InvalidInput, "Invalid pasteboard"))
            })
            .transpose()
    }

    /// Write to the general pasteboard, or to the named private pasteboard
    /// (tests use one so they never touch the user's clipboard).
    pub(super) fn write_files(
        pasteboard: Option<&str>,
        paths: &[PathBuf],
        cut: bool,
    ) -> ServiceResult<()> {
        let pasteboard = pasteboard_name(pasteboard)?;
        let paths = paths
            .iter()
            .map(|path| {
                CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                    ServiceError::new(
                        ErrorCode::InvalidInput,
                        format!("Invalid file path: {}", path.display()),
                    )
                })
            })
            .collect::<ServiceResult<Vec<_>>>()?;
        let pointers: Vec<*const c_char> = paths.iter().map(|path| path.as_ptr()).collect();
        // SAFETY: Every pointer is a NUL-terminated string that outlives the
        // synchronous call, and `count` matches the pointer array.
        let error = unsafe {
            explorie_clipboard_write_files(
                pasteboard
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                pointers.as_ptr(),
                pointers.len(),
                i32::from(cut),
            )
        };
        bridge_result(error, "clipboard_write")
    }

    pub(super) fn clear(pasteboard: Option<&str>) -> ServiceResult<()> {
        let pasteboard = pasteboard_name(pasteboard)?;
        // SAFETY: The name is null or a NUL-terminated string that outlives
        // the synchronous call.
        let error = unsafe {
            explorie_clipboard_clear(
                pasteboard
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
            )
        };
        bridge_result(error, "clipboard_clear")
    }

    pub(super) fn read_files(pasteboard: Option<&str>) -> ServiceResult<Option<ClipboardFiles>> {
        let pasteboard = pasteboard_name(pasteboard)?;
        let mut buffer: *mut c_char = std::ptr::null_mut();
        let mut length = 0_usize;
        let mut cut = 0_i32;
        let mut access = -1_i32;
        // SAFETY: The out-pointers reference live locals; the bridge fills
        // them before returning.
        let found = unsafe {
            explorie_clipboard_read_files(
                pasteboard
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                &mut buffer,
                &mut length,
                &mut cut,
                &mut access,
            )
        };
        if found == 2 {
            return Err(unreadable_files_error(PasteAccess::from_raw(access)));
        }
        if found == 0 || buffer.is_null() {
            return Ok(None);
        }
        // SAFETY: The bridge returned `length` initialized bytes at `buffer`.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), length) };
        let paths = bytes
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsStr::from_bytes(path)))
            .collect();
        // SAFETY: The pointer came from the bridge and has not been released yet.
        unsafe { explorie_clipboard_free(buffer) };
        Ok(Some(ClipboardFiles {
            paths,
            cut: cut != 0,
        }))
    }

    pub(super) fn peek(pasteboard: Option<&str>) -> ServiceResult<ClipboardSummary> {
        let pasteboard = pasteboard_name(pasteboard)?;
        let mut summary = ClipboardSummary::default();
        let mut cut = 0_i32;
        // SAFETY: The out-pointers reference live locals; the bridge fills
        // them before returning.
        let valid = unsafe {
            explorie_clipboard_peek(
                pasteboard
                    .as_ref()
                    .map_or(std::ptr::null(), |name| name.as_ptr()),
                &mut summary.change_count,
                &mut summary.file_count,
                &mut cut,
            )
        };
        if valid == 0 {
            return Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "Invalid pasteboard",
            ));
        }
        summary.cut = cut != 0;
        Ok(summary)
    }

    /// Put one item with raw `data` for `type_name` on a test pasteboard.
    #[cfg(test)]
    pub(super) fn write_type(pasteboard: &str, type_name: &str, data: &[u8]) {
        unsafe extern "C" {
            fn explorie_clipboard_write_type_for_tests(
                pasteboard_name: *const c_char,
                type_name: *const c_char,
                data: *const u8,
                length: usize,
            ) -> i32;
        }
        let name = CString::new(pasteboard).unwrap();
        let type_name = CString::new(type_name).unwrap();
        // SAFETY: The strings are NUL-terminated and `data` is valid for
        // `data.len()` bytes during the synchronous call.
        let written = unsafe {
            explorie_clipboard_write_type_for_tests(
                name.as_ptr(),
                type_name.as_ptr(),
                data.as_ptr(),
                data.len(),
            )
        };
        assert_eq!(written, 1);
    }

    #[cfg(test)]
    pub(super) fn release(pasteboard: &str) {
        unsafe extern "C" {
            fn explorie_clipboard_release(pasteboard_name: *const c_char);
        }
        let name = CString::new(pasteboard).unwrap();
        // SAFETY: The name is a NUL-terminated string used synchronously.
        unsafe { explorie_clipboard_release(name.as_ptr()) };
    }
}

#[cfg(windows)]
mod platform {
    use super::{ClipboardFiles, ClipboardSummary};
    use crate::{ErrorCode, ServiceError, ServiceResult};
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, POINT};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::{
        CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    };
    use windows_sys::Win32::UI::Shell::{DROPFILES, DragQueryFileW};

    /// Holds the clipboard open for this thread and closes it on drop.
    struct OpenedClipboard;

    impl OpenedClipboard {
        fn open() -> ServiceResult<Self> {
            // Another application may hold the clipboard briefly; retry.
            for attempt in 0..10 {
                // SAFETY: A null owner associates the clipboard with this task.
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    return Ok(Self);
                }
                std::thread::sleep(Duration::from_millis(10 * (attempt + 1)));
            }
            Err(last_error("The clipboard is in use by another application"))
        }
    }

    impl Drop for OpenedClipboard {
        fn drop(&mut self) {
            // SAFETY: This value exists only while the clipboard is open.
            unsafe { CloseClipboard() };
        }
    }

    fn last_error(context: &str) -> ServiceError {
        ServiceError::new(
            ErrorCode::Io,
            format!("{context}: {}", std::io::Error::last_os_error()),
        )
    }

    fn preferred_drop_effect_format() -> u32 {
        let name: Vec<u16> = "Preferred DropEffect\0".encode_utf16().collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    }

    /// Copy `bytes` into a movable global allocation owned by the caller.
    fn global_copy(bytes: &[u8]) -> ServiceResult<HGLOBAL> {
        // SAFETY: Allocation and locking follow the documented GlobalAlloc
        // protocol; the copy stays within the allocated length.
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                return Err(last_error("Unable to allocate clipboard memory"));
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                GlobalFree(handle);
                return Err(last_error("Unable to lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
            GlobalUnlock(handle);
            Ok(handle)
        }
    }

    /// Hand `handle` to the clipboard, which owns it on success.
    fn set_clipboard_data(format: u32, handle: HGLOBAL) -> ServiceResult<()> {
        // SAFETY: The clipboard is open and `handle` is a movable global
        // allocation; on failure ownership stays here and it is freed.
        unsafe {
            // HGLOBAL and HANDLE are the same pointer type in windows-sys.
            if SetClipboardData(format, handle).is_null() {
                let error = last_error("Unable to write to the clipboard");
                GlobalFree(handle);
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn write_files(
        _pasteboard: Option<&str>,
        paths: &[PathBuf],
        cut: bool,
    ) -> ServiceResult<()> {
        // DROPFILES header followed by NUL-separated wide paths and a final NUL.
        let header = DROPFILES {
            pFiles: std::mem::size_of::<DROPFILES>() as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: 0,
            fWide: 1,
        };
        let mut bytes = Vec::new();
        // SAFETY: DROPFILES is a plain packed C struct; viewing it as bytes
        // is how the shell expects it to be serialized.
        bytes.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (&header as *const DROPFILES).cast::<u8>(),
                std::mem::size_of::<DROPFILES>(),
            )
        });
        for path in paths {
            for unit in path.as_os_str().encode_wide().chain(std::iter::once(0)) {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        let effect = if cut {
            DROPEFFECT_MOVE
        } else {
            DROPEFFECT_COPY | DROPEFFECT_LINK
        };

        let _clipboard = OpenedClipboard::open()?;
        // SAFETY: The clipboard is open on this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(last_error("Unable to clear the clipboard"));
        }
        set_clipboard_data(u32::from(CF_HDROP), global_copy(&bytes)?)?;
        set_clipboard_data(
            preferred_drop_effect_format(),
            global_copy(&effect.to_le_bytes())?,
        )
    }

    pub(super) fn clear(_pasteboard: Option<&str>) -> ServiceResult<()> {
        let _clipboard = OpenedClipboard::open()?;
        // SAFETY: The clipboard is open on this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(last_error("Unable to clear the clipboard"));
        }
        Ok(())
    }

    pub(super) fn read_files(_pasteboard: Option<&str>) -> ServiceResult<Option<ClipboardFiles>> {
        // SAFETY: Format queries do not require the clipboard to be open.
        if unsafe { IsClipboardFormatAvailable(u32::from(CF_HDROP)) } == 0 {
            return Ok(None);
        }
        let _clipboard = OpenedClipboard::open()?;
        // SAFETY: The clipboard is open; the returned handle stays owned by it.
        // The handle is an HDROP (the same pointer type in windows-sys).
        let hdrop = unsafe { GetClipboardData(u32::from(CF_HDROP)) };
        if hdrop.is_null() {
            return Ok(None);
        }
        // SAFETY: `hdrop` is a valid HDROP while the clipboard stays open; each
        // buffer is sized from DragQueryFileW's reported length plus the NUL.
        let paths: Vec<PathBuf> = unsafe {
            let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            (0..count)
                .filter_map(|index| {
                    let length = DragQueryFileW(hdrop, index, std::ptr::null_mut(), 0);
                    let mut buffer = vec![0_u16; length as usize + 1];
                    let copied =
                        DragQueryFileW(hdrop, index, buffer.as_mut_ptr(), buffer.len() as u32);
                    (copied > 0)
                        .then(|| PathBuf::from(OsString::from_wide(&buffer[..copied as usize])))
                })
                .collect()
        };
        if paths.is_empty() {
            return Ok(None);
        }

        let format = preferred_drop_effect_format();
        // SAFETY: The clipboard is open; the effect is a DWORD in a global
        // allocation owned by the clipboard, read while it is locked.
        let cut = unsafe {
            let handle: HGLOBAL = GetClipboardData(format);
            if handle.is_null() {
                false
            } else {
                let effect = GlobalLock(handle).cast::<u32>();
                if effect.is_null() {
                    false
                } else {
                    let value = effect.read_unaligned();
                    GlobalUnlock(handle);
                    value & DROPEFFECT_MOVE != 0
                }
            }
        };
        Ok(Some(ClipboardFiles { paths, cut }))
    }

    /// Windows has no clipboard consent, so this reads the files as before.
    pub(super) fn peek(pasteboard: Option<&str>) -> ServiceResult<ClipboardSummary> {
        // SAFETY: Takes no arguments and only reports the sequence number.
        let change_count = i64::from(unsafe { GetClipboardSequenceNumber() });
        let files = read_files(pasteboard)?;
        Ok(ClipboardSummary {
            change_count,
            file_count: files.as_ref().map_or(0, |files| files.paths.len()),
            cut: files.is_some_and(|files| files.cut),
        })
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{ClipboardFiles, ClipboardSummary};
    use crate::{ErrorCode, ServiceError, ServiceResult};
    use std::path::PathBuf;

    fn unsupported() -> ServiceError {
        ServiceError::new(
            ErrorCode::Unsupported,
            "The system file clipboard is available only on macOS and Windows",
        )
    }

    pub(super) fn write_files(
        _pasteboard: Option<&str>,
        _paths: &[PathBuf],
        _cut: bool,
    ) -> ServiceResult<()> {
        Err(unsupported())
    }

    pub(super) fn read_files(_pasteboard: Option<&str>) -> ServiceResult<Option<ClipboardFiles>> {
        Err(unsupported())
    }

    pub(super) fn clear(_pasteboard: Option<&str>) -> ServiceResult<()> {
        Err(unsupported())
    }

    pub(super) fn peek(_pasteboard: Option<&str>) -> ServiceResult<ClipboardSummary> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_nothing_is_rejected() {
        assert_eq!(
            write_files(&[], false).unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }

    #[test]
    fn unreadable_files_explain_how_to_paste_them() {
        for access in [
            PasteAccess::Default,
            PasteAccess::Ask,
            PasteAccess::AlwaysDeny,
        ] {
            let error = unreadable_files_error(Some(access));
            assert_eq!(error.code, ErrorCode::PermissionDenied);
            assert!(error.message.contains("Edit ▸ Paste"), "{access:?}");
            assert!(
                error
                    .message
                    .contains("System Settings ▸ Privacy & Security ▸ Paste from Other Apps"),
                "{access:?}"
            );
        }
        assert!(
            unreadable_files_error(Some(PasteAccess::Ask))
                .message
                .contains("Allow Paste")
        );
        assert!(
            unreadable_files_error(Some(PasteAccess::AlwaysDeny))
                .message
                .starts_with("macOS blocked explorie")
        );
        // Without pasteboard privacy to blame, it is an ordinary read failure.
        for access in [Some(PasteAccess::AlwaysAllow), None] {
            let error = unreadable_files_error(access);
            assert_eq!(error.code, ErrorCode::Io);
            assert!(!error.message.contains("System Settings"));
        }
    }

    #[test]
    fn paste_access_matches_nspasteboard_access_behavior() {
        assert_eq!(PasteAccess::from_raw(0), Some(PasteAccess::Default));
        assert_eq!(PasteAccess::from_raw(1), Some(PasteAccess::Ask));
        assert_eq!(PasteAccess::from_raw(2), Some(PasteAccess::AlwaysAllow));
        assert_eq!(PasteAccess::from_raw(3), Some(PasteAccess::AlwaysDeny));
        assert_eq!(PasteAccess::from_raw(-1), None);
    }

    /// A private pasteboard that is discarded afterwards, so tests never
    /// replace what the user copied.
    #[cfg(target_os = "macos")]
    struct PrivatePasteboard(String);

    #[cfg(target_os = "macos")]
    impl PrivatePasteboard {
        fn new() -> Self {
            Self(format!(
                "com.omershatz.explorie.test.{}",
                uuid::Uuid::new_v4()
            ))
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for PrivatePasteboard {
        fn drop(&mut self) {
            platform::release(&self.0);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_files_round_trip_through_a_pasteboard_with_the_cut_marker() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("Quarterly Reports");
        let file = root.path().join("résumé, final.txt");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&file, "cv").unwrap();
        let folder = folder.canonicalize().unwrap();
        let file = file.canonicalize().unwrap();
        let pasteboard = PrivatePasteboard::new();
        assert_eq!(platform::read_files(Some(&pasteboard.0)).unwrap(), None);

        platform::write_files(Some(&pasteboard.0), &[folder.clone(), file.clone()], true).unwrap();
        assert_eq!(
            platform::read_files(Some(&pasteboard.0)).unwrap(),
            Some(ClipboardFiles {
                paths: vec![folder.clone(), file.clone()],
                cut: true,
            })
        );

        // A later copy replaces the cut marker along with the contents.
        platform::write_files(Some(&pasteboard.0), std::slice::from_ref(&file), false).unwrap();
        assert_eq!(
            platform::read_files(Some(&pasteboard.0)).unwrap(),
            Some(ClipboardFiles {
                paths: vec![file],
                cut: false,
            })
        );

        // Clearing leaves nothing to paste.
        platform::clear(Some(&pasteboard.0)).unwrap();
        assert_eq!(platform::read_files(Some(&pasteboard.0)).unwrap(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_peek_describes_the_pasteboard_without_reading_it() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a.txt");
        let second = root.path().join("b.txt");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&second, "b").unwrap();
        let pasteboard = PrivatePasteboard::new();
        let empty = platform::peek(Some(&pasteboard.0)).unwrap();
        assert_eq!((empty.file_count, empty.cut), (0, false));

        platform::write_files(Some(&pasteboard.0), &[first.clone(), second], true).unwrap();
        let cut = platform::peek(Some(&pasteboard.0)).unwrap();
        assert_eq!((cut.file_count, cut.cut), (2, true));
        assert_ne!(cut.change_count, empty.change_count);
        assert_eq!(
            platform::peek(Some(&pasteboard.0)).unwrap(),
            cut,
            "looking changes nothing"
        );

        platform::write_files(Some(&pasteboard.0), &[first], false).unwrap();
        let copy = platform::peek(Some(&pasteboard.0)).unwrap();
        assert_eq!((copy.file_count, copy.cut), (1, false));
        assert_ne!(copy.change_count, cut.change_count);

        // Text is not something to paste into a folder.
        platform::write_type(&pasteboard.0, "public.utf8-plain-text", b"hi");
        let text = platform::peek(Some(&pasteboard.0)).unwrap();
        assert_eq!((text.file_count, text.cut), (0, false));
        assert_eq!(platform::read_files(Some(&pasteboard.0)).unwrap(), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_listed_files_that_cannot_be_read_are_an_error_not_an_empty_clipboard() {
        let pasteboard = PrivatePasteboard::new();
        // What a withheld read looks like: the file URL type is listed but
        // no URL comes back. Private pasteboards always allow reads, so this
        // is reported as a read failure rather than a privacy block.
        platform::write_type(&pasteboard.0, "public.file-url", b"");
        assert_eq!(platform::peek(Some(&pasteboard.0)).unwrap().file_count, 1);
        let error = platform::read_files(Some(&pasteboard.0)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert_eq!(error.operation.as_deref(), Some("clipboard_read"));
    }
}
