use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use explorie_core::{FileEntry, list_dir_with_sizes};
use serde_json::json;
use sysinfo::System;

const VIRTUAL_WINDOW: usize = 64;
const SCROLL_SAMPLES: usize = 1_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = std::env::args()
        .nth(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(100_000);
    let mode = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "shared".to_string());
    let root = std::env::temp_dir().join(format!(
        "explorie-large-folder-profile-{}",
        uuid::Uuid::new_v4()
    ));
    let fixture = root.join("browse");
    fs::create_dir_all(&fixture)?;
    let _cleanup = Cleanup(root.clone());

    let create_started = Instant::now();
    let worker_count = std::thread::available_parallelism()
        .map_or(4, usize::from)
        .min(8);
    std::thread::scope(|scope| {
        for worker in 0..worker_count {
            let fixture = &fixture;
            scope.spawn(move || {
                for index in (worker..count).step_by(worker_count) {
                    let path = fixture.join(format!("entry-{index:07}.txt"));
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .expect("create large-folder fixture");
                }
            });
        }
    });
    let create_ms = create_started.elapsed().as_secs_f64() * 1_000.0;

    let memory_before = process_memory_bytes();
    let list_started = Instant::now();
    let entries = list_dir_with_sizes(&fixture, false)?;
    let list_ms = list_started.elapsed().as_secs_f64() * 1_000.0;
    let memory_after_list = process_memory_bytes();

    let (prepare_ms, memory_after_prepare, scroll_ms, checksum) = match mode.as_str() {
        "legacy" => profile_legacy(entries),
        "shared" => profile_shared(entries),
        _ => return Err("mode must be 'legacy' or 'shared'".into()),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "entries": count,
            "mode": mode,
            "fixtureCreateMs": round(create_ms),
            "nativeListMs": round(list_ms),
            "prepareAndSortMs": round(prepare_ms),
            "virtualScrollSamples": SCROLL_SAMPLES,
            "virtualWindowEntries": VIRTUAL_WINDOW,
            "virtualScrollMs": round(scroll_ms),
            "processMemoryBeforeBytes": memory_before,
            "processMemoryAfterListBytes": memory_after_list,
            "processMemoryAfterPrepareBytes": memory_after_prepare,
            "retainedMemoryDeltaBytes": memory_after_prepare.saturating_sub(memory_before),
            "checksum": checksum,
        }))?
    );
    Ok(())
}

fn profile_legacy(entries: Vec<FileEntry>) -> (f64, u64, f64, usize) {
    let prepare_started = Instant::now();
    let mut visible = entries.clone();
    visible.sort_by_cached_key(|entry| entry_name(entry).to_lowercase());
    let prepare_ms = prepare_started.elapsed().as_secs_f64() * 1_000.0;
    let memory = process_memory_bytes();
    let (scroll_ms, checksum) = profile_scroll(&visible, |entry| entry.clone());
    (prepare_ms, memory, scroll_ms, checksum)
}

fn profile_shared(entries: Vec<FileEntry>) -> (f64, u64, f64, usize) {
    let prepare_started = Instant::now();
    let mut entries = entries.into_iter().map(Arc::new).collect::<Vec<_>>();
    entries.sort_by_cached_key(|entry| entry_name(entry).to_lowercase());
    let visible = entries.clone();
    let prepare_ms = prepare_started.elapsed().as_secs_f64() * 1_000.0;
    let memory = process_memory_bytes();
    let (scroll_ms, checksum) = profile_scroll(&visible, Arc::clone);
    (prepare_ms, memory, scroll_ms, checksum)
}

fn profile_scroll<T>(entries: &[T], clone: impl Fn(&T) -> T) -> (f64, usize)
where
    T: Clone + EntryPath,
{
    let scroll_started = Instant::now();
    let mut checksum = 0usize;
    for sample in 0usize..SCROLL_SAMPLES {
        let max_start = entries.len().saturating_sub(VIRTUAL_WINDOW).max(1);
        let start = sample.wrapping_mul(997) % max_start;
        let window = entries[start..(start + VIRTUAL_WINDOW).min(entries.len())]
            .iter()
            .map(&clone)
            .collect::<Vec<_>>();
        checksum = checksum.wrapping_add(
            window
                .iter()
                .map(|entry| entry.entry_path().as_os_str().len())
                .sum(),
        );
    }
    (scroll_started.elapsed().as_secs_f64() * 1_000.0, checksum)
}

trait EntryPath {
    fn entry_path(&self) -> &PathBuf;
}

impl EntryPath for FileEntry {
    fn entry_path(&self) -> &PathBuf {
        &self.path
    }
}

impl EntryPath for Arc<FileEntry> {
    fn entry_path(&self) -> &PathBuf {
        &self.path
    }
}

fn entry_name(entry: &FileEntry) -> String {
    entry
        .path
        .file_name()
        .unwrap_or(entry.path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn process_memory_bytes() -> u64 {
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0;
    };
    let mut system = System::new();
    system.refresh_process(pid);
    system.process(pid).map_or(0, sysinfo::Process::memory)
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        let target = self.0.as_path();
        let safe_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("explorie-large-folder-profile-"));
        if safe_name && target.starts_with(&temp) {
            let _ = fs::remove_dir_all(target);
        }
    }
}
