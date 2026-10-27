//! Measures one image preview operation in isolation, for comparing peak
//! memory between decoders. Run it under `/usr/bin/time -l` (macOS) or
//! `/usr/bin/time -v` (Linux) and read the maximum resident set size:
//!
//! ```sh
//! cargo run --release -p explorie-native-services --example image_preview_memory -- \
//!     thumbnail ~/Pictures/huge.png 256
//! ```
//!
//! Modes: `thumbnail <image> [size]` (Grid thumbnail), `display <image>` (the
//! direct-image preview the inspector shows) and `artifact <image>` (the
//! generated preview for formats such as HEIC). Every run uses a fresh cache.

use std::path::PathBuf;
use std::time::Instant;

use explorie_native_services::{NativeServices, ResourcePaths};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let usage = "usage: image_preview_memory <thumbnail|display|artifact> <image> [size]";
    let mode = arguments.next().ok_or(usage)?;
    let source = PathBuf::from(arguments.next().ok_or(usage)?);
    let size = arguments
        .next()
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(256);

    let root = tempfile::tempdir()?;
    let previews = NativeServices::new(ResourcePaths::test(root.path())).previews;
    let started = Instant::now();
    let (output, tool) = match mode.as_str() {
        "thumbnail" => (
            previews
                .thumbnail(source, size)
                .wait()?
                .ok_or("no thumbnail for this file")?,
            None,
        ),
        "display" => (previews.display_image(source).wait()?, None),
        "artifact" => {
            let artifact = previews.artifact(source).wait()?;
            (artifact.path, Some(artifact.tool))
        }
        _ => return Err(usage.into()),
    };
    let elapsed = started.elapsed();
    let (width, height) = image::image_dimensions(&output)?;
    println!(
        "{mode}: {width}x{height} in {:.3}s{}",
        elapsed.as_secs_f64(),
        tool.map(|tool| format!(" via {tool}")).unwrap_or_default()
    );
    Ok(())
}
