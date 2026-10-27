//! Ejecting a removable volume the way the platform file manager does: a
//! disk image is detached and a USB drive is made safe to unplug.

use crate::listing::VolumeLocation;
use crate::{ErrorCode, ServiceError, ServiceResult};

#[cfg(any(target_os = "macos", windows))]
use crate::process::{ProcessError, run_with_timeout};
#[cfg(any(target_os = "macos", windows))]
use std::process::Command;
#[cfg(any(target_os = "macos", windows))]
use std::time::Duration;

/// A drive flushing its last writes can take a while to let go.
#[cfg(any(target_os = "macos", windows))]
const EJECT_TIMEOUT: Duration = Duration::from_secs(60);

/// How the volume is named in messages: by name, like the sidebar, or by
/// its path when it has none.
fn display_name(volume: &VolumeLocation) -> &str {
    let name = volume.name.trim();
    if name.is_empty() {
        volume.path.as_str()
    } else {
        name
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn eject(volume: &VolumeLocation) -> ServiceResult<()> {
    // Finder ejects a disk with all its volumes; a network share has no disk,
    // so it is only unmounted.
    let verb = if is_local_volume(&volume.path) {
        "eject"
    } else {
        "unmount"
    };
    let output = run_with_timeout(
        Command::new("/usr/sbin/diskutil")
            .arg(verb)
            .arg(&volume.path),
        EJECT_TIMEOUT,
        64 * 1024,
        64 * 1024,
    );
    let output = match output {
        Ok(output) => output,
        Err(ProcessError::TimedOut) => {
            return Err(ServiceError::new(
                ErrorCode::Busy,
                format!("Ejecting “{}” took too long", display_name(volume)),
            ));
        }
        Err(ProcessError::Io(error)) => return Err(error.into()),
    };
    if output.status.success() {
        return Ok(());
    }
    let mut report = String::from_utf8_lossy(&output.stdout).into_owned();
    report.push('\n');
    report.push_str(&String::from_utf8_lossy(&output.stderr));
    Err(ServiceError::new(
        ErrorCode::Busy,
        eject_failure_message(display_name(volume), &report),
    ))
}

#[cfg(target_os = "macos")]
fn is_local_volume(path: &str) -> bool {
    let Ok(path) = std::ffi::CString::new(path) else {
        return true;
    };
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stats` is a valid out-pointer.
    if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return true;
    }
    // SAFETY: statfs succeeded, so it filled `stats`.
    let stats = unsafe { stats.assume_init() };
    stats.f_flags & libc::MNT_LOCAL as u32 != 0
}

/// diskutil names the process that refused to let go ("Unmount was dissented
/// by PID 601 (/usr/bin/vim)"); otherwise its last line says what failed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn eject_failure_message(name: &str, report: &str) -> String {
    let dissenter = report
        .lines()
        .find(|line| line.contains("dissented by PID"))
        .and_then(|line| {
            let start = line.find('(')? + 1;
            let end = line.rfind(')')?;
            let path = line.get(start..end)?;
            std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
    if let Some(process) = dissenter {
        return format!("“{name}” is in use by {process}. Quit it, then eject again.");
    }
    match report.lines().map(str::trim).rfind(|line| !line.is_empty()) {
        Some(detail) => format!("Couldn't eject “{name}”: {detail}"),
        None => format!("Couldn't eject “{name}”"),
    }
}

#[cfg(windows)]
pub(crate) fn eject(volume: &VolumeLocation) -> ServiceResult<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let drive = volume.path.trim_end_matches(['\\', '/']);
    let letter = drive
        .strip_suffix(':')
        .filter(|letter| letter.len() == 1 && letter.chars().all(|c| c.is_ascii_alphabetic()));
    if letter.is_none() {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            format!("“{}” is not a drive letter", volume.path),
        ));
    }
    // Explorer's own Eject verb on This PC's drive item: it safely removes a
    // USB drive and dismounts a disc image, and explains when it can't.
    let script = format!(
        "$drive = (New-Object -ComObject Shell.Application).Namespace(17).ParseName('{drive}\\'); \
         if ($null -eq $drive) {{ exit 2 }}; $drive.InvokeVerb('Eject')"
    );
    let output = run_with_timeout(
        Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .creation_flags(CREATE_NO_WINDOW),
        EJECT_TIMEOUT,
        0,
        64 * 1024,
    );
    match output {
        Ok(output) if output.status.success() => {}
        Ok(_) | Err(ProcessError::TimedOut) => {
            return Err(ServiceError::new(
                ErrorCode::Busy,
                format!("Windows couldn't eject “{}”", display_name(volume)),
            ));
        }
        Err(ProcessError::Io(error)) => return Err(error.into()),
    }
    // The verb returns before Windows lets go of the drive.
    let root = std::path::Path::new(&volume.path);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if std::fs::metadata(root).is_err() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(ServiceError::new(
        ErrorCode::Busy,
        format!(
            "“{}” is still in use. Close the files and windows that use it, then eject again.",
            display_name(volume)
        ),
    ))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn eject(volume: &VolumeLocation) -> ServiceResult<()> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        format!(
            "Ejecting “{}” is available on macOS and Windows",
            display_name(volume)
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_eject_names_the_process_holding_the_volume() {
        let report = "Unmount of disk13 failed: at least one volume could not be unmounted\n\
                      Unmount was dissented by PID 60703 (/bin/sleep)\n\
                      Dissenter parent PPID 60362 (/bin/zsh)\n";
        assert_eq!(
            eject_failure_message("Installer", report),
            "“Installer” is in use by sleep. Quit it, then eject again."
        );
        assert_eq!(
            eject_failure_message("Installer", "Unable to find disk\n\n"),
            "Couldn't eject “Installer”: Unable to find disk"
        );
        assert_eq!(
            eject_failure_message("Installer", ""),
            "Couldn't eject “Installer”"
        );
    }
}
