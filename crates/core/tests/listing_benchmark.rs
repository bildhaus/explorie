use explorie_core::list_dir;
use std::fs::File;
use std::time::{Duration, Instant};

const TEN_THOUSAND: usize = 10_000;
const ONE_HUNDRED_THOUSAND: usize = 100_000;

fn create_files(count: usize) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("create benchmark directory");
    for index in 0..count {
        File::create(directory.path().join(format!("file-{index:06}.txt")))
            .expect("create benchmark file");
    }
    directory
}

fn measure_listing(count: usize) -> (Duration, Duration, Duration, usize) {
    let directory = create_files(count);

    let listing_started = Instant::now();
    let entries = list_dir(directory.path()).expect("list benchmark directory");
    let listing_elapsed = listing_started.elapsed();
    assert_eq!(entries.len(), count);

    let probe_started = Instant::now();
    let probe_count = std::fs::read_dir(directory.path())
        .expect("read benchmark directory")
        .count();
    let probe_elapsed = probe_started.elapsed();
    assert_eq!(probe_count, count);

    let serialization_started = Instant::now();
    let payload = serde_json::to_vec(&entries).expect("serialize benchmark listing");
    let serialization_elapsed = serialization_started.elapsed();

    (
        probe_elapsed,
        listing_elapsed,
        serialization_elapsed,
        payload.len(),
    )
}

#[test]
fn lists_10k_files_within_the_regression_budget() {
    let directory = create_files(TEN_THOUSAND);
    let started = Instant::now();
    let entries = list_dir(directory.path()).expect("list benchmark directory");
    let elapsed = started.elapsed();

    assert_eq!(entries.len(), TEN_THOUSAND);
    assert!(
        elapsed < Duration::from_secs(5),
        "listing 10,000 files took {elapsed:.2?}, exceeding the 5s regression budget"
    );
}

#[test]
#[ignore = "creates 100,000 files; run explicitly for listing benchmarks"]
fn records_10k_and_100k_listing_baselines() {
    for (count, budget) in [
        (TEN_THOUSAND, Duration::from_secs(5)),
        (ONE_HUNDRED_THOUSAND, Duration::from_secs(10)),
    ] {
        let (probe, listing, serialization, payload_bytes) = measure_listing(count);
        eprintln!(
            "{count:>7} entries | read_dir {probe:.2?} | list_dir {listing:.2?} | serde_json {serialization:.2?} | {payload_bytes} bytes"
        );
        assert!(
            listing < budget,
            "listing {count} files took {listing:.2?}, exceeding the {budget:.0?} benchmark budget"
        );
    }
}
