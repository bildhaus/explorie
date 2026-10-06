#![cfg_attr(windows, windows_subsystem = "windows")]
// Linux builds only compile-check the app; its `main` has no product, so the
// desktop imports below are unused there by design.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(unused_imports))]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use explorie_gpui::SingleInstanceRequest;
use explorie_gpui::{
    APP_NAME, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, DirectoryWindow, ExplorieAssets,
    RecoveryMarker, WindowRuntime, acquire_single_instance, desktop_window_options,
    initial_window_bounds, parse_startup_path,
};
use explorie_native_services::{NativeServices, ResourcePaths};
use gpui::{App, AppContext, Bounds, Focusable, WindowHandle, px, size};
#[cfg(not(target_os = "windows"))]
use gpui_platform::application;

#[cfg(target_os = "windows")]
fn application() -> gpui::Application {
    gpui::Application::with_platform(std::rc::Rc::new(
        gpui_windows::WindowsPlatform::new(false)
            .expect("failed to initialize the Windows GPUI platform"),
    ))
}

#[cfg(any(windows, target_os = "macos"))]
type SharedRequests = Arc<Mutex<mpsc::Receiver<SingleInstanceRequest>>>;

#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone)]
enum PrimaryWindowOpen {
    Initial {
        previous_session_unclean: bool,
        plugin_startup_error: Option<String>,
    },
    #[cfg(target_os = "macos")]
    Reopen,
}

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    explorie_native_services::helper::run_if_requested();
    #[cfg(target_os = "macos")]
    if let Some(result) =
        explorie_native_services::updater::apply_macos_update_command(std::env::args_os())
    {
        if let Err(error) = result {
            eprintln!("unable to apply macOS update: {error}");
        }
        return;
    }
    #[cfg(windows)]
    if let Some(installer) = installer_cleanup_argument(std::env::args_os()) {
        if let Err(error) = cleanup_windows_installer(&installer) {
            eprintln!("unable to remove installer: {error}");
        }
        return;
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        if let Some(enabled) = system_integration_command(std::env::args_os()) {
            if let Err(error) =
                explorie_native_services::integration::set_system_integration(enabled)
            {
                eprintln!("unable to update folder-open integration: {error}");
                std::process::exit(1);
            }
            return;
        }
    }
    let explicit_path = parse_startup_path(std::env::args_os());
    let startup_path_is_explicit = explicit_path.is_some();
    let services = native_services();
    let Some(instance) =
        acquire_single_instance(&services.resources().config_dir, explicit_path.as_deref())
            .expect("unable to acquire or contact app instance")
    else {
        return;
    };
    let _instance = instance.guard;
    let plugin_startup_error =
        explorie_gpui::initialize_plugins(&services, std::env::args_os()).err();
    let single_instance_requests = Arc::new(Mutex::new(instance.requests));
    let path = explicit_path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let recovery_marker = RecoveryMarker::begin(&services.resources().config_dir)
        .map_err(|error| eprintln!("native recovery tracking unavailable: {error}"))
        .ok();
    let previous_session_unclean = recovery_marker
        .as_ref()
        .is_some_and(RecoveryMarker::previous_session_unclean);
    let config_dir = services.resources().config_dir.clone();
    let (window_runtime, restored_window_sessions) = WindowRuntime::open(&config_dir);

    #[cfg(target_os = "macos")]
    let (open_url_tx, open_url_rx) = mpsc::channel();
    #[cfg(target_os = "macos")]
    let open_url_requests = Arc::new(Mutex::new(open_url_rx));
    let request_sources = vec![single_instance_requests];
    #[cfg(target_os = "macos")]
    let mut request_sources = request_sources;
    #[cfg(target_os = "macos")]
    request_sources.push(open_url_requests);
    let application = application().with_assets(ExplorieAssets);
    #[cfg(target_os = "macos")]
    application.on_open_urls(move |urls| {
        for url in urls {
            if let Some(path) = path_from_open_url(&url) {
                let _ = open_url_tx.send(SingleInstanceRequest { path: Some(path) });
            }
        }
    });
    #[cfg(target_os = "macos")]
    {
        let reopen_path = path.clone();
        let reopen_services = services.clone();
        let reopen_runtime = window_runtime.clone();
        let reopen_requests = request_sources.clone();
        application.on_reopen(move |cx| {
            if let Err(error) = open_primary_window(
                cx,
                reopen_path.clone(),
                false,
                reopen_services.clone(),
                reopen_runtime.clone(),
                reopen_requests.clone(),
                PrimaryWindowOpen::Reopen,
            ) {
                eprintln!("unable to reopen Explorie window: {error}");
            }
        });
    }

    application.run(move |cx: &mut App| {
        let quitting_runtime = window_runtime.clone();
        cx.on_app_quit(move |_| {
            quitting_runtime.mark_quitting();
            async {}
        })
        .detach();
        let shutdown_remotes = services.remotes.clone();
        let shutdown_audio = services.audio.clone();
        let shutdown_video = services.video.clone();
        let shutdown_plugins = services.plugins.clone();
        cx.on_app_quit(move |_| {
            let task = shutdown_plugins.shutdown();
            async move {
                let _ = task.await;
            }
        })
        .detach();
        cx.on_app_quit(move |_| {
            shutdown_audio.stop();
            shutdown_video.stop();
            async {}
        })
        .detach();
        cx.on_app_quit(move |_| {
            let task = shutdown_remotes.disconnect_all();
            async move {
                if let Err(error) = task.await {
                    eprintln!("unable to stop remote drives during shutdown: {error}");
                }
            }
        })
        .detach();
        #[cfg(target_os = "macos")]
        {
            let menu_path = path.clone();
            let menu_services = services.clone();
            let menu_runtime = window_runtime.clone();
            let menu_requests = request_sources.clone();
            explorie_gpui::install_app_menus(cx, services.clone(), move |cx| {
                open_primary_window(
                    cx,
                    menu_path.clone(),
                    false,
                    menu_services.clone(),
                    menu_runtime.clone(),
                    menu_requests.clone(),
                    PrimaryWindowOpen::Reopen,
                )
                .map_err(|error| eprintln!("unable to reopen Explorie window: {error}"))
                .ok()
            });
        }
        if let Some(marker) = recovery_marker.clone() {
            cx.on_app_quit(move |_| {
                let marker = marker.clone();
                async move {
                    if let Err(error) = marker.clear() {
                        eprintln!("unable to clear native recovery marker: {error}");
                    }
                }
            })
            .detach();
        }
        open_primary_window(
            cx,
            path.clone(),
            startup_path_is_explicit,
            services.clone(),
            window_runtime.clone(),
            request_sources,
            PrimaryWindowOpen::Initial {
                previous_session_unclean,
                plugin_startup_error,
            },
        )
        .expect("unable to open explorie window");
        for session_id in restored_window_sessions {
            let fallback_bounds = Bounds::centered(
                None,
                size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
                cx,
            );
            let options = desktop_window_options(fallback_bounds);
            let services = services.clone();
            let runtime = window_runtime.clone();
            let fallback_path = path.clone();
            if let Err(error) = cx.open_window(options, move |window, cx| {
                window.set_window_title(APP_NAME);
                let view = cx.new(|cx| {
                    let mut view = DirectoryWindow::restore_window_session(
                        fallback_path,
                        false,
                        services,
                        Some(runtime),
                        session_id,
                        cx,
                    );
                    view.install_shortcut_bindings(cx);
                    view.start_listing(cx);
                    view.start_watching(cx);
                    view.start_system_locations(cx);
                    view.start_service_events(cx);
                    view.start_preview_helpers(cx);
                    view.start_system_integration_status(cx);
                    view.start_remote_drives(cx);
                    view
                });
                let close_view = view.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    close_view.update(cx, |view, cx| view.request_platform_window_close(cx))
                });
                window.focus(&view.focus_handle(cx), cx);
                view
            }) {
                eprintln!("unable to restore Explorie window: {error}");
            }
        }
        cx.activate(true);
    });
}

#[cfg(any(windows, target_os = "macos"))]
fn open_primary_window(
    cx: &mut App,
    path: PathBuf,
    startup_path_is_explicit: bool,
    services: NativeServices,
    runtime: WindowRuntime,
    request_sources: Vec<SharedRequests>,
    open: PrimaryWindowOpen,
) -> Result<WindowHandle<DirectoryWindow>, String> {
    let fallback_bounds = Bounds::centered(
        None,
        size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
        cx,
    );
    let bounds = initial_window_bounds(&services.resources().config_dir, fallback_bounds, cx);
    let options = desktop_window_options(bounds);
    cx.open_window(options, move |window, cx| {
        window.set_window_title(APP_NAME);
        let view = cx.new(|cx| {
            let mut view = DirectoryWindow::restore_window_session(
                path,
                startup_path_is_explicit,
                services,
                Some(runtime),
                "primary".to_string(),
                cx,
            );
            if let PrimaryWindowOpen::Initial {
                plugin_startup_error: Some(error),
                ..
            } = &open
            {
                view.announce_plugin_startup_error(error.clone(), cx);
            }
            view.install_shortcut_bindings(cx);
            view.start_listing(cx);
            view.start_watching(cx);
            view.start_system_locations(cx);
            view.start_service_events(cx);
            view.start_preview_helpers(cx);
            view.start_system_integration_status(cx);
            view.start_remote_drives(cx);
            #[cfg(target_os = "macos")]
            if matches!(open, PrimaryWindowOpen::Initial { .. }) {
                view.start_install_cleanup_offer(cx);
            }
            if matches!(open, PrimaryWindowOpen::Initial { .. }) {
                view.start_update_check(false, cx);
            }
            if matches!(
                open,
                PrimaryWindowOpen::Initial {
                    previous_session_unclean: true,
                    ..
                }
            ) {
                view.announce_unclean_recovery(cx);
            }
            view
        });
        view.update(cx, |view, cx| {
            for requests in request_sources {
                view.start_single_instance_requests(requests, window, cx);
            }
        });
        let close_view = view.clone();
        window.on_window_should_close(cx, move |_, cx| {
            close_view.update(cx, |view, cx| view.request_platform_window_close(cx))
        });
        window.focus(&view.focus_handle(cx), cx);
        view
    })
    .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn installer_cleanup_argument(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<PathBuf> {
    let mut args = args.into_iter().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--cleanup-installer" {
            return Some(args.next().map(PathBuf::from).unwrap_or_default());
        }
    }
    None
}

#[cfg(windows)]
fn cleanup_windows_installer(installer: &std::path::Path) -> std::io::Result<()> {
    let name = installer
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !installer.is_absolute()
        || installer
            .extension()
            .is_none_or(|extension| extension != "exe")
        || !name.starts_with("explorie")
        || !name.contains("windows-x64-setup")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to remove a path that is not an Explorie Windows installer",
        ));
    }
    for _ in 0..120 {
        match std::fs::remove_file(installer) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "the installer remained in use for 30 seconds",
    ))
}

#[cfg(any(windows, target_os = "macos"))]
fn system_integration_command(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<bool> {
    args.into_iter()
        .skip(1)
        .find_map(|argument| match argument.to_str() {
            Some("--register-folder-handler") => Some(true),
            Some("--unregister-folder-handler") => Some(false),
            _ => None,
        })
}

#[cfg(target_os = "macos")]
fn path_from_open_url(url: &str) -> Option<PathBuf> {
    let raw = url.strip_prefix("file://")?;
    let raw = raw.strip_prefix("localhost").unwrap_or(raw);
    if !raw.starts_with('/') {
        return None;
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push((hex_value(high)? << 4) | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn installer_cleanup_command_requires_an_explicit_installer_path() {
        let path = installer_cleanup_argument([
            "Explorie.exe".into(),
            "--cleanup-installer".into(),
            r"C:\Users\fixture\Downloads\explorie-0.2.13-windows-x64-setup-unsigned.exe".into(),
        ])
        .unwrap();
        assert!(path.is_absolute());
        assert!(installer_cleanup_argument(["Explorie.exe".into()]).is_none());
    }

    #[test]
    fn installer_cleanup_refuses_unrelated_executables() {
        let error = cleanup_windows_installer(std::path::Path::new(
            r"C:\Users\fixture\Downloads\unrelated.exe",
        ))
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn native_services() -> NativeServices {
    let resources = ResourcePaths::default().with_app_version(env!("CARGO_PKG_VERSION"));
    let resources = std::env::var_os("EXPLORIE_TEST_CONFIG_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map_or(resources.clone(), |path| {
            resources
                .with_config_dir(&path)
                .with_cache_dir(path.join("cache"))
        });
    NativeServices::new(resources)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("{APP_NAME} has no Linux product target");
}
