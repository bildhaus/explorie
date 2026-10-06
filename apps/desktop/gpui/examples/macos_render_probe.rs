#[cfg(target_os = "macos")]
use std::{error::Error, path::PathBuf, time::Duration};

#[cfg(target_os = "macos")]
use explorie_gpui::{ExplorieAssets, MACOS_SYSTEM_FONT_FAMILY};
#[cfg(target_os = "macos")]
use gpui::{
    App, AppContext, Context, Render, TextRun, Window, WindowBounds, WindowOptions, bounds, div,
    font, point, prelude::*, px, rgb, size,
};
#[cfg(target_os = "macos")]
struct RenderProbe;

#[cfg(target_os = "macos")]
const FONT_FAMILIES: [&str; 6] = [
    MACOS_SYSTEM_FONT_FAMILY,
    ".AppleSystemUIFont",
    "Helvetica",
    "Arial",
    "Menlo",
    "Georgia",
];

#[cfg(target_os = "macos")]
impl Render for RenderProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .bg(rgb(0x101010))
            .children(FONT_FAMILIES.iter().map(|family| {
                div()
                    .flex()
                    .h(px(36.0))
                    .items_center()
                    .font_family(*family)
                    .text_size(px(20.0))
                    .text_color(rgb(0xffffff))
                    .child(format!("Explorie text — {family}"))
            }))
    }
}

#[cfg(target_os = "macos")]
fn validate_image(image: image::RgbaImage, output: PathBuf) -> Result<(), String> {
    let visible_text_pixels = image
        .pixels()
        .filter(|pixel| pixel[0] > 80 && pixel[1] > 80 && pixel[2] > 80 && pixel[3] > 0)
        .count();
    let dark_background_pixels = image
        .pixels()
        .filter(|pixel| pixel[0] < 40 && pixel[1] < 40 && pixel[2] < 40 && pixel[3] > 0)
        .count();
    let total_pixels = image.width() as usize * image.height() as usize;
    let row_height = image.height() as usize / FONT_FAMILIES.len();
    let row_pixel_counts = FONT_FAMILIES
        .iter()
        .enumerate()
        .map(|(index, family)| {
            let start = index * row_height;
            let end = start + row_height;
            let count = image
                .rows()
                .skip(start)
                .take(end - start)
                .flatten()
                .filter(|pixel| pixel[0] > 80 && pixel[1] > 80 && pixel[2] > 80 && pixel[3] > 0)
                .count();
            (*family, count)
        })
        .collect::<Vec<_>>();

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    image.save(&output).map_err(|error| error.to_string())?;

    if dark_background_pixels < total_pixels * 3 / 4 {
        return Err(format!(
            "macOS Metal probe did not render the expected dark surface ({dark_background_pixels}/{total_pixels} pixels)"
        ));
    }
    if visible_text_pixels < 100 {
        return Err(format!(
            "macOS Metal probe rendered only {visible_text_pixels} visible text pixels; per-family counts: {row_pixel_counts:?}"
        ));
    }
    println!(
        "macOS Metal text probe passed with {visible_text_pixels} visible pixels ({row_pixel_counts:?}): {}",
        output.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::var_os("EXPLORIE_RENDER_PROBE_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/macos-render-probe.png"));

    gpui_platform::application()
        .with_assets(ExplorieAssets)
        .run(move |cx: &mut App| {
            let window = match cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds(
                        point(px(40.0), px(40.0)),
                        size(px(640.0), px(240.0)),
                    ))),
                    focus: false,
                    show: true,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| RenderProbe),
            ) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("failed to open native macOS probe window: {error}");
                    std::process::exit(1);
                }
            };
            cx.activate(true);
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let result = window
                    .update(cx, |_, window, _| {
                        let diagnostics = FONT_FAMILIES
                            .iter()
                            .map(|family| {
                                let text = format!("Explorie text — {family}");
                                let run = TextRun {
                                    len: text.len(),
                                    font: font(*family),
                                    color: rgb(0xffffff).into(),
                                    ..Default::default()
                                };
                                let line = window.text_system().shape_line(
                                    text.into(),
                                    px(20.0),
                                    &[run],
                                    None,
                                );
                                let glyph_count =
                                    line.runs.iter().map(|run| run.glyphs.len()).sum::<usize>();
                                let font_ids =
                                    line.runs.iter().map(|run| run.font_id).collect::<Vec<_>>();
                                (*family, line.width(), glyph_count, font_ids)
                            })
                            .collect::<Vec<_>>();
                        eprintln!("macOS text shaping diagnostics: {diagnostics:?}");
                        window.render_to_image()
                    })
                    .map_err(|error| error.to_string())
                    .and_then(|image| image.map_err(|error| error.to_string()))
                    .and_then(|image| validate_image(image, output));
                match result {
                    Ok(()) => {
                        cx.update(|cx| cx.quit());
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        std::process::exit(1);
                    }
                }
            })
            .detach();
        });

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
