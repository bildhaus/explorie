use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(any(windows, target_os = "macos"))]
use crate::APP_IDENTIFIER;

const ENDPOINT_FILE: &str = "single-instance-v1.json";
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_millis(25);
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const WAKE_TIMEOUT: Duration = Duration::from_millis(500);
const ACCEPT_ERROR_DELAY: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SingleInstanceRequest {
    pub path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct SingleInstancePrimary {
    pub guard: SingleInstanceGuard,
    pub requests: mpsc::Receiver<SingleInstanceRequest>,
}

#[derive(Debug)]
pub struct SingleInstanceGuard {
    _platform: PlatformSingleInstanceGuard,
    _server: SingleInstanceServer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EndpointDescriptor {
    port: u16,
    token: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireRequest {
    token: String,
    path: Option<String>,
}

#[derive(Debug)]
struct SingleInstanceServer {
    endpoint_path: PathBuf,
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for SingleInstanceServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The listener blocks in `accept`; a local connection wakes it so it can
        // observe `stop`. If that fails, detach rather than hang on join.
        let woke = TcpStream::connect_timeout(
            &SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.port).into(),
            WAKE_TIMEOUT,
        )
        .is_ok();
        if let Some(thread) = self.thread.take()
            && woke
        {
            let _ = thread.join();
        }

        let owns_endpoint = fs::read(&self.endpoint_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<EndpointDescriptor>(&bytes).ok())
            .is_some_and(|endpoint| endpoint.token == self.token);
        if owns_endpoint {
            let _ = fs::remove_file(&self.endpoint_path);
        }
    }
}

pub fn acquire_single_instance(
    config_dir: &Path,
    launch_path: Option<&Path>,
) -> io::Result<Option<SingleInstancePrimary>> {
    let Some(platform) = try_acquire_platform_guard()? else {
        forward_to_primary(config_dir, launch_path)?;
        return Ok(None);
    };

    let (server, requests) = SingleInstanceServer::start(config_dir)?;
    Ok(Some(SingleInstancePrimary {
        guard: SingleInstanceGuard {
            _platform: platform,
            _server: server,
        },
        requests,
    }))
}

impl SingleInstanceServer {
    fn start(config_dir: &Path) -> io::Result<(Self, mpsc::Receiver<SingleInstanceRequest>)> {
        fs::create_dir_all(config_dir)?;
        let endpoint_path = config_dir.join(ENDPOINT_FILE);
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let token = Uuid::new_v4().to_string();
        let descriptor = EndpointDescriptor {
            port,
            token: token.clone(),
        };
        fs::write(&endpoint_path, serde_json::to_vec(&descriptor)?)?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_token = token.clone();
        let (sender, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("explorie-single-instance".to_string())
            .spawn(move || listen(listener, &thread_token, &thread_stop, sender))?;

        Ok((
            Self {
                endpoint_path,
                port,
                token,
                stop,
                thread: Some(thread),
            },
            receiver,
        ))
    }
}

fn listen(
    listener: TcpListener,
    token: &str,
    stop: &AtomicBool,
    sender: mpsc::Sender<SingleInstanceRequest>,
) {
    for stream in listener.incoming() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match stream {
            Ok(stream) => {
                if let Some(request) = read_request(stream, token) {
                    let _ = sender.send(request);
                }
            }
            // Back off on persistent failures such as descriptor exhaustion.
            Err(_) => thread::sleep(ACCEPT_ERROR_DELAY),
        }
    }
}

fn read_request(mut stream: TcpStream, expected_token: &str) -> Option<SingleInstanceRequest> {
    // Accepted sockets inherit a non-blocking listener's mode on macOS and
    // Windows, where an early read would fail with WouldBlock and drop the
    // request. Always read in blocking mode, bounded by a timeout.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    let mut bytes = Vec::new();
    let mut reader = io::BufReader::new(&mut stream).take(MAX_REQUEST_BYTES + 2);
    if reader.read_until(b'\n', &mut bytes).is_err() {
        return None;
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return None;
    }
    let request = serde_json::from_slice::<WireRequest>(&bytes).ok()?;
    if request.token != expected_token {
        return None;
    }
    Some(SingleInstanceRequest {
        path: request.path.map(PathBuf::from).filter(|path| path.is_dir()),
    })
}

fn forward_to_primary(config_dir: &Path, launch_path: Option<&Path>) -> io::Result<()> {
    let endpoint_path = config_dir.join(ENDPOINT_FILE);
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < FORWARD_TIMEOUT {
        match forward_once(&endpoint_path, launch_path) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(RETRY_DELAY);
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "existing explorie instance did not accept the launch request",
        )
    }))
}

fn forward_once(endpoint_path: &Path, launch_path: Option<&Path>) -> io::Result<()> {
    let endpoint = serde_json::from_slice::<EndpointDescriptor>(&fs::read(endpoint_path)?)?;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, endpoint.port).into(),
        Duration::from_millis(250),
    )?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let request = WireRequest {
        token: endpoint.token,
        path: launch_path.map(|path| path.to_string_lossy().into_owned()),
    };
    let mut bytes = serde_json::to_vec(&request)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "launch request is too large",
        ));
    }
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.shutdown(Shutdown::Write)?;
    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct PlatformSingleInstanceGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for PlatformSingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn try_acquire_platform_guard() -> io::Result<Option<PlatformSingleInstanceGuard>> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = std::ffi::OsStr::new(&format!("Local\\{APP_IDENTIFIER}"))
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(PlatformSingleInstanceGuard { handle }))
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct PlatformSingleInstanceGuard {
    file: fs::File,
}

#[cfg(target_os = "macos")]
impl Drop for PlatformSingleInstanceGuard {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(target_os = "macos")]
fn try_acquire_platform_guard() -> io::Result<Option<PlatformSingleInstanceGuard>> {
    use std::os::fd::AsRawFd;

    let path = std::env::temp_dir().join(format!("{APP_IDENTIFIER}.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(Some(PlatformSingleInstanceGuard { file }));
    }

    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK)
    {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
#[derive(Debug)]
struct PlatformSingleInstanceGuard;

#[cfg(not(any(windows, target_os = "macos")))]
fn try_acquire_platform_guard() -> io::Result<Option<PlatformSingleInstanceGuard>> {
    Ok(Some(PlatformSingleInstanceGuard))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST_DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

    fn fixture_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "explorie-single-instance-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(target_os = "macos")]
        let path = path.canonicalize().unwrap();
        path
    }

    #[test]
    fn forwards_valid_directory_and_activation_requests() {
        let config_dir = fixture_dir();
        let directory = fixture_dir();
        let (server, requests) = SingleInstanceServer::start(&config_dir).unwrap();

        forward_to_primary(&config_dir, Some(&directory)).unwrap();
        assert_eq!(
            requests
                .recv_timeout(REQUEST_DELIVERY_TIMEOUT)
                .expect("primary did not receive the directory launch request"),
            SingleInstanceRequest {
                path: Some(directory.clone())
            }
        );

        forward_to_primary(&config_dir, None).unwrap();
        assert_eq!(
            requests
                .recv_timeout(REQUEST_DELIVERY_TIMEOUT)
                .expect("primary did not receive the activation request"),
            SingleInstanceRequest { path: None }
        );

        drop(server);
        assert!(!config_dir.join(ENDPOINT_FILE).exists());
        fs::remove_dir_all(config_dir).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_bad_tokens_and_non_directory_paths() {
        let config_dir = fixture_dir();
        let missing = config_dir.join("missing");
        let (server, requests) = SingleInstanceServer::start(&config_dir).unwrap();
        let endpoint = serde_json::from_slice::<EndpointDescriptor>(
            &fs::read(config_dir.join(ENDPOINT_FILE)).unwrap(),
        )
        .unwrap();

        let bad_request = WireRequest {
            token: "wrong".to_string(),
            path: Some(missing.to_string_lossy().into_owned()),
        };
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port)).unwrap();
        stream
            .write_all(&serde_json::to_vec(&bad_request).unwrap())
            .unwrap();
        let _ = stream.shutdown(Shutdown::Write);
        assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());

        forward_to_primary(&config_dir, Some(&missing)).unwrap();
        assert_eq!(
            requests
                .recv_timeout(REQUEST_DELIVERY_TIMEOUT)
                .expect("primary did not receive the sanitized launch request"),
            SingleInstanceRequest { path: None }
        );

        drop(server);
        fs::remove_dir_all(config_dir).unwrap();
    }

    #[test]
    fn newline_framing_is_read_before_the_client_connection_closes() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let request = WireRequest {
            token: "test-token".to_string(),
            path: None,
        };
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        let mut client = TcpStream::connect(endpoint).unwrap();
        client.write_all(&bytes).unwrap();
        let (server_stream, _) = listener.accept().unwrap();

        assert_eq!(
            read_request(server_stream, "test-token"),
            Some(SingleInstanceRequest { path: None })
        );
    }

    #[test]
    fn request_written_after_accept_on_a_non_blocking_listener_is_not_dropped() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(endpoint).unwrap();
        let started = Instant::now();
        let server_stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < REQUEST_DELIVERY_TIMEOUT);
                    thread::yield_now();
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        // A loaded client can be descheduled between connect and write; the
        // accepted socket must wait for the data instead of failing early.
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            let mut bytes = serde_json::to_vec(&WireRequest {
                token: "test-token".to_string(),
                path: None,
            })
            .unwrap();
            bytes.push(b'\n');
            client.write_all(&bytes).unwrap();
            client
        });

        assert_eq!(
            read_request(server_stream, "test-token"),
            Some(SingleInstanceRequest { path: None })
        );
        drop(writer.join().unwrap());
    }

    #[test]
    fn dropping_the_server_stops_the_blocking_listener_promptly() {
        let config_dir = fixture_dir();
        let (server, requests) = SingleInstanceServer::start(&config_dir).unwrap();
        let started = Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            requests.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        fs::remove_dir_all(config_dir).unwrap();
    }
}
