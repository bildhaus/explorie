//! Full 7-Zip fallback. The child only writes pipes; Rust owns all extracted paths.

use super::*;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn supports(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "xz" | "bz2"
                    | "gz"
                    | "lzma"
                    | "z"
                    | "zst"
                    | "zstd"
                    | "cab"
                    | "iso"
                    | "udf"
                    | "wim"
                    | "swm"
                    | "esd"
                    | "msi"
                    | "chm"
                    | "arj"
                    | "cpio"
                    | "ar"
                    | "deb"
                    | "rpm"
                    | "dmg"
                    | "xar"
                    | "hfs"
                    | "hfsx"
                    | "apfs"
                    | "squashfs"
                    | "img"
                    | "vhd"
                    | "vhdx"
                    | "vdi"
                    | "vmdk"
                    | "qcow"
                    | "qcow2"
                    | "ntfs"
                    | "fat"
                    | "ext"
                    | "ext2"
                    | "ext3"
                    | "ext4"
            )
        })
}

fn single_stream(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "xz" | "bz2" | "gz" | "lzma" | "z" | "zst" | "zstd"
            )
        })
}

fn executable() -> io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let parent = current
        .parent()
        .ok_or_else(|| io::Error::other("Missing executable directory"))?;
    let name = if cfg!(windows) { "7z.exe" } else { "7zz" };
    let installed = if cfg!(target_os = "macos") {
        parent.join("../Resources/7zip").join(name)
    } else {
        parent.join("7zip").join(name)
    };
    if installed.is_file() {
        return Ok(installed);
    }
    let standalone = parent.join("7zip").join(name);
    if standalone.is_file() {
        return Ok(standalone);
    }
    // Never load a decoder from PATH, the current directory, or a build machine's
    // checkout in an installed release.
    #[cfg(any(debug_assertions, test))]
    {
        let target = format!(
            "{}-{}",
            std::env::consts::ARCH,
            if cfg!(windows) {
                "pc-windows-msvc"
            } else if cfg!(target_os = "macos") {
                "apple-darwin"
            } else {
                "unknown-linux-gnu"
            }
        );
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/native-assets/binaries")
            .join(format!("7zip-{target}"))
            .join(name);
        if development.is_file() {
            return Ok(development);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Bundled 7-Zip is missing. Reinstall Explorie, or run pnpm prepare:native for a development build.",
    ))
}

fn command(program: &Path, verb: &str) -> io::Result<Command> {
    let mut command = Command::new(program);
    command.arg(verb).args(["-sccUTF-8", "-bd", "-bsp0"]);
    // No decoder-controlled writes, no console window, no inherited input handles.
    // DLL lookup and working-directory defaults must not depend on an archive's
    // untrusted directory. All input paths passed below are absolute.
    command.current_dir(
        program
            .parent()
            .ok_or_else(|| io::Error::other("Missing decoder directory"))?,
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    Ok(command)
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "archive extraction cancelled",
        ))
    } else {
        Ok(())
    }
}

// Read both pipes concurrently with bounded buffering. Cancellation and byte-limit
// failures kill and reap the decoder even when it is not producing output.
fn run(
    command: &mut Command,
    password: Option<&str>,
    cancelled: &AtomicBool,
    timeout: Option<Duration>,
    mut consume: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    check_cancelled(cancelled)?;
    let password = password.unwrap_or("");
    if password.len() > 1024 || password.contains(['\r', '\n', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "7-Zip passwords must be at most 1024 bytes and cannot contain line breaks or NUL",
        ));
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(2);
        for (diagnostic, pipe) in [
            (
                false,
                Box::new(child.stdout.take().expect("piped stdout")) as Box<dyn Read + Send>,
            ),
            (
                true,
                Box::new(child.stderr.take().expect("piped stderr")) as Box<dyn Read + Send>,
            ),
        ] {
            let sender = sender.clone();
            scope.spawn(move || {
                let mut pipe = pipe;
                loop {
                    let mut bytes = vec![0; 64 * 1024];
                    let result = pipe.read(&mut bytes).map(|count| {
                        bytes.truncate(count);
                        bytes
                    });
                    let finished = result.as_ref().map_or(true, Vec::is_empty);
                    if sender.send((diagnostic, result)).is_err() || finished {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut input = child.stdin.take().expect("piped stdin");
        let result = (|| {
            // 7-Zip reads a password only if necessary. A short line fits the pipe;
            // closing it prevents interactive prompts from hanging background jobs.
            if let Err(error) = input.write_all(format!("{password}\n").as_bytes())
                && error.kind() != io::ErrorKind::BrokenPipe
            {
                return Err(error);
            }
            drop(input);
            let mut diagnostics = Vec::new();
            let mut finished = 0;
            while finished < 2 {
                check_cancelled(cancelled)?;
                if timeout.is_some_and(|limit| started.elapsed() > limit) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "7-Zip listing timed out",
                    ));
                }
                match receiver.recv_timeout(POLL_INTERVAL) {
                    Ok((diagnostic, result)) => {
                        let bytes = result?;
                        if bytes.is_empty() {
                            finished += 1;
                        } else if diagnostic {
                            if diagnostics.len() + bytes.len() > MAX_DIAGNOSTIC_BYTES {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "7-Zip diagnostic output exceeded its safety limit",
                                ));
                            }
                            diagnostics.extend_from_slice(&bytes);
                        } else {
                            consume(&bytes)?;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(io::Error::other("7-Zip output reader stopped"));
                    }
                }
            }
            loop {
                check_cancelled(cancelled)?;
                if timeout.is_some_and(|limit| started.elapsed() > limit) {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "7-Zip listing timed out",
                    ));
                }
                if let Some(status) = child.try_wait()? {
                    if !status.success() {
                        let diagnostic = String::from_utf8_lossy(&diagnostics).to_ascii_lowercase();
                        let message = if diagnostic.contains("password")
                            || diagnostic.contains("encrypted")
                        {
                            "7-Zip could not decrypt the archive. Check its password."
                        } else {
                            "7-Zip could not read the archive; it may be corrupt or unsupported."
                        };
                        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
                    }
                    break;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Ok(diagnostics)
        })();
        // Drop the receiver before joining, including on a consumer error, so a
        // reader cannot remain blocked on a full channel after the decoder dies.
        drop(receiver);
        if result.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        result
    })
}

struct ListedEntry {
    entry: ArchiveEntry,
    expected_size: Option<u64>,
    encrypted: bool,
}

fn invalid_listing() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "7-Zip returned an unsafe or ambiguous archive listing",
    )
}

fn parse_listing(text: &str, archive: &Path, max_entries: usize) -> io::Result<Vec<ListedEntry>> {
    let text = text.replace("\r\n", "\n");
    // Encrypted headers cause the console frontend to write its fixed prompt
    // before the technical records, even when the password arrives via stdin.
    let text = text.trim_start_matches('\n');
    let text = text
        .strip_prefix("Enter password (will not be echoed):\n")
        .or_else(|| text.strip_prefix("Enter password:\n"))
        .unwrap_or(text);
    let mut entries = Vec::new();
    let mut paths = HashSet::new();
    for record in text
        .split("\n\n")
        .filter(|record| !record.trim().is_empty())
    {
        if entries.len() >= max_entries {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Archive listing exceeds the safety limit of entries",
            ));
        }
        let mut fields = HashMap::new();
        for line in record.lines() {
            let (key, value) = line.split_once(" = ").ok_or_else(invalid_listing)?;
            if fields.insert(key, value).is_some() {
                return Err(invalid_listing());
            }
        }
        for key in [
            "Symbolic Link",
            "Hard Link",
            "Copy Link",
            "Link",
            "Reparse",
            "Reparse Data",
        ] {
            if fields.get(key).is_some_and(|value| !value.is_empty()) {
                return Err(invalid_listing());
            }
        }
        for key in ["Alternate Stream", "Anti"] {
            if fields
                .get(key)
                .is_some_and(|value| *value != "-" && !value.is_empty())
            {
                return Err(invalid_listing());
            }
        }
        let attributes = fields.get("Attributes").copied().unwrap_or("");
        let mode = fields.get("Mode").copied().unwrap_or("");
        if attributes.contains('L')
            || attributes
                .split_whitespace()
                .any(|part| part.starts_with(['l', 'b', 'c', 'p', 's']))
            || mode.starts_with(['l', 'b', 'c', 'p', 's'])
        {
            return Err(invalid_listing());
        }
        let path = if let Some(path) = fields.get("Path") {
            path.to_string()
        } else if single_stream(archive) {
            archive
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(invalid_listing)?
                .to_string()
        } else {
            return Err(invalid_listing());
        };
        if path.chars().any(char::is_control) || path.contains(':') || path.starts_with(['/', '\\'])
        {
            return Err(invalid_listing());
        }
        let normalized = path.replace('\\', "/");
        validate_archive_entry_path(Path::new(&normalized))?;
        let normalized = Path::new(&normalized)
            .components()
            .filter_map(|component| {
                if let Component::Normal(value) = component {
                    Some(value.to_string_lossy())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        // Reject case aliases on all platforms: Windows and common macOS volumes
        // are insensitive, and the technical listing normalizes some characters.
        if !paths.insert(normalized.to_lowercase()) {
            return Err(invalid_listing());
        }
        let is_dir = fields.get("Folder").is_some_and(|value| *value == "+")
            || attributes.starts_with('D')
            || mode.starts_with('d');
        let expected_size = fields
            .get("Size")
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<u64>().map_err(|_| invalid_listing()))
            .transpose()?;
        if expected_size.is_none() && !is_dir && !single_stream(archive) {
            return Err(invalid_listing());
        }
        let compressed_size = fields
            .get("Packed Size")
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<u64>().map_err(|_| invalid_listing()))
            .transpose()?
            .unwrap_or(0);
        entries.push(ListedEntry {
            entry: ArchiveEntry {
                name: normalized
                    .rsplit('/')
                    .next()
                    .ok_or_else(invalid_listing)?
                    .to_string(),
                path,
                size: expected_size.unwrap_or(0),
                compressed_size,
                is_dir,
            },
            expected_size,
            encrypted: fields.get("Encrypted").is_some_and(|value| *value == "+"),
        });
    }
    Ok(entries)
}

/// Untrusted-listing entry point for `crate::fuzzing`.
pub(crate) fn parse_listing_entries(
    text: &str,
    archive: &Path,
    max_entries: usize,
) -> io::Result<Vec<ArchiveEntry>> {
    let entries = parse_listing(text, archive, max_entries)?;
    Ok(entries.into_iter().map(|listed| listed.entry).collect())
}

fn listing(
    program: &Path,
    archive: &Path,
    password: Option<&str>,
    cancelled: &AtomicBool,
    max_entries: usize,
) -> io::Result<Vec<ListedEntry>> {
    let mut command = command(program, "l")?;
    command.args(["-slt", "-ba", "--"]).arg(archive);
    let mut output = Vec::new();
    run(
        &mut command,
        password,
        cancelled,
        Some(Duration::from_secs(120)),
        |bytes| {
            if output.len() + bytes.len() > MAX_LIST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Archive listing exceeds the safety limit of 32 MiB",
                ));
            }
            output.extend_from_slice(bytes);
            Ok(())
        },
    )?;
    parse_listing(
        std::str::from_utf8(&output).map_err(|_| invalid_listing())?,
        archive,
        max_entries,
    )
}

pub(super) fn list(path: &Path) -> io::Result<ArchiveInfo> {
    let archive = fs::canonicalize(path)?;
    let entries = listing(
        &executable()?,
        &archive,
        None,
        &AtomicBool::new(false),
        ExtractionLimits::default().max_entries,
    )?;
    let entries: Vec<_> = entries.into_iter().map(|entry| entry.entry).collect();
    let total_size = entries.iter().try_fold(0_u64, |total, entry| {
        total.checked_add(entry.size).ok_or_else(invalid_listing)
    })?;
    Ok(ArchiveInfo {
        format: path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("archive")
            .to_ascii_lowercase(),
        compressed_size: fs::metadata(path)?.len(),
        total_size,
        entry_count: entries.len(),
        entries,
    })
}

pub(super) fn needs_password(path: &Path) -> io::Result<bool> {
    let archive = fs::canonicalize(path)?;
    Ok(listing(
        &executable()?,
        &archive,
        None,
        &AtomicBool::new(false),
        ExtractionLimits::default().max_entries,
    )?
    .iter()
    .any(|entry| entry.encrypted))
}

pub(super) fn extract_into(
    path: &Path,
    output: &Path,
    password: Option<&str>,
    budget: &mut ExtractionBudget<'_>,
) -> io::Result<u64> {
    let archive = fs::canonicalize(path)?;
    let program = executable()?;
    let entries = listing(
        &program,
        &archive,
        password,
        budget.cancelled,
        budget.limits.max_entries,
    )?;
    for listed in entries {
        budget.begin_entry(listed.entry.size)?;
        let entry_path = listed.entry.path.replace('\\', "/");
        let destination = ensure_safe_extraction_path(output, Path::new(&entry_path))?;
        if listed.entry.is_dir {
            fs::create_dir_all(destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
            ensure_no_link_ancestors(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut command = command(&program, "x")?;
        command
            .args(["-so", "-spd", "-ssc", "-r-", "-spm", "-bso2", "-bse2", "--"])
            .arg(&archive)
            .arg(&listed.entry.path);
        let cancelled = budget.cancelled;
        let mut written = 0_u64;
        let diagnostics = run(&mut command, password, cancelled, None, |bytes| {
            budget.add_bytes(bytes.len() as u64)?;
            written = written
                .checked_add(bytes.len() as u64)
                .ok_or_else(invalid_listing)?;
            if listed.expected_size.is_some_and(|size| written > size) {
                return Err(invalid_listing());
            }
            file.write_all(bytes)
        })?;
        if listed.expected_size.is_some_and(|size| written != size)
            || String::from_utf8_lossy(&diagnostics)
                .lines()
                .any(|line| line.trim() == "No files to process")
        {
            return Err(invalid_listing());
        }
        file.flush()?;
    }
    Ok(budget.total_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(windows, target_os = "macos"))]
    use tempfile::TempDir;

    #[test]
    fn listing_rejects_unsafe_paths_aliases_and_special_entries() {
        for path in [
            "../escape",
            "/absolute",
            "C:\\escape",
            "safe/../../escape",
            "file:stream",
        ] {
            assert!(
                parse_listing(
                    &format!("Path = {path}\nSize = 0\n"),
                    Path::new("data.cab"),
                    100
                )
                .is_err(),
                "{path}"
            );
        }
        for property in [
            "Symbolic Link = target",
            "Hard Link = target",
            "Copy Link = target",
            "Alternate Stream = +",
            "Anti = +",
            "Mode = lrwxrwxrwx",
            "Attributes = A lrwxrwxrwx",
        ] {
            assert!(
                parse_listing(
                    &format!("Path = file\nSize = 0\n{property}\n"),
                    Path::new("data.cab"),
                    100
                )
                .is_err(),
                "{property}"
            );
        }
        for paths in [("file", "FILE"), ("a/b", "a\\b"), ("./file", "file")] {
            assert!(
                parse_listing(
                    &format!(
                        "Path = {}\nSize = 0\n\nPath = {}\nSize = 0\n",
                        paths.0, paths.1
                    ),
                    Path::new("data.cab"),
                    100
                )
                .is_err()
            );
        }
        assert!(
            parse_listing(
                "Path = file\nPath = other\nSize = 0\n",
                Path::new("data.cab"),
                100
            )
            .is_err()
        );
        assert!(parse_listing("Path = file\nSize = 0\n", Path::new("data.cab"), 0).is_err());
    }

    #[test]
    fn listing_accepts_single_streams_without_names_or_declared_sizes() {
        let entries =
            parse_listing("Size = \nPacked Size = \n", Path::new("notes.bz2"), 100).unwrap();
        assert_eq!(entries[0].entry.path, "notes");
        assert_eq!(entries[0].expected_size, None);
        assert!(parse_listing("Size = \nPacked Size = \n", Path::new("notes.cab"), 100).is_err());
    }

    #[test]
    fn listing_accepts_only_exact_leading_password_prompts() {
        let record = "Path = secret.txt\nSize = 15\nEncrypted = +\n";
        for prompt in ["Enter password (will not be echoed):", "Enter password:"] {
            for newline in ["\n", "\r\n"] {
                let listing = format!("\n{prompt}\n{record}").replace('\n', newline);
                let entries = parse_listing(&listing, Path::new("encrypted.cab"), 100).unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].entry.path, "secret.txt");
                assert!(entries[0].encrypted);
            }
            // Prompts inside technical records must not be silently discarded.
            assert!(
                parse_listing(
                    &format!("{record}\n{prompt}\n"),
                    Path::new("encrypted.cab"),
                    100,
                )
                .is_err()
            );
        }
        for prompt in [
            "Enter password:unexpected\n",
            "Enter password (will not be echoed):unexpected\n",
            "Enter password\n",
            "Enter password:\nEnter password:\n",
        ] {
            assert!(
                parse_listing(
                    &format!("{prompt}{record}"),
                    Path::new("encrypted.cab"),
                    100,
                )
                .is_err()
            );
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn fixture(root: &Path, format: &str, extension: &str) -> PathBuf {
        let source = root.join("input");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("hello.txt"), b"archive integration proof").unwrap();
        let archive = root.join(format!("fixture.{extension}"));
        let mut command = command(
            &executable().expect("Run pnpm prepare:native before archive integration tests"),
            "a",
        )
        .unwrap();
        command
            .current_dir(&source)
            .arg(format!("-t{format}"))
            .arg(&archive)
            .arg("hello.txt");
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        archive
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn bundled_engine_lists_and_extracts_xz_bzip2_gzip_and_wim() {
        for (format, extension) in [
            ("xz", "xz"),
            ("bzip2", "bz2"),
            ("gzip", "gz"),
            ("wim", "wim"),
        ] {
            let temporary = TempDir::new().unwrap();
            let root = temporary.path().join("No files to process");
            fs::create_dir(&root).unwrap();
            let archive = fixture(&root, format, extension);
            assert!(is_archive(&archive));
            let info = list_archive_contents(&archive).unwrap();
            assert_eq!(info.entry_count, 1);
            let output = root.join("output");
            let written = extract_archive(&archive, &output).unwrap();
            assert_eq!(written, b"archive integration proof".len() as u64);
            assert_eq!(
                fs::read(output.join(&info.entries[0].path)).unwrap(),
                b"archive integration proof"
            );
            assert!(!archive_needs_password(&archive).unwrap());
        }
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn bundled_engine_failure_limits_and_cancellation_publish_nothing() {
        let temporary = TempDir::new().unwrap();
        let archive = fixture(temporary.path(), "bzip2", "bz2");
        let output = temporary.path().join("output");
        let cancelled = AtomicBool::new(false);
        for limits in [
            ExtractionLimits {
                max_entry_bytes: 4,
                ..ExtractionLimits::default()
            },
            ExtractionLimits {
                max_available_bytes: 0,
                ..ExtractionLimits::extended()
            },
            ExtractionLimits {
                max_entries: 0,
                ..ExtractionLimits::default()
            },
        ] {
            assert!(
                extract_archive_with_password_and_limits(
                    &archive, &output, None, limits, &cancelled
                )
                .is_err()
            );
            assert!(!output.exists());
        }
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            extract_archive_with_password_and_limits(
                &archive,
                &output,
                None,
                ExtractionLimits::default(),
                &cancelled
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        // A single-stream archive (one file) also never lands in an existing
        // folder.
        cancelled.store(false, Ordering::Release);
        fs::create_dir(&output).unwrap();
        fs::write(output.join("input"), "original").unwrap();
        assert_eq!(
            extract_archive(&archive, &output).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            fs::read_to_string(output.join("input")).unwrap(),
            "original"
        );
        fs::remove_dir_all(&output).unwrap();
        fs::write(&archive, "not an archive").unwrap();
        assert!(extract_archive(&archive, &output).is_err());
        assert!(!output.exists());
        assert!(fs::read_dir(temporary.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("explorie-extract")
        }));
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn bundled_engine_matches_literal_names_and_rejects_traversal() {
        let temporary = TempDir::new().unwrap();
        // ZIP contents with a fallback suffix exercise the full engine's content
        // detection independently of the existing Rust ZIP dispatcher.
        let archive = temporary.path().join("fixture.cab");
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        for name in [
            "@list.txt",
            "-switch.txt",
            "שלום = data.txt",
            "empty",
            "nested/file.txt",
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            if name != "empty" {
                zip.write_all(name.as_bytes()).unwrap();
            }
        }
        zip.finish().unwrap();
        let output = temporary.path().join("output");
        extract_archive(&archive, &output).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("@list.txt")).unwrap(),
            "@list.txt"
        );
        assert_eq!(
            fs::read_to_string(output.join("שלום = data.txt")).unwrap(),
            "שלום = data.txt"
        );
        assert_eq!(fs::metadata(output.join("empty")).unwrap().len(), 0);
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        zip.start_file("../escape", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"bad").unwrap();
        zip.finish().unwrap();
        let traversal_output = temporary.path().join("traversal-output");
        assert!(extract_archive(&archive, &traversal_output).is_err());
        assert!(!traversal_output.exists());
        assert!(!temporary.path().join("escape").exists());
        assert_eq!(
            fs::read_to_string(output.join("@list.txt")).unwrap(),
            "@list.txt"
        );
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn bundled_engine_accepts_passwords_via_stdin_for_encrypted_headers() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("secret.txt");
        let content =
            b"\nEnter password:\nEnter password (will not be echoed):\n\0\xfffixture content";
        fs::write(&source, content).unwrap();
        let archive = temporary.path().join("encrypted.cab");
        let mut create = command(&executable().unwrap(), "a").unwrap();
        // This is a fixed test password, not user data. Production supplies
        // passwords exclusively through the child's stdin.
        let status = create
            .args(["-t7z", "-pfixture-password", "-mhe=on"])
            .arg(&archive)
            .arg(&source)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        let output = temporary.path().join("output");
        assert!(extract_archive_with_password(&archive, &output, Some("wrong")).is_err());
        assert!(!output.exists());
        extract_archive_with_password(&archive, &output, Some("fixture-password")).unwrap();
        assert_eq!(fs::read(output.join("secret.txt")).unwrap(), content);
    }

    #[test]
    fn cancels_a_decoder_that_has_stopped_producing_output() {
        let cancelled = AtomicBool::new(false);
        let (ready, started) = mpsc::channel();
        thread::scope(|scope| {
            let cancellation = &cancelled;
            scope.spawn(move || {
                started.recv_timeout(Duration::from_secs(10)).unwrap();
                cancellation.store(true, Ordering::Release);
            });
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    "archive::sevenzip::tests::decoder_wait_fixture",
                    "--nocapture",
                ])
                .env("EXPLORIE_SEVENZIP_WAIT_FIXTURE", "1");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let mut output = Vec::new();
            let error = run(
                &mut command,
                None,
                &cancelled,
                Some(Duration::from_secs(10)),
                |bytes| {
                    output.extend_from_slice(bytes);
                    if String::from_utf8_lossy(&output).contains("decoder-ready") {
                        let _ = ready.send(());
                    }
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        });
    }

    #[test]
    #[ignore = "child-process fixture for cancellation test"]
    fn decoder_wait_fixture() {
        if std::env::var_os("EXPLORIE_SEVENZIP_WAIT_FIXTURE").is_some() {
            println!("decoder-ready");
            io::stdout().flush().unwrap();
            loop {
                thread::park();
            }
        }
    }
}
