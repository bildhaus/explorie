use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub(crate) enum ProcessError {
    Io(io::Error),
    TimedOut,
}

struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

pub(crate) fn run_with_timeout(
    command: &mut Command,
    timeout: Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Result<ProcessOutput, ProcessError> {
    command.stdout(if max_stdout == 0 {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    command.stderr(if max_stderr == 0 {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    let mut child = command.spawn().map_err(ProcessError::Io)?;
    let stdout = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || capture(stdout, max_stdout)));
    let stderr = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || capture(stderr, max_stderr)));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                join_capture(stdout);
                join_capture(stderr);
                return Err(ProcessError::TimedOut);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                join_capture(stdout);
                join_capture(stderr);
                return Err(ProcessError::Io(error));
            }
        }
    };
    let stdout = join_capture(stdout).unwrap_or(CapturedOutput {
        bytes: Vec::new(),
        truncated: false,
    });
    let stderr = join_capture(stderr).unwrap_or(CapturedOutput {
        bytes: Vec::new(),
        truncated: false,
    });
    Ok(ProcessOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

fn capture(mut reader: impl Read, limit: usize) -> CapturedOutput {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        truncated |= count > remaining;
    }
    CapturedOutput { bytes, truncated }
}

fn join_capture(reader: Option<thread::JoinHandle<CapturedOutput>>) -> Option<CapturedOutput> {
    reader.and_then(|reader| reader.join().ok())
}
