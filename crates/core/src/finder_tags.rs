//! Finder tags.
//!
//! Finder stores an item's tags in the `com.apple.metadata:_kMDItemUserTags`
//! extended attribute as a binary plist array of `"Name\n<colorIndex>"`
//! strings (the colour suffix is optional). Reading the attribute directly,
//! instead of asking Spotlight through `mdls`, keeps colours, works on
//! unindexed volumes, and cannot lag behind a write; writes merge into what
//! is on disk so colours and entries this code does not understand survive.
//!
//! This module is the only place that encodes or decodes that attribute:
//! listings use [`listed_finder_tags`] and the Finder-tag editor goes through
//! [`read_finder_tags`] and [`write_finder_tags`].

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

/// Finder's tag colour indices: 0 is "no colour", then Gray, Green, Purple,
/// Blue, Yellow, Red and Orange.
pub const FINDER_TAG_COLOR_COUNT: u8 = 8;

/// Listings keep at most this many tags per item.
pub const MAX_LISTED_FINDER_TAGS: usize = 32;

/// One Finder tag: its name and colour index (see [`FINDER_TAG_COLOR_COUNT`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FinderTag {
    pub name: String,
    #[serde(default)]
    pub color: u8,
}

impl FinderTag {
    /// Parse a stored tag string: `"Name"` or `"Name\n<colorIndex>"`.
    /// Unknown colour indices mean "no colour".
    pub fn from_raw(raw: &str) -> Self {
        match raw.split_once('\n') {
            Some((name, color)) => Self {
                name: name.to_string(),
                color: color
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|color| *color < FINDER_TAG_COLOR_COUNT)
                    .unwrap_or(0),
            },
            None => Self {
                name: raw.to_string(),
                color: 0,
            },
        }
    }

    /// The stored form of this tag.
    pub fn to_raw(&self) -> String {
        if self.color == 0 {
            self.name.clone()
        } else {
            format!("{}\n{}", self.name, self.color)
        }
    }
}

/// The raw tag strings on `path` (empty when it has none, and on platforms
/// without Finder tags). A tag attribute that is not a readable tag list is
/// an `InvalidData` error.
pub fn read_finder_tags(path: &Path) -> io::Result<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        xattr::get(path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Ok(Vec::new())
    }
}

/// Replace the tags on `path` with `tags` (raw strings). A plain name keeps
/// the colour it already has on disk, stored entries that are not tag
/// strings are kept, and an unreadable attribute is never overwritten.
pub fn write_finder_tags(path: &Path, tags: &[String]) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        xattr::set(path, tags)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, tags);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Finder tags are only available on macOS.",
        ))
    }
}

/// Tags for a directory listing: one bounded read, and anything unreadable
/// simply means "no tags". Only worth calling for items with extended
/// attributes.
pub(crate) fn listed_finder_tags(path: &Path) -> Vec<FinderTag> {
    #[cfg(target_os = "macos")]
    {
        xattr::listed(path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
mod xattr {
    use super::{FinderTag, MAX_LISTED_FINDER_TAGS};
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const USER_TAGS: &CStr = c"com.apple.metadata:_kMDItemUserTags";
    /// Enough for dozens of tags; listings read larger attributes only up
    /// to [`MAX_LISTED_BYTES`].
    const LISTING_BUFFER: usize = 4096;
    const MAX_LISTED_BYTES: usize = 64 * 1024;

    pub(super) fn get(path: &Path) -> io::Result<Vec<String>> {
        let Some(bytes) = read(path, usize::MAX)? else {
            return Ok(Vec::new());
        };
        Ok(tag_strings(parse(&bytes)?))
    }

    pub(super) fn listed(path: &Path) -> Vec<FinderTag> {
        let Ok(c_path) = c_path(path) else {
            return Vec::new();
        };
        let mut buffer = [0_u8; LISTING_BUFFER];
        // SAFETY: Both strings are NUL-terminated and the buffer is valid for
        // `buffer.len()` writable bytes.
        let size = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                USER_TAGS.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        let bytes = if size >= 0 {
            buffer[..size as usize].to_vec()
        } else if io::Error::last_os_error().raw_os_error() == Some(libc::ERANGE) {
            match read(path, MAX_LISTED_BYTES) {
                Ok(Some(bytes)) => bytes,
                _ => return Vec::new(),
            }
        } else {
            return Vec::new();
        };
        parse(&bytes)
            .map(|values| {
                tag_strings(values)
                    .iter()
                    .take(MAX_LISTED_FINDER_TAGS)
                    .map(|raw| FinderTag::from_raw(raw))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn set(path: &Path, tags: &[String]) -> io::Result<()> {
        let existing = match read(path, usize::MAX)? {
            // Refuse to replace data that cannot be parsed rather than wipe it.
            Some(bytes) => parse(&bytes)?,
            None => Vec::new(),
        };
        let merged = merge(existing, tags);
        if merged.is_empty() {
            return remove(path);
        }
        let mut encoded = Vec::new();
        plist::Value::Array(merged)
            .to_writer_binary(&mut encoded)
            .map_err(io::Error::other)?;
        write(path, &encoded)
    }

    fn tag_strings(values: Vec<plist::Value>) -> Vec<String> {
        values
            .into_iter()
            .filter_map(plist::Value::into_string)
            .filter(|tag| !tag.is_empty())
            .collect()
    }

    /// Apply the requested tag list on top of the stored entries: a plain
    /// name keeps the colour it already has on disk, and entries that are not
    /// tag strings are carried over untouched.
    pub(super) fn merge(existing: Vec<plist::Value>, requested: &[String]) -> Vec<plist::Value> {
        let stored_tags: Vec<&str> = existing
            .iter()
            .filter_map(plist::Value::as_string)
            .collect();
        let mut merged: Vec<plist::Value> = requested
            .iter()
            .map(|tag| {
                let tag = if tag.contains('\n') {
                    tag.as_str()
                } else {
                    stored_tags
                        .iter()
                        .copied()
                        .find(|stored| {
                            stored
                                .split_once('\n')
                                .is_some_and(|(name, _)| name == tag.as_str())
                        })
                        .unwrap_or(tag.as_str())
                };
                plist::Value::String(tag.to_string())
            })
            .collect();
        merged.extend(
            existing
                .into_iter()
                .filter(|value| value.as_string().is_none()),
        );
        merged
    }

    fn parse(bytes: &[u8]) -> io::Result<Vec<plist::Value>> {
        match plist::Value::from_reader(io::Cursor::new(bytes)) {
            Ok(plist::Value::Array(values)) => Ok(values),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Finder tags on this item are not a tag list",
            )),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Finder tags on this item are unreadable: {error}"),
            )),
        }
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid path"))
    }

    /// The attribute's bytes, `None` when absent. Attributes larger than
    /// `limit` bytes are an `InvalidData` error.
    fn read(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
        let c_path = c_path(path)?;
        loop {
            // SAFETY: Both strings are NUL-terminated; a null buffer asks for
            // the attribute size without writing anything.
            let size = unsafe {
                libc::getxattr(
                    c_path.as_ptr(),
                    USER_TAGS.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            };
            if size < 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ENOATTR) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            if size as usize > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Finder tags on this item are too large",
                ));
            }
            let mut buffer = vec![0_u8; size as usize];
            // SAFETY: The buffer is valid for `buffer.len()` writable bytes.
            let read = unsafe {
                libc::getxattr(
                    c_path.as_ptr(),
                    USER_TAGS.as_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                    libc::XATTR_NOFOLLOW,
                )
            };
            if read >= 0 {
                buffer.truncate(read as usize);
                return Ok(Some(buffer));
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                // The attribute grew between the two calls; ask again.
                Some(libc::ERANGE) => continue,
                Some(libc::ENOATTR) => return Ok(None),
                _ => return Err(error),
            }
        }
    }

    fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let c_path = c_path(path)?;
        // SAFETY: Both strings are NUL-terminated and the value pointer and
        // length describe `bytes`.
        let result = unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                USER_TAGS.as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn remove(path: &Path) -> io::Result<()> {
        let c_path = c_path(path)?;
        // SAFETY: Both strings are NUL-terminated.
        let result =
            unsafe { libc::removexattr(c_path.as_ptr(), USER_TAGS.as_ptr(), libc::XATTR_NOFOLLOW) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn write_raw_tags(path: &Path, values: Vec<plist::Value>) {
            let mut encoded = Vec::new();
            plist::Value::Array(values)
                .to_writer_binary(&mut encoded)
                .unwrap();
            write(path, &encoded).unwrap();
        }

        #[test]
        fn merge_prefers_explicit_colors() {
            let merged = merge(
                vec![
                    plist::Value::String("Work\n7".into()),
                    plist::Value::String("Home\n2".into()),
                ],
                &["Work\n1".to_string(), "Home".to_string(), "New".to_string()],
            );
            assert_eq!(
                merged,
                vec![
                    plist::Value::String("Work\n1".into()),
                    plist::Value::String("Home\n2".into()),
                    plist::Value::String("New".into()),
                ]
            );
        }

        #[test]
        fn listing_reads_names_and_colors_and_ignores_unreadable_tags() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("tagged.txt");
            std::fs::write(&path, "content").unwrap();
            assert!(listed(&path).is_empty());

            write_raw_tags(
                &path,
                vec![
                    plist::Value::String("Important\n6".into()),
                    plist::Value::String("Plain".into()),
                    plist::Value::String("Odd\n42".into()),
                    plist::Value::Integer(7.into()),
                ],
            );
            assert_eq!(
                listed(&path),
                vec![
                    FinderTag {
                        name: "Important".into(),
                        color: 6
                    },
                    FinderTag {
                        name: "Plain".into(),
                        color: 0
                    },
                    FinderTag {
                        name: "Odd".into(),
                        color: 0
                    },
                ]
            );

            write(&path, b"not a plist").unwrap();
            assert!(listed(&path).is_empty());
        }

        #[test]
        fn listing_reads_tags_larger_than_its_first_buffer() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("tagged.txt");
            std::fs::write(&path, "content").unwrap();
            let long_name = "x".repeat(LISTING_BUFFER);
            write_raw_tags(
                &path,
                vec![
                    plist::Value::String(format!("{long_name}\n4")),
                    plist::Value::String("Second\n2".into()),
                ],
            );
            let tags = listed(&path);
            assert_eq!(tags.len(), 2);
            assert_eq!(tags[0].name, long_name);
            assert_eq!(tags[0].color, 4);
            assert_eq!(tags[1].name, "Second");
        }

        #[test]
        fn listing_keeps_a_bounded_number_of_tags() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("tagged.txt");
            std::fs::write(&path, "content").unwrap();
            write_raw_tags(
                &path,
                (0..MAX_LISTED_FINDER_TAGS + 10)
                    .map(|index| plist::Value::String(format!("Tag {index}")))
                    .collect(),
            );
            assert_eq!(listed(&path).len(), MAX_LISTED_FINDER_TAGS);
            assert_eq!(get(&path).unwrap().len(), MAX_LISTED_FINDER_TAGS + 10);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_tags_round_trip_names_and_colors() {
        assert_eq!(
            FinderTag::from_raw("Important\n6"),
            FinderTag {
                name: "Important".into(),
                color: 6
            }
        );
        assert_eq!(FinderTag::from_raw("Plain").color, 0);
        assert_eq!(FinderTag::from_raw("Odd\n9").color, 0);
        assert_eq!(FinderTag::from_raw("Odd\nblue").name, "Odd");
        assert_eq!(FinderTag::from_raw("Important\n6").to_raw(), "Important\n6");
        assert_eq!(FinderTag::from_raw("Plain").to_raw(), "Plain");
    }

    #[test]
    fn serialized_tags_default_their_color() {
        let tag: FinderTag = serde_json::from_str(r#"{"name":"Work"}"#).unwrap();
        assert_eq!(tag.color, 0);
    }
}
