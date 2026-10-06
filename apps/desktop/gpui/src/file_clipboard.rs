//! The system file clipboard behind Copy, Cut and Paste.
//!
//! Windows talk to it through [`FileClipboard`] so tests can substitute an
//! in-memory clipboard and never replace what the user copied.
//!
//! On macOS, reading files another app copied asks the user unless the user
//! started the paste from Edit ▸ Paste or its shortcut, so windows only
//! [`FileClipboard::peek`] to keep their Paste affordances current and
//! [`FileClipboard::read`] when pasting.

use std::path::PathBuf;

use explorie_native_services::ServiceResult;
use explorie_native_services::clipboard::{self, ClipboardFiles, ClipboardSummary};

use crate::operation::{ClipboardKind, ClipboardState};

pub(crate) trait FileClipboard {
    /// Replace the clipboard contents with `paths`, marked as cut when `cut`.
    fn write(&self, paths: &[PathBuf], cut: bool) -> ServiceResult<()>;
    /// The files on the clipboard, or `None` when it holds something else.
    fn read(&self) -> ServiceResult<Option<ClipboardFiles>>;
    /// What the clipboard holds, without reading the files.
    fn peek(&self) -> ServiceResult<ClipboardSummary>;
    /// Empty the clipboard.
    fn clear(&self) -> ServiceResult<()>;
}

/// The platform clipboard: NSPasteboard on macOS, CF_HDROP on Windows.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct SystemFileClipboard;

impl FileClipboard for SystemFileClipboard {
    fn write(&self, paths: &[PathBuf], cut: bool) -> ServiceResult<()> {
        clipboard::write_files(paths, cut)
    }

    fn read(&self) -> ServiceResult<Option<ClipboardFiles>> {
        clipboard::read_files()
    }

    fn peek(&self) -> ServiceResult<ClipboardSummary> {
        clipboard::peek()
    }

    fn clear(&self) -> ServiceResult<()> {
        clipboard::clear()
    }
}

/// What the system clipboard held when it was last looked at.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum SystemClipboard {
    /// Not looked at yet, or unavailable: Paste falls back to the in-app
    /// state.
    #[default]
    Unknown,
    /// This many files to paste; their paths are read when pasting.
    Files(usize),
    /// Nothing pasteable (no files, or text).
    Empty,
}

impl SystemClipboard {
    pub(crate) fn from_summary(summary: &ClipboardSummary) -> Self {
        match summary.file_count {
            0 => Self::Empty,
            count => Self::Files(count),
        }
    }

    pub(crate) fn from_files(files: Option<&ClipboardFiles>) -> Self {
        match files.map_or(0, |files| files.paths.len()) {
            0 => Self::Empty,
            count => Self::Files(count),
        }
    }
}

/// The paste a read of the clipboard offers.
pub(crate) fn clipboard_state(files: ClipboardFiles) -> Option<ClipboardState> {
    (!files.paths.is_empty()).then_some(ClipboardState {
        kind: if files.cut {
            ClipboardKind::Cut
        } else {
            ClipboardKind::Copy
        },
        paths: files.paths,
    })
}

/// An in-memory clipboard for tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryFileClipboard {
    pub(crate) contents: std::sync::Mutex<Option<ClipboardFiles>>,
    change_count: std::sync::atomic::AtomicI64,
    /// Make reads and peeks fail, like a platform without a file clipboard.
    pub(crate) unreadable: std::sync::atomic::AtomicBool,
    /// Make reads fail as macOS does when it withholds files another app
    /// copied; peeks still see them.
    pub(crate) withheld: std::sync::atomic::AtomicBool,
    /// How many times the files were read (peeks do not count).
    pub(crate) reads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl MemoryFileClipboard {
    pub(crate) fn contents(&self) -> Option<ClipboardFiles> {
        self.contents.lock().unwrap().clone()
    }

    pub(crate) fn set(&self, contents: Option<ClipboardFiles>) {
        *self.contents.lock().unwrap() = contents;
        self.change_count
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    pub(crate) fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Acquire)
    }

    fn unavailable() -> explorie_native_services::ServiceError {
        explorie_native_services::ServiceError::new(
            explorie_native_services::ErrorCode::Unsupported,
            "The system file clipboard is unavailable",
        )
    }
}

#[cfg(test)]
impl FileClipboard for MemoryFileClipboard {
    fn write(&self, paths: &[PathBuf], cut: bool) -> ServiceResult<()> {
        self.set(Some(ClipboardFiles {
            paths: paths.to_vec(),
            cut,
        }));
        Ok(())
    }

    fn read(&self) -> ServiceResult<Option<ClipboardFiles>> {
        use std::sync::atomic::Ordering;
        if self.unreadable.load(Ordering::Acquire) {
            return Err(Self::unavailable());
        }
        self.reads.fetch_add(1, Ordering::AcqRel);
        if self.withheld.load(Ordering::Acquire) {
            return Err(explorie_native_services::ServiceError::new(
                explorie_native_services::ErrorCode::PermissionDenied,
                WITHHELD_MESSAGE,
            ));
        }
        Ok(self.contents())
    }

    fn peek(&self) -> ServiceResult<ClipboardSummary> {
        use std::sync::atomic::Ordering;
        if self.unreadable.load(Ordering::Acquire) {
            return Err(Self::unavailable());
        }
        let contents = self.contents();
        Ok(ClipboardSummary {
            change_count: self.change_count.load(Ordering::Acquire),
            file_count: contents.as_ref().map_or(0, |files| files.paths.len()),
            cut: contents.is_some_and(|files| files.cut),
        })
    }

    fn clear(&self) -> ServiceResult<()> {
        self.set(None);
        Ok(())
    }
}

/// The guidance [`MemoryFileClipboard`] reports for withheld files.
#[cfg(test)]
pub(crate) const WITHHELD_MESSAGE: &str = "macOS didn't let explorie read the files copied in another app. Paste with Edit ▸ Paste or its shortcut.";

/// The clipboard new windows use: the system one, or in tests a private
/// in-memory one.
pub(crate) fn default_file_clipboard() -> std::rc::Rc<dyn FileClipboard> {
    #[cfg(test)]
    {
        std::rc::Rc::new(MemoryFileClipboard::default())
    }
    #[cfg(not(test))]
    {
        std::rc::Rc::new(SystemFileClipboard)
    }
}
