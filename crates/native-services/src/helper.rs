//! Out-of-process helpers for native parsers that are not memory safe.
//!
//! Some previews (3D models through assimp) run large C/C++ parsers on
//! untrusted files. A crash, abort or runaway allocation in them must become a
//! preview error rather than take the app down, so they run in a short-lived
//! copy of the current executable started with [`HELPER_FLAG`]. The request
//! arrives on stdin, the response leaves as one framed message on stdout, and
//! the parent enforces a deadline, an output bound and a memory limit.
//!
//! The desktop binary opts in by calling [`run_if_requested`] first thing in
//! `main`. Executables that never call it (tests of other crates, the CLI)
//! keep parsing in-process, because re-executing them would not reach a helper.
//!
//! 3D models are the only helper-backed preview, so builds without the
//! `preview-3d` feature leave out the parent side; a helper started anyway
//! answers every request as unknown.

use crate::{ErrorCode, ServiceError, ServiceResult};
use std::ffi::OsStr;
use std::io::{Read, Write};
#[cfg(any(feature = "preview-3d", test))]
use std::path::{Path, PathBuf};
#[cfg(any(feature = "preview-3d", test))]
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(feature = "preview-3d", test))]
use std::time::{Duration, Instant};

/// First argument that turns the process into a helper.
pub const HELPER_FLAG: &str = "--explorie-helper";
const FRAME_MAGIC: &[u8; 8] = b"EXPLHLP1";
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

static HELPERS_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Serve a helper request and exit if this process was started as a helper;
/// otherwise record that this executable can serve helpers and return.
pub fn run_if_requested() {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new(HELPER_FLAG)) {
        HELPERS_AVAILABLE.store(true, Ordering::Release);
        return;
    }
    let kind = arguments
        .next()
        .and_then(|kind| kind.into_string().ok())
        .unwrap_or_default();
    std::process::exit(serve_stdio(&kind));
}

#[cfg(feature = "preview-3d")]
pub(crate) fn is_available() -> bool {
    cfg!(test) || HELPERS_AVAILABLE.load(Ordering::Acquire)
}

/// Resource limits the parent enforces on one helper run.
#[cfg(any(feature = "preview-3d", test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct HelperLimits {
    pub timeout: Duration,
    pub memory_bytes: u64,
    pub max_output_bytes: usize,
}

/// Run one request in a helper process and return its response payload.
#[cfg(any(feature = "preview-3d", test))]
pub(crate) fn run(kind: &str, request: &[u8], limits: HelperLimits) -> ServiceResult<Vec<u8>> {
    let mut command = helper_command(kind)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|error| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            format!("Unable to start the preview helper: {error}"),
        )
    })?;
    // The helper blocks reading stdin, so it touches no untrusted data before
    // the job limits are in place.
    #[cfg(windows)]
    let job = match windows_job::ProcessJob::attach(&child, limits.memory_bytes) {
        Ok(job) => job,
        Err(error) => {
            terminate(&mut child);
            return Err(error);
        }
    };
    let written = child
        .stdin
        .take()
        .ok_or_else(|| ServiceError::new(ErrorCode::Internal, "Preview helper stdin is missing"))
        .and_then(|mut stdin| stdin.write_all(request).map_err(ServiceError::from));
    if let Err(error) = written {
        terminate(&mut child);
        return Err(error);
    }
    let Some(stdout) = child.stdout.take() else {
        terminate(&mut child);
        return Err(ServiceError::new(
            ErrorCode::Internal,
            "Preview helper stdout is missing",
        ));
    };
    let max_output = limits.max_output_bytes;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(max_output as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let status = wait_within_limits(&mut child, limits);
    #[cfg(windows)]
    drop(job);
    let output = reader.join().ok().and_then(Result::ok).unwrap_or_default();
    let status = status?;
    if output.len() > max_output {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "The preview helper returned more data than Explorie accepts",
        ));
    }
    match parse_frame(&output) {
        Some(Ok(payload)) => Ok(payload),
        Some(Err(error)) => Err(error),
        None => Err(ServiceError::new(
            ErrorCode::Internal,
            format!(
                "The preview helper stopped unexpectedly ({})",
                describe_exit(status)
            ),
        )),
    }
}

#[cfg(any(feature = "preview-3d", test))]
fn wait_within_limits(child: &mut Child, limits: HelperLimits) -> ServiceResult<ExitStatus> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                terminate(child);
                return Err(ServiceError::from(error));
            }
        }
        if started.elapsed() >= limits.timeout {
            terminate(child);
            return Err(ServiceError::new(
                ErrorCode::Busy,
                format!(
                    "The preview helper timed out after {} seconds",
                    limits.timeout.as_secs()
                ),
            )
            .retryable(true));
        }
        if memory_footprint(child).is_some_and(|bytes| bytes > limits.memory_bytes) {
            terminate(child);
            return Err(ServiceError::new(
                ErrorCode::Unsupported,
                "The preview helper exceeded its memory limit",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Physical memory attributed to the helper, where the platform can report it
/// cheaply. Windows enforces the limit through the job object instead.
#[cfg(all(target_os = "macos", any(feature = "preview-3d", test)))]
fn memory_footprint(child: &Child) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `info` is a writable rusage_info_v2 buffer, which is what the
    // RUSAGE_INFO_V2 flavor fills in; the call only reads the child's PID.
    let status = unsafe {
        libc::proc_pid_rusage(
            child.id() as libc::c_int,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: a zero status means the kernel initialized the structure.
    (status == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(all(not(target_os = "macos"), any(feature = "preview-3d", test)))]
fn memory_footprint(_child: &Child) -> Option<u64> {
    None
}

#[cfg(any(feature = "preview-3d", test))]
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: the helper was started in its own process group, so signalling
    // the negative PID reaches only the helper and anything it started.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(any(feature = "preview-3d", test))]
fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("terminated by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "unknown exit status".to_string(),
    }
}

#[cfg(all(feature = "preview-3d", not(test)))]
fn helper_command(kind: &str) -> ServiceResult<Command> {
    let mut command = Command::new(std::env::current_exe().map_err(ServiceError::from)?);
    command.arg(HELPER_FLAG).arg(kind);
    Ok(command)
}

/// Unit tests re-run the test binary on a single test that serves the request,
/// because the libtest harness owns `main`.
#[cfg(test)]
fn helper_command(kind: &str) -> ServiceResult<Command> {
    let mut command = Command::new(std::env::current_exe().map_err(ServiceError::from)?);
    command
        .args([
            "--exact",
            "helper::tests::helper_process_entry_point",
            "--nocapture",
            "--test-threads=1",
            "--quiet",
        ])
        .env(tests::TEST_CHILD_ENV, kind);
    Ok(command)
}

/// Helper-side entry: apply limits, read the request, answer on stdout.
fn serve_stdio(kind: &str) -> i32 {
    restrict_helper_process();
    let mut request = Vec::new();
    let response = std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(ServiceError::from)
        .and_then(|_| {
            if request.len() as u64 > MAX_REQUEST_BYTES {
                Err(ServiceError::new(
                    ErrorCode::InvalidInput,
                    "The preview helper request is too large",
                ))
            } else {
                dispatch(kind, &request)
            }
        });
    let frame = encode_frame(&response);
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(&frame)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return 1;
    }
    0
}

fn dispatch(kind: &str, request: &[u8]) -> ServiceResult<Vec<u8>> {
    // Without `preview-3d` no helper kind reads the request.
    #[cfg(not(any(feature = "preview-3d", test)))]
    let _ = request;
    match kind {
        #[cfg(feature = "preview-3d")]
        crate::model_preview::GEOMETRY_HELPER => {
            crate::model_preview::serve_geometry_request(request)
        }
        #[cfg(test)]
        _ if kind.starts_with("test-") => tests::dispatch_test_kind(kind, request),
        _ => Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Unknown preview helper request",
        )),
    }
}

/// Defense in depth for the helper itself: no core dumps, no file writes, no
/// child processes, and a CPU-time ceiling that outlasts the parent deadline.
fn restrict_helper_process() {
    #[cfg(unix)]
    for (resource, limit) in [
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_FSIZE, 0),
        (libc::RLIMIT_NPROC, 0),
        (libc::RLIMIT_CPU, 120),
    ] {
        let limit = libc::rlimit {
            rlim_cur: limit,
            rlim_max: limit,
        };
        // SAFETY: setrlimit only reads the provided rlimit structure; lowering
        // limits for the current process is always permitted.
        unsafe {
            libc::setrlimit(resource, &limit);
        }
    }
}

fn encode_frame(response: &ServiceResult<Vec<u8>>) -> Vec<u8> {
    let (status, payload) = match response {
        Ok(payload) => (0_u8, payload.clone()),
        Err(error) => {
            let mut payload = vec![error_code_byte(&error.code)];
            payload.extend_from_slice(error.message.as_bytes());
            (1_u8, payload)
        }
    };
    let mut frame = Vec::with_capacity(FRAME_MAGIC.len() + 9 + payload.len());
    frame.extend_from_slice(FRAME_MAGIC);
    frame.push(status);
    frame.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// Find the framed response in the helper's stdout. Anything before the magic
/// (the libtest banner in unit tests) is ignored; a truncated frame is `None`.
#[cfg(any(feature = "preview-3d", test))]
fn parse_frame(output: &[u8]) -> Option<ServiceResult<Vec<u8>>> {
    let start = output
        .windows(FRAME_MAGIC.len())
        .position(|window| window == FRAME_MAGIC)?
        + FRAME_MAGIC.len();
    let status = *output.get(start)?;
    let length = u64::from_le_bytes(output.get(start + 1..start + 9)?.try_into().ok()?);
    let length = usize::try_from(length).ok()?;
    let payload = output.get(start + 9..start.checked_add(9)?.checked_add(length)?)?;
    match status {
        0 => Some(Ok(payload.to_vec())),
        1 => {
            let (&code, message) = payload.split_first()?;
            Some(Err(ServiceError::new(
                error_code_from_byte(code),
                String::from_utf8_lossy(message).into_owned(),
            )))
        }
        _ => None,
    }
}

fn error_code_byte(code: &ErrorCode) -> u8 {
    match code {
        ErrorCode::InvalidInput => 1,
        ErrorCode::Unsupported => 2,
        ErrorCode::NotFound => 3,
        ErrorCode::PermissionDenied => 4,
        ErrorCode::Io => 5,
        _ => 0,
    }
}

#[cfg(any(feature = "preview-3d", test))]
fn error_code_from_byte(byte: u8) -> ErrorCode {
    match byte {
        1 => ErrorCode::InvalidInput,
        2 => ErrorCode::Unsupported,
        3 => ErrorCode::NotFound,
        4 => ErrorCode::PermissionDenied,
        5 => ErrorCode::Io,
        _ => ErrorCode::Internal,
    }
}

/// Encode a path losslessly for a helper request.
#[cfg(any(feature = "preview-3d", test))]
pub(crate) fn encode_path(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}

#[cfg(any(feature = "preview-3d", test))]
pub(crate) fn decode_path(bytes: &[u8]) -> ServiceResult<PathBuf> {
    let invalid = || ServiceError::new(ErrorCode::InvalidInput, "Invalid preview helper path");
    if bytes.is_empty() {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(OsStr::from_bytes(bytes)))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let (pairs, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(invalid());
        }
        let wide: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
    }
}

#[cfg(all(windows, any(feature = "preview-3d", test)))]
mod windows_job {
    use crate::{ErrorCode, ServiceError, ServiceResult};
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// Kills the helper when dropped, caps its committed memory and keeps it
    /// from starting other processes.
    pub(super) struct ProcessJob(HANDLE);

    impl ProcessJob {
        pub(super) fn attach(child: &Child, memory_bytes: u64) -> ServiceResult<Self> {
            use std::os::windows::io::AsRawHandle;
            let failure = || {
                ServiceError::new(
                    ErrorCode::Internal,
                    "Could not contain the preview helper process",
                )
            };
            // SAFETY: both arguments may be null for an unnamed job with
            // default security; the returned handle is owned by ProcessJob.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(failure());
            }
            let job = Self(handle);
            // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a
            // valid "no limits" value that is then filled in below.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.ProcessMemoryLimit = usize::try_from(memory_bytes).unwrap_or(usize::MAX);
            // SAFETY: the job handle is valid, the pointer and size describe
            // `limits`, and the child handle stays open for the call.
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ) != 0
                    && AssignProcessToJobObject(handle, child.as_raw_handle().cast()) != 0
            };
            if ok { Ok(job) } else { Err(failure()) }
        }
    }

    impl Drop for ProcessJob {
        fn drop(&mut self) {
            // SAFETY: the handle is owned by this value and closed exactly once.
            unsafe {
                TerminateJobObject(self.0, 1);
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const TEST_CHILD_ENV: &str = "EXPLORIE_HELPER_TEST_KIND";

    const LIMITS: HelperLimits = HelperLimits {
        timeout: Duration::from_secs(30),
        memory_bytes: 2 * 1024 * 1024 * 1024,
        max_output_bytes: 1024 * 1024,
    };

    /// Not a real test: when the helper tests re-run this binary with
    /// `TEST_CHILD_ENV` set, this entry point becomes the helper process.
    #[test]
    fn helper_process_entry_point() {
        if let Ok(kind) = std::env::var(TEST_CHILD_ENV) {
            std::process::exit(serve_stdio(&kind));
        }
    }

    pub(crate) fn dispatch_test_kind(kind: &str, request: &[u8]) -> ServiceResult<Vec<u8>> {
        match kind {
            "test-echo" => Ok(request.iter().rev().copied().collect()),
            "test-error" => Err(ServiceError::new(
                ErrorCode::Unsupported,
                "helper refused the request",
            )),
            "test-abort" => std::process::abort(),
            "test-hang" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            "test-flood" => Ok(vec![7_u8; 4 * 1024 * 1024]),
            "test-allocate" => {
                let mut blocks = Vec::new();
                loop {
                    blocks.push(vec![1_u8; 64 * 1024 * 1024]);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            "test-spawn" => match Command::new("/bin/echo").status() {
                Ok(_) => Ok(b"spawned".to_vec()),
                Err(_) => Ok(b"blocked".to_vec()),
            },
            _ => Err(ServiceError::new(
                ErrorCode::InvalidInput,
                "unknown test kind",
            )),
        }
    }

    #[test]
    fn helpers_answer_over_stdio_and_errors_keep_their_codes() {
        assert_eq!(run("test-echo", b"abc", LIMITS).unwrap(), b"cba");
        let error = run("test-error", b"", LIMITS).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(error.message, "helper refused the request");
        let error = run("no-such-helper", b"", LIMITS).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn helper_crashes_timeouts_and_floods_become_errors() {
        let error = run("test-abort", b"", LIMITS).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.contains("stopped unexpectedly"), "{error:?}");

        let started = Instant::now();
        let error = run(
            "test-hang",
            b"",
            HelperLimits {
                timeout: Duration::from_millis(1500),
                ..LIMITS
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Busy);
        assert!(started.elapsed() < Duration::from_secs(20));

        let error = run("test-flood", b"", LIMITS).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }

    #[test]
    fn helper_memory_is_bounded() {
        let error = run(
            "test-allocate",
            b"",
            HelperLimits {
                memory_bytes: 256 * 1024 * 1024,
                ..LIMITS
            },
        )
        .unwrap_err();
        assert!(
            matches!(error.code, ErrorCode::Unsupported | ErrorCode::Internal),
            "{error:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn helpers_cannot_start_processes() {
        // RLIMIT_NPROC does not apply to root, so the helper can only be kept
        // from forking when explorie runs as an ordinary user.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert_eq!(run("test-spawn", b"", LIMITS).unwrap(), b"blocked");
    }

    #[test]
    fn frames_and_paths_round_trip() {
        let frame = encode_frame(&Ok(b"payload".to_vec()));
        let mut noisy = b"\nrunning 1 test\n".to_vec();
        noisy.extend_from_slice(&frame);
        assert_eq!(parse_frame(&noisy).unwrap().unwrap(), b"payload");
        assert!(parse_frame(&frame[..frame.len() - 1]).is_none());
        assert!(parse_frame(b"no frame").is_none());
        let error = parse_frame(&encode_frame(&Err(ServiceError::new(
            ErrorCode::NotFound,
            "gone",
        ))))
        .unwrap()
        .unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (ErrorCode::NotFound, "gone")
        );

        let path = Path::new("/tmp/models/ünïcode scene.glb");
        assert_eq!(decode_path(&encode_path(path)).unwrap(), path);
        assert!(decode_path(b"").is_err());
    }
}
