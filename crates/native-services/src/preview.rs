use crate::model_preview::{ModelCamera, ModelPreview, ModelPreviewCache};
use crate::rich_preview::{self, RichPreview};
use crate::{
    BlockingTask, ErrorCode, HelperStatusEvent, ServiceContext, ServiceError, ServiceEvent,
    ServiceResult,
};
use crate::{ImageMetadata, image_metadata};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use syntect::easy::ScopeRegionIterator;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, test))]
mod windows_icon;

const MAX_TEXT_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT_HIGHLIGHTS: usize = 50_000;
#[cfg(any(windows, target_os = "macos"))]
const MAX_ICON_CACHE_ENTRIES: usize = 256;
#[cfg(any(windows, target_os = "macos"))]
const MAX_ICON_CACHE_BYTES: u64 = 32 * 1024 * 1024;
/// Edge of cached system icons: the largest Grid icon (60 points) on a 2x
/// display, so one rendering serves every listing view.
#[cfg(any(windows, target_os = "macos"))]
const SYSTEM_ICON_PIXELS: u32 = 128;
#[cfg(target_os = "macos")]
const SYSTEM_ICON_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const QUICKLOOK_THUMBNAIL_TIMEOUT: Duration = Duration::from_secs(5);
/// Office documents can take Quick Look several seconds at preview size.
#[cfg(target_os = "macos")]
const QUICKLOOK_PREVIEW_TIMEOUT: Duration = Duration::from_secs(15);
/// Edges Quick Look is asked for at preview size, largest first. It refuses
/// some large photos (a 6016 px HEIC fails with "Could not generate a
/// thumbnail" from about 2000 px) that it renders fine a little smaller.
#[cfg(target_os = "macos")]
const QUICKLOOK_PREVIEW_DIMENSIONS: [u32; 3] = [IMAGE_PREVIEW_DIMENSION, 1_600, 1_024];
/// ImageIO decodes in process; a 36-megapixel HEIC takes about 0.3 seconds at
/// preview size, a camera RAW somewhat longer.
#[cfg(target_os = "macos")]
const IMAGEIO_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest source ImageIO may decode, matching the ImageMagick area limit.
/// ImageIO downsamples while it decodes, so even this stays far below the
/// memory a full-resolution decode of a smaller image takes.
#[cfg(target_os = "macos")]
const MAX_IMAGEIO_SOURCE_PIXELS: u64 = 128_000_000;
/// Images with more pixels than this go to ImageIO before the bundled
/// decoders on macOS. Every thumbnail and preview of them is downsampled, and
/// ImageIO decodes JPEG and HEIC at the reduced size and streams PNG instead of
/// holding the full image (an 8000 px PNG peaks near 200 MB through the bundled
/// decoder). Smaller images keep the bundled decoder's output.
const IMAGEIO_MIN_SOURCE_PIXELS: u64 = DISPLAY_IMAGE_MAX_PIXELS;
const IMAGEIO_TOOL: &str = "macOS ImageIO";
const MAX_THUMBNAIL_ENTRIES: usize = 256;
const MAX_THUMBNAIL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PDF_CACHE_ENTRIES: usize = 48;
const MAX_PDF_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const PDF_RENDER_SCALE: f32 = 1.5;
const MAX_IMAGE_PREVIEW_BYTES: u64 = 256 * 1024 * 1024;
const MAX_IMAGE_DECODE_DIMENSION: u32 = 32_768;
const MAX_IMAGE_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const IMAGE_PREVIEW_DIMENSION: u32 = 2_048;
const MAX_SVG_BYTES: u64 = 16 * 1024 * 1024;
const DETECTION_BYTES: u64 = 8 * 1024;
const HEX_PREVIEW_BYTES: usize = 16 * 12;
const HELPER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const HELPER_CONVERSION_TIMEOUT: Duration = Duration::from_secs(45);
const FFMPEG_INPUT_PROTOCOLS: &str = "file,pipe";
const IMAGEMAGICK_POLICY: &str = include_str!("../assets/imagemagick-policy.xml");
const IMAGEMAGICK_LIMITS: [(&str, &str); 7] = [
    ("memory", "256MiB"),
    ("map", "512MiB"),
    ("disk", "1GiB"),
    ("area", "128MP"),
    ("width", "16KP"),
    ("height", "16KP"),
    ("time", "60"),
];
const LIBREOFFICE_REGISTRY: &str = include_str!("../assets/libreoffice-registrymodifications.xcu");
#[cfg(windows)]
const SHELL_ICON_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HELPER_STDOUT_BYTES: usize = 64 * 1024;
const MAX_GENERATED_ARTIFACT_ENTRIES: usize = 96;
const MAX_GENERATED_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PDF_PIXEL_DIMENSION: u32 = 16_384;
const MAX_PDF_PIXEL_COUNT: u64 = 100_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectedPreviewKind {
    Text,
    Image,
    Svg,
    Audio,
    Video,
    Pdf,
    Archive,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDetection {
    pub kind: DetectedPreviewKind,
    pub description: String,
    pub mime_type: Option<String>,
    pub byte_sample: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPreview {
    pub text: String,
    pub truncated: bool,
    pub language: Option<String>,
    #[serde(default = "default_text_encoding")]
    pub encoding: String,
    pub wrapped: bool,
    pub highlights: Vec<TextHighlight>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextHighlightKind {
    Comment,
    String,
    Number,
    Keyword,
    Function,
    Type,
    Variable,
    Constant,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextHighlight {
    pub start: usize,
    pub end: usize,
    pub kind: TextHighlightKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PreviewArtifact {
    pub kind: String,
    pub path: PathBuf,
    pub mime_type: String,
    pub tool: String,
}

/// One locally rasterized PDF page. The file path points into the injected
/// preview cache; source bytes never cross the native UI boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPagePreview {
    pub source_path: PathBuf,
    pub image_path: PathBuf,
    pub page_index: usize,
    pub page_count: usize,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: u32,
    pub display_height: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperStatus {
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub extensions: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct PreviewService {
    context: ServiceContext,
    cache_gate: Arc<RwLock<()>>,
    icon_lock: Arc<Mutex<()>>,
    thumbnail_locks: Arc<[Mutex<()>; 4]>,
    artifact_lock: Arc<Mutex<()>>,
    pdf_lock: Arc<Mutex<()>>,
    helper_generation: Arc<AtomicU64>,
    model_cache: ModelPreviewCache,
}

impl PreviewService {
    pub(crate) fn new(context: ServiceContext) -> Self {
        Self {
            context,
            cache_gate: Arc::new(RwLock::new(())),
            icon_lock: Arc::new(Mutex::new(())),
            thumbnail_locks: Arc::new(std::array::from_fn(|_| Mutex::new(()))),
            artifact_lock: Arc::new(Mutex::new(())),
            pdf_lock: Arc::new(Mutex::new(())),
            helper_generation: Arc::new(AtomicU64::new(0)),
            model_cache: ModelPreviewCache::default(),
        }
    }

    /// Share cache coordination across the process, but keep helper-preview
    /// cancellation private to one window.
    pub fn fork_cancellation_scope(&self) -> Self {
        Self {
            context: self.context.clone(),
            cache_gate: Arc::clone(&self.cache_gate),
            icon_lock: Arc::clone(&self.icon_lock),
            thumbnail_locks: Arc::clone(&self.thumbnail_locks),
            artifact_lock: Arc::clone(&self.artifact_lock),
            pdf_lock: Arc::clone(&self.pdf_lock),
            helper_generation: Arc::new(AtomicU64::new(0)),
            model_cache: self.model_cache.clone(),
        }
    }

    pub fn read_text(&self, path: PathBuf, max_bytes: u64) -> BlockingTask<TextPreview> {
        self.context
            .spawn_blocking(move || read_text_preview(&path, max_bytes))
    }

    pub fn detect(&self, path: PathBuf) -> BlockingTask<PreviewDetection> {
        self.context
            .spawn_blocking(move || detect_preview_content(&path))
    }

    pub fn helper_status(&self) -> BlockingTask<Vec<HelperStatus>> {
        let context = self.context.clone();
        self.context.spawn_blocking(move || {
            let statuses = helper_statuses();
            for status in &statuses {
                context
                    .events()
                    .publish(ServiceEvent::HelperStatus(HelperStatusEvent {
                        helper: status.name.clone(),
                        status: status.clone(),
                    }));
            }
            Ok(statuses)
        })
    }

    pub fn clear_cache(&self) -> BlockingTask<()> {
        self.cancel_artifact();
        let directory = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        self.context.spawn_blocking(move || {
            let _guard = cache_gate
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if directory.exists() {
                fs::remove_dir_all(&directory).map_err(ServiceError::from)?;
            }
            fs::create_dir_all(&directory).map_err(ServiceError::from)
        })
    }

    pub fn file_icon(&self, path: PathBuf) -> BlockingTask<Option<PathBuf>> {
        let cache = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        let icon_lock = Arc::clone(&self.icon_lock);
        self.context.spawn_blocking(move || {
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _icon_guard = icon_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            get_file_icon(&path, &cache)
        })
    }

    pub fn thumbnail(&self, path: PathBuf, max_size: u32) -> BlockingTask<Option<PathBuf>> {
        let cache = self.cache_dir();
        let model_cache = self.model_cache.clone();
        let cache_gate = Arc::clone(&self.cache_gate);
        let thumbnail_locks = Arc::clone(&self.thumbnail_locks);
        let lane = thumbnail_lock_index(&path, max_size);
        self.context.spawn_blocking(move || {
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _thumbnail_guard = thumbnail_locks[lane]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Without `preview-3d`, models take the generic path, which has no
            // thumbnailer for them, so they keep their file icon.
            if cfg!(feature = "preview-3d") && is_model_preview_path(&path) {
                get_model_thumbnail(&path, max_size, &cache, &model_cache)
            } else {
                get_file_thumbnail(&path, max_size, &cache)
            }
        })
    }

    pub fn artifact(&self, path: PathBuf) -> BlockingTask<PreviewArtifact> {
        let cache = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        let artifact_lock = Arc::clone(&self.artifact_lock);
        let helper_generation = Arc::clone(&self.helper_generation);
        let ticket = helper_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.context.spawn_blocking(move || {
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _artifact_guard = artifact_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            generate_preview_artifact(&path, &cache, &helper_generation, ticket)
        })
    }

    pub fn pdf_page(&self, path: PathBuf, page_index: usize) -> BlockingTask<PdfPagePreview> {
        let cache = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        let pdf_lock = Arc::clone(&self.pdf_lock);
        self.context.spawn_blocking(move || {
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _pdf_guard = pdf_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            render_pdf_page(&path, page_index, &cache)
        })
    }

    pub fn image_metadata(&self, path: PathBuf) -> BlockingTask<ImageMetadata> {
        self.context
            .spawn_blocking(move || image_metadata::load_image_metadata(&path))
    }

    pub fn model(
        &self,
        path: PathBuf,
        camera: ModelCamera,
        width: u32,
        height: u32,
    ) -> BlockingTask<ModelPreview> {
        let cache = self.model_cache.clone();
        self.context
            .spawn_blocking(move || cache.render(&path, camera, width, height))
    }

    pub fn rich(&self, path: PathBuf) -> BlockingTask<RichPreview> {
        let cache = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        let artifact_lock = Arc::clone(&self.artifact_lock);
        self.context.spawn_blocking(move || {
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _artifact_guard = artifact_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rich_preview::preview(&path, &cache)
        })
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.context.resources().cache_dir.join("preview")
    }

    pub fn cancel_artifact(&self) {
        self.helper_generation.fetch_add(1, Ordering::AcqRel);
    }
}

fn thumbnail_lock_index(path: &Path, max_size: u32) -> usize {
    path.as_os_str()
        .to_string_lossy()
        .bytes()
        .fold(max_size as usize, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(usize::from(byte))
        })
        % 4
}

fn read_text_preview(path: &Path, max_bytes: u64) -> ServiceResult<TextPreview> {
    let limit = max_bytes.min(MAX_TEXT_PREVIEW_BYTES);
    let mut bytes = Vec::with_capacity(limit.min(128 * 1024) as usize);
    File::open(path)
        .map_err(ServiceError::from)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ServiceError::from)?;
    let truncated = bytes.len() as u64 > limit;
    bytes.truncate(limit as usize);
    let (text, encoding) = decode_preview_text(&bytes);
    let text = normalize_preview_line_endings(text);
    let (language, highlights) = highlight_text(path, &text);
    Ok(TextPreview {
        text,
        truncated,
        language,
        encoding: encoding.to_string(),
        wrapped: matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "md" | "markdown" | "rst" | "adoc" | "asciidoc" | "org"
        ),
        highlights,
    })
}

fn normalize_preview_line_endings(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

fn default_text_encoding() -> String {
    "UTF-8".to_string()
}

fn decode_preview_text(bytes: &[u8]) -> (String, &'static str) {
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let words = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect::<Vec<_>>();
        return (String::from_utf16_lossy(&words), "UTF-16 LE");
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let words = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        return (String::from_utf16_lossy(&words), "UTF-16 BE");
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return (String::from_utf8_lossy(bytes).into_owned(), "UTF-8 BOM");
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return (text.to_string(), "UTF-8");
    }
    (String::from_utf8_lossy(bytes).into_owned(), "UTF-8 lossy")
}

fn detect_preview_content(path: &Path) -> ServiceResult<PreviewDetection> {
    let metadata = fs::metadata(path).map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Preview detection requires a regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(DETECTION_BYTES as usize);
    File::open(path)
        .map_err(ServiceError::from)?
        .take(DETECTION_BYTES)
        .read_to_end(&mut bytes)
        .map_err(ServiceError::from)?;
    let extension = extension(path);

    let detected = if bytes.starts_with(b"%PDF-") {
        (DetectedPreviewKind::Pdf, "PDF document", "application/pdf")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        (DetectedPreviewKind::Image, "PNG image", "image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        (DetectedPreviewKind::Image, "JPEG image", "image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        (DetectedPreviewKind::Image, "GIF image", "image/gif")
    } else if bytes.starts_with(b"BM") {
        (DetectedPreviewKind::Image, "BMP image", "image/bmp")
    } else if bytes.starts_with(b"qoif") {
        (DetectedPreviewKind::Image, "QOI image", "image/qoi")
    } else if bytes.starts_with(b"8BPS\0\x01") {
        (
            DetectedPreviewKind::Image,
            "Photoshop document",
            "image/vnd.adobe.photoshop",
        )
    } else if bytes.starts_with(&[0xff, 0x0a])
        || bytes.starts_with(&[
            0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
        ])
    {
        (DetectedPreviewKind::Image, "JPEG XL image", "image/jxl")
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        (DetectedPreviewKind::Image, "TIFF image", "image/tiff")
    } else if bytes.starts_with(&[0, 0, 1, 0]) {
        (DetectedPreviewKind::Image, "Icon image", "image/x-icon")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        (DetectedPreviewKind::Image, "WebP image", "image/webp")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        (DetectedPreviewKind::Audio, "WAVE audio", "audio/wav")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"AVI " {
        (DetectedPreviewKind::Video, "AVI video", "video/x-msvideo")
    } else if bytes.len() >= 12
        && &bytes[..4] == b"FORM"
        && matches!(&bytes[8..12], b"AIFF" | b"AIFC")
    {
        (DetectedPreviewKind::Audio, "AIFF audio", "audio/aiff")
    } else if bytes.starts_with(b"caff") {
        (DetectedPreviewKind::Audio, "Core Audio file", "audio/x-caf")
    } else if bytes.starts_with(b"fLaC") {
        (DetectedPreviewKind::Audio, "FLAC audio", "audio/flac")
    } else if bytes.starts_with(b"OggS") {
        if extension == "ogv" {
            (DetectedPreviewKind::Video, "Ogg video", "video/ogg")
        } else {
            (DetectedPreviewKind::Audio, "Ogg audio", "audio/ogg")
        }
    } else if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        (DetectedPreviewKind::Text, "Text document", "text/plain")
    } else if bytes.starts_with(b"ID3")
        || bytes
            .windows(2)
            .next()
            .is_some_and(|pair| pair[0] == 0xff && pair[1] & 0xe0 == 0xe0)
    {
        (DetectedPreviewKind::Audio, "MPEG audio", "audio/mpeg")
    } else if bytes.starts_with(b"\x1a\x45\xdf\xa3") {
        (
            DetectedPreviewKind::Video,
            "Matroska/WebM video",
            "video/x-matroska",
        )
    } else if bytes.starts_with(b"FLV") {
        (DetectedPreviewKind::Video, "Flash video", "video/x-flv")
    } else if bytes.starts_with(&[0, 0, 1, 0xba]) {
        (DetectedPreviewKind::Video, "MPEG video", "video/mpeg")
    } else if let Some((description, mime)) = detect_iso_media(&bytes, &extension) {
        let kind = if mime.starts_with("image/") {
            DetectedPreviewKind::Image
        } else if mime.starts_with("audio/") {
            DetectedPreviewKind::Audio
        } else {
            DetectedPreviewKind::Video
        };
        (kind, description, mime)
    } else if looks_like_svg(&bytes) {
        (DetectedPreviewKind::Svg, "SVG image", "image/svg+xml")
    } else if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        (
            DetectedPreviewKind::Archive,
            "ZIP archive",
            "application/zip",
        )
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        (
            DetectedPreviewKind::Archive,
            "Gzip archive",
            "application/gzip",
        )
    } else if bytes.starts_with(b"7z\xbc\xaf\x27\x1c") {
        (
            DetectedPreviewKind::Archive,
            "7-Zip archive",
            "application/x-7z-compressed",
        )
    } else if bytes.starts_with(b"Rar!\x1a\x07") {
        (
            DetectedPreviewKind::Archive,
            "RAR archive",
            "application/vnd.rar",
        )
    } else if bytes
        .get(257..262)
        .is_some_and(|signature| signature == b"ustar")
    {
        (
            DetectedPreviewKind::Archive,
            "TAR archive",
            "application/x-tar",
        )
    } else if explorie_core::archive::is_archive(path) {
        (
            DetectedPreviewKind::Archive,
            "Archive",
            "application/octet-stream",
        )
    } else if looks_like_text(&bytes) {
        (DetectedPreviewKind::Text, "Text document", "text/plain")
    } else {
        return Ok(PreviewDetection {
            kind: DetectedPreviewKind::Unknown,
            description: "Unknown binary file".to_string(),
            mime_type: Some("application/octet-stream".to_string()),
            byte_sample: Some(hex_preview(&bytes)),
        });
    };

    Ok(PreviewDetection {
        kind: detected.0,
        description: detected.1.to_string(),
        mime_type: Some(detected.2.to_string()),
        byte_sample: None,
    })
}

fn detect_iso_media(bytes: &[u8], extension: &str) -> Option<(&'static str, &'static str)> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let brands = &bytes[8..bytes.len().min(64)];
    if brands
        .windows(4)
        .any(|brand| matches!(brand, b"avif" | b"avis"))
    {
        return Some(("AVIF image", "image/avif"));
    }
    if brands
        .windows(4)
        .any(|brand| matches!(brand, b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1"))
    {
        return Some(("HEIF image", "image/heif"));
    }
    if matches!(extension, "m4a" | "m4b" | "aac" | "alac")
        || brands.windows(4).any(|brand| brand == b"M4A ")
    {
        Some(("MPEG-4 audio", "audio/mp4"))
    } else {
        Some(("MPEG-4 video", "video/mp4"))
    }
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg")
        || (text.starts_with("<?xml")
            && text
                .get(..text.len().min(2048))
                .is_some_and(|head| head.contains("<svg")))
}

fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        return true;
    }
    let Ok(text) = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
    else {
        return false;
    };
    let control_count = text
        .chars()
        .filter(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        .count();
    control_count.saturating_mul(100) <= text.chars().count().max(1).saturating_mul(2)
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .get(..bytes.len().min(HEX_PREVIEW_BYTES))
        .unwrap_or(bytes)
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            format!("{:08X}  {:<47}  |{}|", row * 16, hex, ascii)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn highlight_text(path: &Path, text: &str) -> (Option<String>, Vec<TextHighlight>) {
    let mut extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension.is_empty() {
        extension = match path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "dockerfile" => "dockerfile",
            "makefile" => "makefile",
            "cmakelists.txt" => "cmake",
            ".gitignore" | ".gitattributes" => "git",
            _ => "txt",
        }
        .to_string();
    }
    if matches!(extension.as_str(), "txt" | "log" | "csv") {
        return (None, Vec::new());
    }

    let syntaxes = syntax_set();
    let Some(syntax) = find_syntax(syntaxes, &extension) else {
        return (None, Vec::new());
    };
    if syntax.name == "Plain Text" {
        return (None, Vec::new());
    }
    (
        Some(syntax.name.clone()),
        highlight_with_syntax(syntaxes, syntax, text),
    )
}

/// Syntax highlights for a Markdown code block whose info string names
/// `language`, either as an extension ("rs") or a name ("rust").
pub(crate) fn highlight_code(language: &str, text: &str) -> Vec<TextHighlight> {
    let language = language.trim().to_ascii_lowercase();
    if language.is_empty() {
        return Vec::new();
    }
    let syntaxes = syntax_set();
    match find_syntax(syntaxes, &language).or_else(|| syntaxes.find_syntax_by_token(&language)) {
        Some(syntax) if syntax.name != "Plain Text" => {
            highlight_with_syntax(syntaxes, syntax, text)
        }
        _ => Vec::new(),
    }
}

fn highlight_with_syntax(
    syntaxes: &SyntaxSet,
    syntax: &SyntaxReference,
    text: &str,
) -> Vec<TextHighlight> {
    let mut parser = ParseState::new(syntax);
    let mut scopes = ScopeStack::new();
    let mut highlights = Vec::<TextHighlight>::new();
    let mut offset = 0usize;

    for line in LinesWithEndings::from(text) {
        let Ok(operations) = parser.parse_line(line, syntaxes) else {
            break;
        };
        for (region, operation) in ScopeRegionIterator::new(&operations, line) {
            if scopes.apply(operation).is_err() {
                return highlights;
            }
            let start = offset;
            offset = offset.saturating_add(region.len());
            let Some(kind) = highlight_kind(&scopes) else {
                continue;
            };
            if let Some(previous) = highlights.last_mut()
                && previous.end == start
                && previous.kind == kind
            {
                previous.end = offset;
            } else if highlights.len() < MAX_TEXT_HIGHLIGHTS {
                highlights.push(TextHighlight {
                    start,
                    end: offset,
                    kind,
                });
            } else {
                return highlights;
            }
        }
    }

    highlights
}

fn find_syntax<'a>(syntaxes: &'a SyntaxSet, extension: &str) -> Option<&'a SyntaxReference> {
    syntaxes.find_syntax_by_extension(extension).or_else(|| {
        let aliases: &[&str] = match extension {
            "json5" | "jsonc" => &["json"],
            "tsx" => &["ts", "js"],
            "jsx" | "mjs" | "cjs" => &["js"],
            "pyw" => &["py"],
            "csharp" => &["cs"],
            "htm" => &["html"],
            "scss" | "sass" => &["css"],
            "cxx" | "cc" | "hpp" | "hh" => &["cpp"],
            "kts" => &["kt"],
            "bash" | "zsh" => &["sh"],
            "toml" | "lock" => &["ini"],
            "psm1" => &["ps1"],
            "vue" | "svelte" | "astro" | "xsl" | "xslt" => &["html"],
            "ndjson" | "jsonl" => &["json"],
            "tf" | "tfvars" | "hcl" => &["terraform"],
            "adoc" | "asciidoc" | "rst" | "org" => &["md"],
            "dockerfile" => &["dockerfile", "sh"],
            "makefile" => &["makefile", "sh"],
            "cmake" => &["cmake"],
            _ => &[],
        };
        aliases
            .iter()
            .find_map(|alias| syntaxes.find_syntax_by_extension(alias))
    })
}

fn highlight_kind(scopes: &ScopeStack) -> Option<TextHighlightKind> {
    let scopes = scopes
        .as_slice()
        .iter()
        .map(|scope| scope.build_string())
        .collect::<Vec<_>>()
        .join(" ");
    if scopes.contains("invalid") {
        Some(TextHighlightKind::Invalid)
    } else if scopes.contains("comment") {
        Some(TextHighlightKind::Comment)
    } else if scopes.contains("string") {
        Some(TextHighlightKind::String)
    } else if scopes.contains("constant.numeric") {
        Some(TextHighlightKind::Number)
    } else if scopes.contains("keyword")
        || scopes.contains("storage.modifier")
        || scopes.contains("meta.preprocessor")
    {
        Some(TextHighlightKind::Keyword)
    } else if scopes.contains("entity.name.function") || scopes.contains("support.function") {
        Some(TextHighlightKind::Function)
    } else if scopes.contains("entity.name.type")
        || scopes.contains("entity.name.tag")
        || scopes.contains("support.type")
        || scopes.contains("storage.type")
    {
        Some(TextHighlightKind::Type)
    } else if scopes.contains("variable") {
        Some(TextHighlightKind::Variable)
    } else if scopes.contains("constant") {
        Some(TextHighlightKind::Constant)
    } else {
        None
    }
}

fn helper_statuses() -> Vec<HelperStatus> {
    [
        (
            "FFmpeg",
            &["ffmpeg"][..],
            "-version",
            vec![
                "mov", "avi", "mkv", "wmv", "mp4", "webm", "flv", "m2ts", "mts", "mpeg", "mpg",
                "3gp", "ogv", "ts", "vob",
            ],
        ),
        (
            "LibreOffice",
            &["soffice", "libreoffice"][..],
            "--version",
            vec![
                "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
            ],
        ),
        (
            "ImageMagick",
            &["magick"][..],
            "--version",
            vec![
                "heic", "heif", "avif", "jxl", "jpegxl", "psd", "dng", "cr2", "cr3", "nef", "arw",
                "orf", "rw2", "raf",
            ],
        ),
    ]
    .into_iter()
    .map(|(name, candidates, version_arg, extensions)| {
        let found = candidates.iter().find_map(|candidate| {
            let output = run_helper_with_timeout(
                command(candidate).arg(version_arg),
                HELPER_PROBE_TIMEOUT,
                true,
            )
            .ok()?;
            output.status.success().then(|| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
        });
        match found {
            Some(version) => HelperStatus {
                name: name.to_string(),
                available: true,
                version: (!version.is_empty()).then_some(version),
                extensions: extensions.into_iter().map(String::from).collect(),
                error: None,
            },
            None => HelperStatus {
                name: name.to_string(),
                available: false,
                version: None,
                extensions: extensions.into_iter().map(String::from).collect(),
                error: Some(format!("{name} is unavailable")),
            },
        }
    })
    .collect()
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn is_external_document(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "rtf"
    )
}

fn is_external_video(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "mov"
            | "avi"
            | "mkv"
            | "wmv"
            | "flv"
            | "m2ts"
            | "mts"
            | "mpeg"
            | "mpg"
            | "3gp"
            | "ogv"
            | "ts"
            | "vob"
    )
}

fn is_external_image(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "heic"
            | "heif"
            | "avif"
            | "jxl"
            | "jpegxl"
            | "psd"
            | "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "orf"
            | "rw2"
            | "raf"
    )
}

fn is_svg_image(path: &Path) -> bool {
    matches!(extension(path).as_str(), "svg" | "svgz")
}

fn is_native_image(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "tif"
            | "tiff"
            | "ico"
            | "tga"
            | "dds"
            | "hdr"
            | "pnm"
            | "pbm"
            | "pgm"
            | "ppm"
            | "pam"
            | "qoi"
    )
}

fn cache_key(path: &Path, suffix: &str) -> String {
    let metadata = fs::metadata(path).ok();
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    if let Some(metadata) = metadata {
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_secs().to_le_bytes());
            digest.update(duration.subsec_nanos().to_le_bytes());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            digest.update(metadata.dev().to_le_bytes());
            digest.update(metadata.ino().to_le_bytes());
            digest.update(metadata.ctime().to_le_bytes());
            digest.update(metadata.ctime_nsec().to_le_bytes());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;

            digest.update(metadata.creation_time().to_le_bytes());
            digest.update(metadata.last_write_time().to_le_bytes());
            update_windows_file_identity(&mut digest, path);
        }
    }
    format!("{}-{suffix}", hex_digest(digest.finalize()))
}

#[cfg(windows)]
fn update_windows_file_identity(digest: &mut Sha256, path: &Path) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let Ok(file) = File::open(path) else {
        return;
    };
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let loaded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as _, &mut information as *mut _)
    };
    if loaded != 0 {
        digest.update(information.dwVolumeSerialNumber.to_le_bytes());
        digest.update(information.nFileIndexHigh.to_le_bytes());
        digest.update(information.nFileIndexLow.to_le_bytes());
    }
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cache_output(cache: &Path, path: &Path, suffix: &str, extension: &str) -> PathBuf {
    cache.join(format!("{}-{suffix}.{extension}", cache_key(path, suffix)))
}

fn render_pdf_page(path: &Path, page_index: usize, cache: &Path) -> ServiceResult<PdfPagePreview> {
    let metadata = fs::metadata(path).map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "PDF preview requires a regular file",
        ));
    }
    let mut signature = [0_u8; 5];
    let is_pdf = File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok()
        && &signature == b"%PDF-";
    if extension(path) != "pdf" && !is_pdf {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Native document rendering only accepts PDF content",
        ));
    }
    if metadata.len() > MAX_PDF_BYTES {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "PDF preview is limited to files no larger than 256 MiB",
        ));
    }

    let syntax =
        hayro_syntax::Pdf::new(fs::read(path).map_err(ServiceError::from)?).map_err(|error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("PDF cannot be inspected safely: {error:?}"),
            )
        })?;
    let page_count = syntax.pages().len();
    if page_count == 0 {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "PDF contains no pages",
        ));
    }
    if page_index >= page_count {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            format!(
                "PDF page {} is outside this {page_count}-page document",
                page_index + 1
            ),
        ));
    }
    let (page_width, page_height) = syntax.pages()[page_index].render_dimensions();
    let expected_width = (page_width * PDF_RENDER_SCALE).ceil();
    let expected_height = (page_height * PDF_RENDER_SCALE).ceil();
    if !expected_width.is_finite()
        || !expected_height.is_finite()
        || expected_width <= 0.0
        || expected_height <= 0.0
        || expected_width > MAX_PDF_PIXEL_DIMENSION as f32
        || expected_height > MAX_PDF_PIXEL_DIMENSION as f32
        || (expected_width as u64).saturating_mul(expected_height as u64) > MAX_PDF_PIXEL_COUNT
    {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "PDF page dimensions exceed the native preview safety limit",
        ));
    }
    drop(syntax);
    let bytes = fs::read(path).map_err(ServiceError::from)?;
    let document = karet_pdf::Document::load(bytes).map_err(map_pdf_error)?;

    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let suffix = format!("pdf-page-{}-150", page_index + 1);
    let image_path = cache.join(format!("{}.png", cache_key(path, &suffix)));
    let (pixel_width, pixel_height) = if let Some(dimensions) = pdf_png_dimensions(&image_path) {
        validate_pdf_pixel_dimensions(dimensions.0, dimensions.1)?;
        dimensions
    } else {
        let rendered = document
            .render_page(page_index, PDF_RENDER_SCALE)
            .map_err(map_pdf_error)?;
        let dimensions = (rendered.width(), rendered.height());
        validate_pdf_pixel_dimensions(dimensions.0, dimensions.1)?;
        write_pdf_png_atomically(&image_path, &rendered)?;
        prune_pdf_cache(cache);
        dimensions
    };
    Ok(PdfPagePreview {
        source_path: path.to_path_buf(),
        image_path,
        page_index,
        page_count,
        pixel_width,
        pixel_height,
        display_width: ((pixel_width as f32) / PDF_RENDER_SCALE).round() as u32,
        display_height: ((pixel_height as f32) / PDF_RENDER_SCALE).round() as u32,
    })
}

fn validate_pdf_pixel_dimensions(width: u32, height: u32) -> ServiceResult<()> {
    if width == 0
        || height == 0
        || width > MAX_PDF_PIXEL_DIMENSION
        || height > MAX_PDF_PIXEL_DIMENSION
        || u64::from(width).saturating_mul(u64::from(height)) > MAX_PDF_PIXEL_COUNT
    {
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "PDF page dimensions exceed the native preview safety limit",
        ))
    } else {
        Ok(())
    }
}

fn pdf_png_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = File::open(path).ok()?;
    let reader = png::Decoder::new(BufReader::new(file)).read_info().ok()?;
    let info = reader.info();
    (info.width > 0 && info.height > 0).then_some((info.width, info.height))
}

fn map_pdf_error(error: karet_pdf::PdfError) -> ServiceError {
    match error {
        karet_pdf::PdfError::Encrypted => ServiceError::new(
            ErrorCode::Unsupported,
            "Password-protected PDFs cannot be previewed natively; use Open to view this file",
        ),
        karet_pdf::PdfError::PageOutOfRange { index, count } => ServiceError::new(
            ErrorCode::InvalidInput,
            format!(
                "PDF page {} is outside this {count}-page document",
                index + 1
            ),
        ),
        karet_pdf::PdfError::Parse => ServiceError::new(
            ErrorCode::InvalidInput,
            "PDF is damaged or uses features the native renderer cannot parse",
        ),
        _ => ServiceError::new(
            ErrorCode::Unsupported,
            format!("PDF cannot be rendered natively: {error}"),
        ),
    }
}

fn write_pdf_png_atomically(path: &Path, page: &karet_pdf::RenderedPage) -> ServiceResult<()> {
    let temporary = path.with_extension(format!("png.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = File::create(&temporary).map_err(ServiceError::from)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), page.width(), page.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| {
            ServiceError::new(
                ErrorCode::Internal,
                format!("Unable to encode PDF page: {error}"),
            )
        })?;
        writer.write_image_data(page.rgba()).map_err(|error| {
            ServiceError::new(
                ErrorCode::Internal,
                format!("Unable to encode PDF page: {error}"),
            )
        })?;
        writer.finish().map_err(|error| {
            ServiceError::new(
                ErrorCode::Internal,
                format!("Unable to finish PDF page: {error}"),
            )
        })?;
        if path.exists() {
            if pdf_png_dimensions(path).is_some() {
                fs::remove_file(&temporary).map_err(ServiceError::from)?;
                return Ok(());
            }
            fs::remove_file(path).map_err(ServiceError::from)?;
        }
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(_) if pdf_png_dimensions(path).is_some() => {
                fs::remove_file(&temporary).map_err(ServiceError::from)
            }
            Err(error) => Err(ServiceError::from(error)),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn prune_pdf_cache(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut pages: Vec<(PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if !name.to_string_lossy().contains("-pdf-page-") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                entry.path(),
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect();
    pages.sort_by_key(|(_, modified, _)| *modified);
    let mut total = pages.iter().map(|(_, _, size)| *size).sum::<u64>();
    while pages.len() > MAX_PDF_CACHE_ENTRIES || total > MAX_PDF_CACHE_BYTES {
        let (path, _, size) = pages.remove(0);
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn get_file_icon(path: &Path, cache: &Path) -> ServiceResult<Option<PathBuf>> {
    #[cfg(windows)]
    {
        get_windows_file_icon(path, cache)
    }
    #[cfg(target_os = "macos")]
    {
        get_macos_file_icon(path, cache)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, cache);
        Ok(None)
    }
}

/// The shell's icon for `path`, extracted in process and cached per source
/// identity.
#[cfg(windows)]
fn get_windows_file_icon(path: &Path, cache: &Path) -> ServiceResult<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    // A new name so 32-pixel icons cached by the former PowerShell extractor
    // are not reused.
    let output = cache_output(cache, path, "shell-icon", "png");
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(Some(output));
    }
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let Some(icon) = windows_icon::shell_icon_with_timeout(path, SHELL_ICON_TIMEOUT)
        .and_then(|icon| icon.fit_within(SYSTEM_ICON_PIXELS))
    else {
        return Ok(None);
    };
    if icon.save_png(&output).is_err() {
        let _ = fs::remove_file(&output);
        return Ok(None);
    }
    prune_icon_cache(cache);
    Ok(Some(output))
}

/// Whether macOS draws this entry with an icon of its own — applications and
/// other packages, symbolic links, Finder aliases and special folders such as
/// Desktop, Downloads or Applications — rather than the icon every item of
/// its kind shares.
///
/// The UI keys its icon cache with this, and [`PreviewService::file_icon`]
/// follows the same rule, so an item-specific icon is never shared by kind.
#[cfg(target_os = "macos")]
pub fn has_item_specific_icon(entry: &explorie_core::FileEntry) -> bool {
    if entry.is_symlink || entry.is_package {
        return true;
    }
    if entry.is_dir {
        return is_special_folder(&entry.path);
    }
    // Finder aliases keep their Finder info in an extended attribute and have
    // no real extension ("Report alias", "Report.pdf alias").
    entry.has_xattrs
        && entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| extension.is_empty() || extension.contains(' '))
}

/// Folders Finder shows with their own icon: the home folder and its standard
/// children, system locations, iCloud Drive, cloud-storage providers and
/// mounted volumes.
#[cfg(target_os = "macos")]
pub fn is_special_folder(path: &Path) -> bool {
    struct SpecialFolders {
        exact: Vec<PathBuf>,
        parents: Vec<PathBuf>,
    }
    static FOLDERS: OnceLock<SpecialFolders> = OnceLock::new();
    let folders = FOLDERS.get_or_init(|| {
        let mut exact: Vec<PathBuf> = [
            "/",
            "/Applications",
            "/Applications/Utilities",
            "/Library",
            "/System",
            "/System/Applications",
            "/System/Applications/Utilities",
            "/Users",
            "/Users/Shared",
            "/Volumes",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let mut parents = vec![PathBuf::from("/Volumes")];
        if let Some(home) = dirs::home_dir() {
            exact.extend(
                [
                    ".Trash",
                    "Applications",
                    "Desktop",
                    "Documents",
                    "Downloads",
                    "Library",
                    "Library/Mobile Documents/com~apple~CloudDocs",
                    "Movies",
                    "Music",
                    "Pictures",
                    "Public",
                    "Sites",
                ]
                .into_iter()
                .map(|child| home.join(child)),
            );
            parents.push(home.join("Library/CloudStorage"));
            exact.push(home);
        }
        SpecialFolders { exact, parents }
    });
    folders.exact.iter().any(|folder| folder == path)
        || path
            .parent()
            .is_some_and(|parent| folders.parents.iter().any(|folder| folder == parent))
}

/// The icon Finder would show for `path`. Items with their own icon (see
/// [`has_item_specific_icon`]) are rendered from the item and cached per
/// source identity; everything else gets its kind's shared icon, cached per
/// extension. Both are also keyed by the light or dark appearance they were
/// drawn in. Cloud placeholders always get the kind icon because inspecting
/// the item could download it.
#[cfg(target_os = "macos")]
fn get_macos_file_icon(path: &Path, cache: &Path) -> ServiceResult<Option<PathBuf>> {
    let Ok(entry) = explorie_core::entry_for_path(path) else {
        return Ok(None);
    };
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let placeholder = treat_as_cloud_placeholder(path, entry.is_cloud_placeholder);
    let dark = macos::icons_draw_dark();
    let (output, rendered) = if has_item_specific_icon(&entry) && !placeholder {
        let suffix = if dark { "dark-icon" } else { "icon" };
        let output = cache_output(cache, path, suffix, "png");
        if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            return Ok(Some(output));
        }
        let rendered = macos::file_icon(path, SYSTEM_ICON_PIXELS, SYSTEM_ICON_TIMEOUT, &output);
        (output, rendered)
    } else {
        let container = if entry.is_package {
            macos::IconContainer::Package
        } else if entry.is_dir {
            macos::IconContainer::Folder
        } else {
            macos::IconContainer::File
        };
        let extension = if container == macos::IconContainer::Folder {
            String::new()
        } else {
            extension(path)
        };
        let output = kind_icon_cache_path(cache, container, &extension, dark);
        if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            return Ok(Some(output));
        }
        let rendered = macos::type_icon(
            &extension,
            container,
            SYSTEM_ICON_PIXELS,
            SYSTEM_ICON_TIMEOUT,
            &output,
        );
        (output, rendered)
    };
    if rendered.is_ok() && output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        prune_icon_cache(cache);
        Ok(Some(output))
    } else {
        let _ = fs::remove_file(&output);
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn kind_icon_cache_path(
    cache: &Path,
    container: macos::IconContainer,
    extension: &str,
    dark: bool,
) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(format!("{container:?}\0{extension}\0{SYSTEM_ICON_PIXELS}\0{dark}").as_bytes());
    cache.join(format!("{}-kind-icon.png", hex_digest(digest.finalize())))
}

#[cfg(any(windows, target_os = "macos"))]
fn prune_icon_cache(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut cached: Vec<(PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if !name.to_string_lossy().contains("-icon.") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                entry.path(),
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect();
    cached.sort_by_key(|(_, modified, _)| *modified);
    let mut total = cached.iter().map(|(_, _, size)| *size).sum::<u64>();
    while cached.len() > MAX_ICON_CACHE_ENTRIES || total > MAX_ICON_CACHE_BYTES {
        let (path, _, size) = cached.remove(0);
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Whether to treat `path` as a cloud placeholder whose contents must not be
/// read. Tests can mark ordinary files, since real placeholders need a cloud
/// provider.
fn treat_as_cloud_placeholder(path: &Path, is_cloud_placeholder: bool) -> bool {
    #[cfg(test)]
    if tests::forced_cloud_placeholder(path) {
        return true;
    }
    let _ = path;
    is_cloud_placeholder
}

/// How a file's thumbnail is produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThumbnailSource {
    Svg,
    Psd,
    /// Formats the bundled image decoder reads.
    NativeImage,
    /// HEIC, AVIF, JPEG XL and camera RAW.
    ExternalImage,
    Video,
    /// Documents, fonts and 3D scenes only Quick Look renders (macOS).
    QuickLook,
}

fn thumbnail_source(path: &Path) -> Option<ThumbnailSource> {
    let extension = extension(path);
    if is_svg_image(path) {
        Some(ThumbnailSource::Svg)
    } else if extension == "psd" {
        Some(ThumbnailSource::Psd)
    } else if is_external_image(path) {
        Some(ThumbnailSource::ExternalImage)
    } else if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"
    ) || is_native_image(path)
    {
        Some(ThumbnailSource::NativeImage)
    } else if matches!(
        extension.as_str(),
        "mp4"
            | "webm"
            | "m4v"
            | "mov"
            | "avi"
            | "mkv"
            | "wmv"
            | "flv"
            | "m2ts"
            | "mts"
            | "mpeg"
            | "mpg"
            | "3gp"
            | "ogv"
            | "ts"
            | "vob"
    ) {
        Some(ThumbnailSource::Video)
    } else if cfg!(target_os = "macos") && is_quicklook_document(path) {
        Some(ThumbnailSource::QuickLook)
    } else {
        None
    }
}

/// Documents, fonts and scenes macOS Quick Look thumbnails without any
/// helper application.
fn is_quicklook_document(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "rtf"
            | "odt"
            | "pages"
            | "numbers"
            | "key"
            | "ttf"
            | "otf"
            | "ttc"
            | "usdz"
            | "usd"
            | "usda"
            | "usdc"
            | "reality"
            | "icns"
            | "exr"
            | "jp2"
    )
}

/// Camera RAW formats. Most are TIFF containers whose first image is only a
/// small embedded preview.
fn is_camera_raw(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf"
    )
}

/// iWork documents, which have no renderer besides Quick Look.
fn is_iwork_document(path: &Path) -> bool {
    matches!(extension(path).as_str(), "pages" | "numbers" | "key")
}

/// A thumbnail PNG for `path`, cached per source identity and size. Formats
/// the bundled decoders read are decoded natively; on macOS ImageIO decodes
/// what they cannot (HEIC, AVIF, RAW) and large images, and Quick Look covers
/// the rest (video poster frames, PDF, Office, iWork, fonts, USDZ...); FFmpeg
/// and ImageMagick are the last resort. Cloud placeholders get no thumbnail
/// because reading them would download them.
fn get_file_thumbnail(path: &Path, max_size: u32, cache: &Path) -> ServiceResult<Option<PathBuf>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if treat_as_cloud_placeholder(path, explorie_core::is_cloud_placeholder(&metadata)) {
        return Ok(None);
    }
    let Some(source) = thumbnail_source(path) else {
        return Ok(None);
    };
    let max_size = max_size.clamp(64, 512);
    let output = cache_output(cache, path, &format!("thumbnail-{max_size}"), "png");
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(Some(output));
    }
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    match source {
        ThumbnailSource::Svg => render_svg_preview(path, &output, max_size)?,
        ThumbnailSource::Psd => {
            imageio_image_before(is_large_image(path), path, &output, max_size, || {
                render_psd_preview(path, &output, max_size)
            })
            .or_else(|error| quicklook_thumbnail_after(path, &output, max_size, error))?
        }
        ThumbnailSource::NativeImage => {
            imageio_image_before(is_large_image(path), path, &output, max_size, || {
                generate_native_thumbnail(path, &output, max_size)
            })
            .or_else(|error| quicklook_thumbnail_after(path, &output, max_size, error))?
        }
        ThumbnailSource::ExternalImage => {
            imageio_image_before(true, path, &output, max_size, || {
                quicklook_thumbnail_before(path, &output, max_size, || {
                    generate_external_image_thumbnail(path, &output, max_size, cache)
                })
            })?
        }
        ThumbnailSource::Video => quicklook_thumbnail_before(path, &output, max_size, || {
            generate_video_thumbnail(path, &output, max_size)
        })?,
        ThumbnailSource::QuickLook => quicklook_thumbnail(path, &output, max_size)?,
    }
    if !output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Err(ServiceError::new(
            ErrorCode::Internal,
            "Native thumbnailer produced an empty image",
        ));
    }
    prune_thumbnail_cache(cache);
    Ok(Some(output))
}

/// Quick Look's thumbnail (macOS only).
fn quicklook_thumbnail(path: &Path, output: &Path, max_size: u32) -> ServiceResult<()> {
    #[cfg(target_os = "macos")]
    {
        macos::quicklook_thumbnail(path, max_size, QUICKLOOK_THUMBNAIL_TIMEOUT, output, None)
            .map_err(|error| quicklook_error(error, QUICKLOOK_THUMBNAIL_TIMEOUT))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, output, max_size);
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Quick Look thumbnails are only available on macOS",
        ))
    }
}

/// Try Quick Look when a native decoder failed with `error` (macOS); keep
/// `error` when Quick Look cannot help either.
fn quicklook_thumbnail_after(
    path: &Path,
    output: &Path,
    max_size: u32,
    error: ServiceError,
) -> ServiceResult<()> {
    if cfg!(target_os = "macos") {
        quicklook_thumbnail(path, output, max_size).map_err(|_| error)
    } else {
        Err(error)
    }
}

/// Try Quick Look first (macOS), then the helper-backed `fallback`.
fn quicklook_thumbnail_before(
    path: &Path,
    output: &Path,
    max_size: u32,
    fallback: impl FnOnce() -> ServiceResult<()>,
) -> ServiceResult<()> {
    if cfg!(target_os = "macos") && quicklook_thumbnail(path, output, max_size).is_ok() {
        return Ok(());
    }
    fallback()
}

/// ImageIO's decode of `path`, fitted within `max_size` and upright, written
/// to `output` as PNG (macOS only). With `cancellation`, a newer preview stops
/// the wait.
fn imageio_image(
    path: &Path,
    output: &Path,
    max_size: u32,
    cancellation: Option<(&AtomicU64, u64)>,
) -> ServiceResult<()> {
    #[cfg(target_os = "macos")]
    {
        let limits = macos::SourceLimits {
            max_dimension: MAX_IMAGE_DECODE_DIMENSION,
            max_pixels: MAX_IMAGEIO_SOURCE_PIXELS,
        };
        macos::imageio_thumbnail(
            path,
            max_size,
            limits,
            IMAGEIO_TIMEOUT,
            output,
            cancellation,
        )
        .map_err(imageio_error)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, output, max_size, cancellation);
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "ImageIO is only available on macOS",
        ))
    }
}

/// On macOS, when `imageio_first`, try ImageIO before `decode` and keep
/// `decode` for what ImageIO cannot read.
fn imageio_image_before(
    imageio_first: bool,
    path: &Path,
    output: &Path,
    max_size: u32,
    decode: impl FnOnce() -> ServiceResult<()>,
) -> ServiceResult<()> {
    if cfg!(target_os = "macos")
        && imageio_first
        && imageio_image(path, output, max_size, None).is_ok()
    {
        return Ok(());
    }
    decode()
}

/// Whether `path` declares more than [`IMAGEIO_MIN_SOURCE_PIXELS`], which
/// macOS decodes with ImageIO first. Only headers are read.
fn is_large_image(path: &Path) -> bool {
    cfg!(target_os = "macos")
        && source_pixel_count(path).is_some_and(|pixels| pixels > IMAGEIO_MIN_SOURCE_PIXELS)
}

/// The pixel count an image's header declares, for formats the bundled
/// decoders read, without decoding pixels.
fn source_pixel_count(path: &Path) -> Option<u64> {
    if let Some((width, height)) = image_header_dimensions(path) {
        return Some(u64::from(width) * u64::from(height));
    }
    // A Photoshop header stores the height and width after its signature,
    // version, reserved bytes and channel count.
    let mut header = [0_u8; 22];
    File::open(path).ok()?.read_exact(&mut header).ok()?;
    if !header.starts_with(b"8BPS") {
        return None;
    }
    let height = u32::from_be_bytes(header[14..18].try_into().ok()?);
    let width = u32::from_be_bytes(header[18..22].try_into().ok()?);
    Some(u64::from(width) * u64::from(height))
}

#[cfg(target_os = "macos")]
fn imageio_error(error: macos::NativeImageError) -> ServiceError {
    match error {
        macos::NativeImageError::Unavailable => {
            ServiceError::new(ErrorCode::Unsupported, "macOS cannot decode this image")
        }
        macos::NativeImageError::TooLarge => ServiceError::new(
            ErrorCode::Unsupported,
            "Image dimensions are too large to preview safely",
        ),
        macos::NativeImageError::TimedOut => ServiceError::new(
            ErrorCode::Internal,
            format!(
                "Decoding the image timed out after {} seconds",
                IMAGEIO_TIMEOUT.as_secs()
            ),
        ),
        macos::NativeImageError::Cancelled => ServiceError::new(
            ErrorCode::Cancelled,
            "Preview helper superseded by a newer preview",
        ),
        macos::NativeImageError::WriteFailed => {
            ServiceError::new(ErrorCode::Internal, "Unable to save the decoded image")
        }
        macos::NativeImageError::InvalidInput => {
            ServiceError::new(ErrorCode::InvalidInput, "macOS cannot open this path")
        }
    }
}

#[cfg(target_os = "macos")]
fn quicklook_error(error: macos::NativeImageError, timeout: Duration) -> ServiceError {
    match error {
        macos::NativeImageError::Unavailable => {
            ServiceError::new(ErrorCode::Unsupported, "Quick Look cannot render this file")
        }
        macos::NativeImageError::TimedOut => ServiceError::new(
            ErrorCode::Internal,
            format!("Quick Look timed out after {} seconds", timeout.as_secs()),
        ),
        macos::NativeImageError::Cancelled => ServiceError::new(
            ErrorCode::Cancelled,
            "Preview helper superseded by a newer preview",
        ),
        macos::NativeImageError::WriteFailed => {
            ServiceError::new(ErrorCode::Internal, "Unable to save the Quick Look image")
        }
        macos::NativeImageError::InvalidInput => {
            ServiceError::new(ErrorCode::InvalidInput, "Quick Look cannot open this path")
        }
        macos::NativeImageError::TooLarge => ServiceError::new(
            ErrorCode::Unsupported,
            "Image dimensions are too large to preview safely",
        ),
    }
}

fn is_model_preview_path(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "glb" | "gltf" | "obj" | "stl" | "ply" | "3mf" | "fbx"
    )
}

fn get_model_thumbnail(
    path: &Path,
    max_size: u32,
    cache: &Path,
    model_cache: &ModelPreviewCache,
) -> ServiceResult<Option<PathBuf>> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if treat_as_cloud_placeholder(path, explorie_core::is_cloud_placeholder(&metadata)) {
        return Ok(None);
    }
    let max_size = max_size.clamp(64, 512);
    let output = cache_output(cache, path, &format!("thumbnail-{max_size}"), "png");
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(Some(output));
    }
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let rendered = model_cache.render(path, ModelCamera::default(), max_size, max_size)?;
    let temporary = output.with_extension(format!("png.{}.tmp", uuid::Uuid::new_v4()));
    let result = image::save_buffer_with_format(
        &temporary,
        &rendered.frame.rgba,
        rendered.frame.width,
        rendered.frame.height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|error| {
        ServiceError::new(
            ErrorCode::Internal,
            format!("Unable to encode model thumbnail: {error}"),
        )
    })
    .and_then(|()| {
        if output.exists() {
            fs::remove_file(&output).map_err(ServiceError::from)?;
        }
        fs::rename(&temporary, &output).map_err(ServiceError::from)
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    prune_thumbnail_cache(cache);
    Ok(Some(output))
}

fn generate_native_thumbnail(input: &Path, output: &Path, max_size: u32) -> ServiceResult<()> {
    let mut reader = image::ImageReader::open(input)
        .map_err(|error| ServiceError::new(ErrorCode::InvalidInput, error.to_string()))?
        .with_guessed_format()
        .map_err(|error| ServiceError::new(ErrorCode::InvalidInput, error.to_string()))?;
    reader.limits(image_decode_limits());
    let image = reader
        .decode()
        .map_err(|error| ServiceError::new(ErrorCode::InvalidInput, error.to_string()))?;
    let max_size = max_size.min(image.width().max(image.height()));
    image
        .thumbnail(max_size, max_size)
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|error| ServiceError::new(ErrorCode::Internal, error.to_string()))
}

fn generate_external_image_thumbnail(
    input: &Path,
    output: &Path,
    max_size: u32,
    cache: &Path,
) -> ServiceResult<()> {
    if extension(input) == "psd" {
        return render_psd_preview(input, output, max_size);
    }
    let coder = imagemagick_coder(input, None).ok_or_else(unsupported_imagemagick_input)?;
    let tool = first_available_tool(&["magick"], "--version").ok_or_else(|| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            "Install ImageMagick to preview this image format.",
        )
    })?;
    let thumbnail = format!("{max_size}x{max_size}>");
    let status = run_helper_with_timeout(
        &mut imagemagick_command(
            &tool,
            cache,
            coder,
            input,
            &["-auto-orient", "-thumbnail", &thumbnail],
            output,
        )?,
        HELPER_CONVERSION_TIMEOUT,
        false,
    )?
    .status;
    if status.success() && output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        Ok(())
    } else {
        Err(ServiceError::new(
            ErrorCode::Internal,
            "ImageMagick could not generate an image thumbnail",
        ))
    }
}

fn image_decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
    limits
}

fn validate_preview_image(path: &Path, max_bytes: u64) -> ServiceResult<()> {
    let metadata = fs::metadata(path).map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Image preview requires a regular file",
        ));
    }
    if metadata.len() > max_bytes {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            format!(
                "Image preview is limited to files no larger than {} MiB",
                max_bytes / 1024 / 1024
            ),
        ));
    }
    Ok(())
}

fn render_svg_preview(input: &Path, output: &Path, max_dimension: u32) -> ServiceResult<()> {
    validate_preview_image(input, MAX_SVG_BYTES)?;
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(());
    }
    let data = fs::read(input).map_err(ServiceError::from)?;
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut options = resvg::usvg::Options::default();
        options.fontdb_mut().load_system_fonts();
        options.image_href_resolver = resvg::usvg::ImageHrefResolver {
            resolve_data: resvg::usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        };
        let tree = resvg::usvg::Tree::from_data(&data, &options)
            .map_err(|error| format!("Unable to parse SVG: {error}"))?;
        let size = tree.size();
        let max_source_dimension = size.width().max(size.height());
        if !max_source_dimension.is_finite() || max_source_dimension <= 0.0 {
            return Err("SVG has invalid dimensions".to_string());
        }
        let max_dimension = max_dimension.clamp(64, IMAGE_PREVIEW_DIMENSION);
        let scale = max_dimension as f32 / max_source_dimension;
        let width = (size.width() * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        let height = (size.height() * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| "Unable to allocate SVG preview".to_string())?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        Ok::<_, String>(pixmap)
    }))
    .map_err(|_| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "SVG renderer rejected malformed or excessively complex content",
        )
    })?
    .map_err(|error| ServiceError::new(ErrorCode::InvalidInput, error))?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(ServiceError::from)?;
    }
    let temporary = output.with_extension(format!("png.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        rendered.save_png(&temporary).map_err(|error| {
            ServiceError::new(
                ErrorCode::Internal,
                format!("Unable to encode SVG preview: {error}"),
            )
        })?;
        if output.exists() {
            if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
                fs::remove_file(&temporary).map_err(ServiceError::from)?;
                return Ok(());
            }
            fs::remove_file(output).map_err(ServiceError::from)?;
        }
        match fs::rename(&temporary, output) {
            Ok(()) => Ok(()),
            Err(_) if output.metadata().is_ok_and(|metadata| metadata.len() > 0) => {
                fs::remove_file(&temporary).map_err(ServiceError::from)
            }
            Err(error) => Err(ServiceError::from(error)),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn generate_video_thumbnail(input: &Path, output: &Path, max_size: u32) -> ServiceResult<()> {
    let tool = first_available_tool(&["ffmpeg"], "-version").ok_or_else(|| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            "Install FFmpeg to generate video thumbnails.",
        )
    })?;
    let filter =
        format!("thumbnail,scale={max_size}:{max_size}:force_original_aspect_ratio=decrease");
    let status = run_helper_with_timeout(
        command(&tool)
            .args(ffmpeg_frame_arguments(input, &filter, output))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        HELPER_CONVERSION_TIMEOUT,
        false,
    )?
    .status;
    status.success().then_some(()).ok_or_else(|| {
        ServiceError::new(
            ErrorCode::Internal,
            "FFmpeg could not generate a video thumbnail",
        )
    })
}

fn prune_thumbnail_cache(cache: &Path) {
    static PRUNE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = PRUNE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut cached: Vec<(PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if !name.to_string_lossy().contains("-thumbnail-") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                entry.path(),
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect();
    cached.sort_by_key(|(_, modified, _)| *modified);
    let mut total = cached.iter().map(|(_, _, size)| *size).sum::<u64>();
    let protected_after = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(2))
        .unwrap_or(std::time::UNIX_EPOCH);
    while cached.len() > MAX_THUMBNAIL_ENTRIES || total > MAX_THUMBNAIL_BYTES {
        let Some(index) = cached
            .iter()
            .position(|(_, modified, _)| *modified < protected_after)
        else {
            break;
        };
        let (path, _, size) = cached.remove(index);
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn generate_preview_artifact(
    path: &Path,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    ensure_helper_current(helper_generation, ticket)?;
    if !path.exists() {
        return Err(ServiceError::new(ErrorCode::NotFound, "File not found."));
    }
    if let Ok(detection) = detect_preview_content(path) {
        match detection.kind {
            DetectedPreviewKind::Svg => return convert_svg_preview(path, cache),
            DetectedPreviewKind::Image => {
                return match detection.mime_type.as_deref() {
                    Some(
                        mime @ ("image/avif"
                        | "image/heif"
                        | "image/jxl"
                        | "image/vnd.adobe.photoshop"),
                    ) => convert_image_preview(path, Some(mime), cache, helper_generation, ticket),
                    // TIFF-based camera RAW would decode natively as its small
                    // embedded preview; macOS develops the RAW data instead.
                    Some("image/tiff") if cfg!(target_os = "macos") && is_camera_raw(path) => {
                        convert_image_preview(path, None, cache, helper_generation, ticket)
                    }
                    _ => convert_native_image_preview(path, cache),
                };
            }
            _ => {}
        }
    }
    if is_external_document(path) {
        return convert_document_preview(path, cache, helper_generation, ticket);
    }
    if cfg!(target_os = "macos") && is_iwork_document(path) {
        return convert_quicklook_preview(path, cache, Some((helper_generation, ticket)));
    }
    if is_external_video(path) {
        return convert_video_preview(path, cache, helper_generation, ticket);
    }
    if is_svg_image(path) {
        return convert_svg_preview(path, cache);
    }
    if is_native_image(path) {
        return convert_native_image_preview(path, cache);
    }
    if is_external_image(path) {
        return convert_image_preview(path, None, cache, helper_generation, ticket);
    }
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "No external preview provider is available for this file type.",
    ))
}

fn first_available_tool(candidates: &[&str], version_arg: &str) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        let output = run_helper_with_timeout(
            command(candidate).arg(version_arg),
            HELPER_PROBE_TIMEOUT,
            false,
        )
        .ok()?;
        output.status.success().then(|| (*candidate).to_string())
    })
}

fn convert_document_preview(
    path: &Path,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    convert_libreoffice_preview(path, cache, helper_generation, ticket)
        .or_else(|error| quicklook_preview_fallback(path, cache, helper_generation, ticket, error))
}

fn convert_libreoffice_preview(
    path: &Path,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    let tool = first_available_tool(&["soffice", "libreoffice"], "--version").ok_or_else(|| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            "Install LibreOffice to preview Office and OpenDocument files.",
        )
    })?;
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "document", "pdf");
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(PreviewArtifact {
            kind: "pdf".into(),
            path: output,
            mime_type: "application/pdf".into(),
            tool,
        });
    }
    if output.exists() {
        fs::remove_file(&output).map_err(ServiceError::from)?;
    }
    let profile = prepare_libreoffice_profile(cache)?;
    let isolated_output = cache.join(format!(".document-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&isolated_output).map_err(ServiceError::from)?;
    let conversion = (|| {
        let status = run_helper_with_cancellation(
            command(&tool)
                .args(libreoffice_arguments(&profile, &isolated_output, path))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            HELPER_CONVERSION_TIMEOUT,
            false,
            Some((helper_generation, ticket)),
        )?
        .status;
        if !status.success() {
            return Err(ServiceError::new(
                ErrorCode::Internal,
                "LibreOffice could not convert this document for preview.",
            ));
        }
        let produced = isolated_output.join(format!(
            "{}.pdf",
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("preview")
        ));
        if !produced.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            return Err(ServiceError::new(
                ErrorCode::Internal,
                "LibreOffice finished without producing a PDF preview.",
            ));
        }
        fs::rename(&produced, &output).map_err(ServiceError::from)
    })();
    let _ = fs::remove_dir_all(&isolated_output);
    conversion?;
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "pdf".into(),
        path: output,
        mime_type: "application/pdf".into(),
        tool,
    })
}

fn convert_video_preview(
    path: &Path,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    let tool = first_available_tool(&["ffmpeg"], "-version").ok_or_else(|| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            "Install FFmpeg to preview this video format.",
        )
    })?;
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "video", "png");
    let status = run_helper_with_cancellation(
        command(&tool)
            .args(ffmpeg_frame_arguments(
                path,
                "thumbnail,scale=1280:-1",
                &output,
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        HELPER_CONVERSION_TIMEOUT,
        false,
        Some((helper_generation, ticket)),
    )?
    .status;
    if !status.success() || !output.exists() {
        return Err(ServiceError::new(
            ErrorCode::Internal,
            "FFmpeg could not generate a thumbnail for this video.",
        ));
    }
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool,
    })
}

fn convert_svg_preview(path: &Path, cache: &Path) -> ServiceResult<PreviewArtifact> {
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "svg", "png");
    render_svg_preview(path, &output, IMAGE_PREVIEW_DIMENSION)?;
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool: "Explorie SVG renderer".into(),
    })
}

fn convert_native_image_preview(path: &Path, cache: &Path) -> ServiceResult<PreviewArtifact> {
    validate_preview_image(path, MAX_IMAGE_PREVIEW_BYTES)?;
    if is_large_image(path)
        && let Ok(artifact) = convert_imageio_preview(path, cache, None)
    {
        return Ok(artifact);
    }
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "native-image", "png");
    if !output.metadata().is_ok_and(|metadata| metadata.len() > 0)
        && let Err(error) = generate_native_thumbnail(path, &output, IMAGE_PREVIEW_DIMENSION)
    {
        let _ = fs::remove_file(&output);
        // Variants the bundled decoder rejects (for example some TIFF
        // compressions) may still render through Quick Look on macOS.
        return if cfg!(target_os = "macos") {
            convert_quicklook_preview(path, cache, None).map_err(|_| error)
        } else {
            Err(error)
        };
    }
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool: "Explorie image decoder".into(),
    })
}

/// A preview of a format the bundled raster decoder cannot read: HEIC, AVIF,
/// JPEG XL, Photoshop and camera RAW. macOS decodes them with ImageIO (small
/// Photoshop documents keep the bundled decoder); otherwise, and when ImageIO
/// fails, the PSD decoder or ImageMagick converts them, and Quick Look stands
/// in for a missing or failing helper.
fn convert_image_preview(
    path: &Path,
    detected_mime: Option<&str>,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    let is_psd = extension(path) == "psd";
    if cfg!(target_os = "macos") && (!is_psd || is_large_image(path)) {
        match convert_imageio_preview(path, cache, Some((helper_generation, ticket))) {
            Ok(artifact) => return Ok(artifact),
            Err(error) if error.code == ErrorCode::Cancelled => return Err(error),
            Err(_) => {}
        }
    }
    let converted = if is_psd {
        convert_psd_preview(path, cache)
    } else {
        convert_imagemagick_preview(path, detected_mime, cache, helper_generation, ticket)
    };
    converted
        .or_else(|error| quicklook_preview_fallback(path, cache, helper_generation, ticket, error))
}

fn convert_psd_preview(path: &Path, cache: &Path) -> ServiceResult<PreviewArtifact> {
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "image", "png");
    render_psd_preview(path, &output, IMAGE_PREVIEW_DIMENSION)?;
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool: "Explorie PSD decoder".into(),
    })
}

fn convert_imagemagick_preview(
    path: &Path,
    detected_mime: Option<&str>,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
) -> ServiceResult<PreviewArtifact> {
    let coder = imagemagick_coder(path, detected_mime).ok_or_else(unsupported_imagemagick_input)?;
    let tool = first_available_tool(&["magick"], "--version").ok_or_else(|| {
        ServiceError::new(
            ErrorCode::HelperMissing,
            "Install ImageMagick to preview this image format.",
        )
    })?;
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "image", "png");
    let status = run_helper_with_cancellation(
        &mut imagemagick_command(&tool, cache, coder, path, &[], &output)?,
        HELPER_CONVERSION_TIMEOUT,
        false,
        Some((helper_generation, ticket)),
    )?
    .status;
    if !status.success() || !output.exists() {
        return Err(ServiceError::new(
            ErrorCode::Internal,
            "ImageMagick could not convert this image for preview.",
        ));
    }
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool,
    })
}

/// ImageIO's decode of `path`, fitted within the preview dimension, as the
/// preview image (macOS only). With `cancellation`, a newer preview stops the
/// wait.
fn convert_imageio_preview(
    path: &Path,
    cache: &Path,
    cancellation: Option<(&AtomicU64, u64)>,
) -> ServiceResult<PreviewArtifact> {
    validate_preview_image(path, MAX_IMAGE_PREVIEW_BYTES)?;
    if let Some((generation, ticket)) = cancellation {
        ensure_helper_current(generation, ticket)?;
    }
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache_output(cache, path, "imageio", "png");
    if !output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        imageio_image(path, &output, IMAGE_PREVIEW_DIMENSION, cancellation)?;
    }
    prune_generated_artifact_cache(cache, &output);
    Ok(PreviewArtifact {
        kind: "image".into(),
        path: output,
        mime_type: "image/png".into(),
        tool: IMAGEIO_TOOL.into(),
    })
}

/// Quick Look's rendering of `path`, fitted within the preview dimension, as
/// the preview image (macOS only). Sizes Quick Look refuses are retried
/// smaller (see [`QUICKLOOK_PREVIEW_DIMENSIONS`]) within one timeout. With
/// `cancellation`, a newer preview stops the wait.
fn convert_quicklook_preview(
    path: &Path,
    cache: &Path,
    cancellation: Option<(&AtomicU64, u64)>,
) -> ServiceResult<PreviewArtifact> {
    #[cfg(target_os = "macos")]
    {
        validate_preview_image(path, MAX_IMAGE_PREVIEW_BYTES)?;
        if let Some((generation, ticket)) = cancellation {
            ensure_helper_current(generation, ticket)?;
        }
        fs::create_dir_all(cache).map_err(ServiceError::from)?;
        let output = cache_output(cache, path, "quicklook", "png");
        if !output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            quicklook_with_smaller_retries(QUICKLOOK_PREVIEW_TIMEOUT, |dimension, timeout| {
                macos::quicklook_thumbnail(path, dimension, timeout, &output, cancellation)
            })
            .map_err(|error| quicklook_error(error, QUICKLOOK_PREVIEW_TIMEOUT))?;
        }
        prune_generated_artifact_cache(cache, &output);
        Ok(PreviewArtifact {
            kind: "image".into(),
            path: output,
            mime_type: "image/png".into(),
            tool: "Quick Look".into(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, cache, cancellation);
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Quick Look previews are only available on macOS",
        ))
    }
}

/// Run `attempt` with each of [`QUICKLOOK_PREVIEW_DIMENSIONS`] and the time
/// left of `timeout`, moving to the next, smaller size only while Quick Look
/// reports it has no image. Timeouts, cancellation and success end the loop.
#[cfg(target_os = "macos")]
fn quicklook_with_smaller_retries(
    timeout: Duration,
    mut attempt: impl FnMut(u32, Duration) -> Result<(), macos::NativeImageError>,
) -> Result<(), macos::NativeImageError> {
    let deadline = Instant::now() + timeout;
    let mut result = Err(macos::NativeImageError::Unavailable);
    for dimension in QUICKLOOK_PREVIEW_DIMENSIONS {
        result = attempt(
            dimension,
            deadline.saturating_duration_since(Instant::now()),
        );
        if result != Err(macos::NativeImageError::Unavailable) {
            break;
        }
    }
    result
}

/// On macOS, answer a failed or unavailable helper conversion with Quick
/// Look's rendering instead. `error` stays the result when Quick Look cannot
/// render the file either, so its guidance (such as which helper to install)
/// still reaches the user; a superseded preview stays cancelled.
fn quicklook_preview_fallback(
    path: &Path,
    cache: &Path,
    helper_generation: &AtomicU64,
    ticket: u64,
    error: ServiceError,
) -> ServiceResult<PreviewArtifact> {
    if !cfg!(target_os = "macos") || error.code == ErrorCode::Cancelled {
        return Err(error);
    }
    convert_quicklook_preview(path, cache, Some((helper_generation, ticket))).map_err(
        |quicklook_error| {
            if quicklook_error.code == ErrorCode::Cancelled {
                quicklook_error
            } else {
                error
            }
        },
    )
}

/// Arguments for extracting one frame with FFmpeg. `-nostdin` keeps FFmpeg from
/// reading the terminal, and the input protocol whitelist stops a crafted
/// container or playlist from reaching network, `concat:` or `subfile:` URLs.
fn ffmpeg_frame_arguments(input: &Path, filter: &str, output: &Path) -> Vec<std::ffi::OsString> {
    let mut arguments: Vec<std::ffi::OsString> = [
        "-nostdin",
        "-hide_banner",
        "-y",
        "-protocol_whitelist",
        FFMPEG_INPUT_PROTOCOLS,
        "-i",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    arguments.push(input.into());
    arguments.extend(["-frames:v", "1", "-vf", filter].map(Into::into));
    arguments.push(output.into());
    arguments
}

/// The ImageMagick decoder for a file, chosen explicitly so ImageMagick never
/// picks one by sniffing content (for example Ghostscript for a disguised
/// PostScript file). A signature-detected type wins over the extension.
fn imagemagick_coder(path: &Path, detected_mime: Option<&str>) -> Option<&'static str> {
    match detected_mime {
        Some("image/avif") => return Some("AVIF"),
        Some("image/heif") => return Some("HEIC"),
        Some("image/jxl") => return Some("JXL"),
        Some("image/vnd.adobe.photoshop") => return Some("PSD"),
        _ => {}
    }
    Some(match extension(path).as_str() {
        "heic" | "heif" => "HEIC",
        "avif" => "AVIF",
        "jxl" | "jpegxl" => "JXL",
        "psd" => "PSD",
        "dng" => "DNG",
        "cr2" => "CR2",
        "cr3" => "CR3",
        "nef" => "NEF",
        "arw" => "ARW",
        "orf" => "ORF",
        "rw2" => "RW2",
        "raf" => "RAF",
        _ => return None,
    })
}

fn unsupported_imagemagick_input() -> ServiceError {
    ServiceError::new(
        ErrorCode::Unsupported,
        "No safe ImageMagick decoder is configured for this file type.",
    )
}

/// Write Explorie's restrictive ImageMagick policy into the preview cache and
/// return the directory to use as `MAGICK_CONFIGURE_PATH`.
fn prepare_imagemagick_config(cache: &Path) -> ServiceResult<PathBuf> {
    let directory = cache.join("imagemagick");
    write_if_changed(&directory.join("policy.xml"), IMAGEMAGICK_POLICY.as_bytes())?;
    Ok(directory)
}

fn imagemagick_arguments(
    coder: &str,
    input: &Path,
    operations: &[&str],
    output: &Path,
) -> Vec<std::ffi::OsString> {
    let mut arguments = Vec::<std::ffi::OsString>::new();
    for (resource, limit) in IMAGEMAGICK_LIMITS {
        arguments.extend(["-limit", resource, limit].map(Into::into));
    }
    let mut source = std::ffi::OsString::from(format!("{coder}:"));
    source.push(input);
    arguments.push(source);
    arguments.extend(operations.iter().map(Into::into));
    let mut destination = std::ffi::OsString::from("PNG:");
    destination.push(output);
    arguments.push(destination);
    arguments
}

fn imagemagick_command(
    tool: &str,
    cache: &Path,
    coder: &str,
    input: &Path,
    operations: &[&str],
    output: &Path,
) -> ServiceResult<Command> {
    let config = prepare_imagemagick_config(cache)?;
    let mut command = command(tool);
    command
        .args(imagemagick_arguments(coder, input, operations, output))
        .env("MAGICK_CONFIGURE_PATH", config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// Create (or repair) the dedicated LibreOffice profile used for previews. It
/// keeps conversions away from the user's own profile, macros and trusted
/// locations, and pins macro security to its highest level with link updates,
/// active content and recalculation of external data turned off.
fn prepare_libreoffice_profile(cache: &Path) -> ServiceResult<PathBuf> {
    let profile = cache.join("libreoffice-profile");
    let registry = profile.join("user").join("registrymodifications.xcu");
    let current = fs::read_to_string(&registry).unwrap_or_default();
    let seeded = LIBREOFFICE_REGISTRY
        .lines()
        .filter(|line| line.starts_with("<item "))
        .all(|line| current.contains(line));
    if !seeded {
        write_if_changed(&registry, LIBREOFFICE_REGISTRY.as_bytes())?;
    }
    Ok(profile)
}

fn libreoffice_arguments(
    profile: &Path,
    output_dir: &Path,
    input: &Path,
) -> Vec<std::ffi::OsString> {
    let mut arguments: Vec<std::ffi::OsString> = vec![
        format!("-env:UserInstallation={}", file_url(profile)).into(),
        "--headless".into(),
        "--norestore".into(),
        "--nolockcheck".into(),
        "--nodefault".into(),
        "--nologo".into(),
        "--convert-to".into(),
        "pdf".into(),
        "--outdir".into(),
    ];
    arguments.push(output_dir.into());
    arguments.push(input.into());
    arguments
}

/// A `file://` URL for an absolute local path, percent-encoding everything
/// outside the unreserved set so profile paths with spaces or non-ASCII text
/// survive LibreOffice's URL parsing.
fn file_url(path: &Path) -> String {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut url = String::from("file://");
    if !bytes.starts_with(b"/") {
        url.push('/');
    }
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'\\' if cfg!(windows) => url.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(char::from(byte));
            }
            // Keep a Windows drive separator (`C:`) literal.
            b':' if index == 1 => url.push(':'),
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

fn write_if_changed(path: &Path, contents: &[u8]) -> ServiceResult<()> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        ServiceError::new(ErrorCode::Internal, "Helper configuration has no parent")
    })?;
    fs::create_dir_all(parent).map_err(ServiceError::from)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = fs::write(&temporary, contents)
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(ServiceError::from);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn render_psd_preview(input: &Path, output: &Path, max_dimension: u32) -> ServiceResult<()> {
    validate_preview_image(input, MAX_IMAGE_PREVIEW_BYTES)?;
    if output.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(());
    }
    let bytes = fs::read(input).map_err(ServiceError::from)?;
    let document = psd::Psd::from_bytes(&bytes).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to parse Photoshop document: {error}"),
        )
    })?;
    let width = document.width();
    let height = document.height();
    let decoded_bytes = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DECODE_DIMENSION
        || height > MAX_IMAGE_DECODE_DIMENSION
        || decoded_bytes > MAX_IMAGE_DECODE_BYTES
    {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Photoshop document dimensions are too large to preview safely",
        ));
    }
    let pixels = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| document.rgba()))
        .map_err(|_| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                "Unable to decode Photoshop document pixels",
            )
        })?;
    let image = image::RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "Photoshop document returned invalid pixel data",
        )
    })?;
    let max_dimension = max_dimension
        .clamp(64, IMAGE_PREVIEW_DIMENSION)
        .min(width.max(height));
    image::DynamicImage::ImageRgba8(image)
        .thumbnail(max_dimension, max_dimension)
        .save_with_format(output, image::ImageFormat::Png)
        .map_err(|error| ServiceError::new(ErrorCode::Internal, error.to_string()))
}

fn prune_generated_artifact_cache(cache: &Path, protected: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let mut artifacts: Vec<(PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path == protected || !path.is_file() {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if ![
                "-document.",
                "-video.",
                "-svg.",
                "-native-image.",
                "-image.",
                "-imageio.",
                "-quicklook.",
            ]
            .iter()
            .any(|marker| name.contains(marker))
            {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            Some((
                path,
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect();
    artifacts.sort_by_key(|(_, modified, _)| *modified);
    let protected_size = protected
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut total = artifacts
        .iter()
        .map(|(_, _, size)| *size)
        .sum::<u64>()
        .saturating_add(protected_size);
    while artifacts.len().saturating_add(1) > MAX_GENERATED_ARTIFACT_ENTRIES
        || total > MAX_GENERATED_ARTIFACT_BYTES
    {
        let Some((path, _, size)) = artifacts.first().cloned() else {
            break;
        };
        artifacts.remove(0);
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[derive(Debug)]
struct HelperOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_helper_with_timeout(
    command: &mut Command,
    timeout: Duration,
    capture_stdout: bool,
) -> ServiceResult<HelperOutput> {
    run_helper_with_cancellation(command, timeout, capture_stdout, None)
}

fn run_helper_with_cancellation(
    command: &mut Command,
    timeout: Duration,
    capture_stdout: bool,
    cancellation: Option<(&AtomicU64, u64)>,
) -> ServiceResult<HelperOutput> {
    if capture_stdout {
        command.stdout(Stdio::piped());
    } else {
        command.stdout(Stdio::null());
    }
    command.stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| ServiceError::new(ErrorCode::HelperMissing, error.to_string()))?;
    let stdout_reader = child.stdout.take().map(|mut stdout| {
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = stdout.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let remaining = MAX_HELPER_STDOUT_BYTES.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..count.min(remaining)]);
            }
            captured
        })
    });
    let started = Instant::now();
    let status = loop {
        if cancellation
            .is_some_and(|(generation, ticket)| generation.load(Ordering::Acquire) != ticket)
        {
            terminate_helper_process(&mut child);
            let _ = child.wait();
            if let Some(reader) = stdout_reader {
                let _ = reader.join();
            }
            return Err(ServiceError::new(
                ErrorCode::Cancelled,
                "Preview helper superseded by a newer preview",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                terminate_helper_process(&mut child);
                let _ = child.wait();
                if let Some(reader) = stdout_reader {
                    let _ = reader.join();
                }
                return Err(ServiceError::new(
                    ErrorCode::Internal,
                    format!(
                        "Preview helper timed out after {} seconds",
                        timeout.as_secs()
                    ),
                ));
            }
            Err(error) => {
                terminate_helper_process(&mut child);
                let _ = child.wait();
                return Err(ServiceError::from(error));
            }
        }
    };
    let stdout = stdout_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    Ok(HelperOutput { status, stdout })
}

fn ensure_helper_current(generation: &AtomicU64, ticket: u64) -> ServiceResult<()> {
    if generation.load(Ordering::Acquire) == ticket {
        Ok(())
    } else {
        Err(ServiceError::new(
            ErrorCode::Cancelled,
            "Preview helper superseded by a newer preview",
        ))
    }
}

fn terminate_helper_process(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    unsafe {
        // Helper commands are spawned into their own process group below, so
        // killing the negative PID also terminates descendants they started.
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Largest decoded size, in pixels, that a direct image preview hands to the
/// UI unchanged. Bigger images are downscaled to fit
/// [`IMAGE_PREVIEW_DIMENSION`] on their longest edge, which is roughly twice
/// the largest preview viewport (Quick Look).
const DISPLAY_IMAGE_MAX_PIXELS: u64 =
    IMAGE_PREVIEW_DIMENSION as u64 * IMAGE_PREVIEW_DIMENSION as u64;

impl PreviewService {
    /// Resolve the file the UI should decode for a direct (PNG, JPEG, GIF,
    /// BMP, WebP) image preview. Images up to [`DISPLAY_IMAGE_MAX_PIXELS`] are
    /// returned unchanged; larger ones go through the native image preview
    /// path, which decodes under the service's decode limits and writes a
    /// downscaled copy to the preview cache. The UI therefore never decodes a
    /// 100-megapixel photo at full resolution.
    ///
    /// Dropping the returned task abandons work that has not started yet.
    pub fn display_image(&self, path: PathBuf) -> BlockingTask<PathBuf> {
        let cache = self.cache_dir();
        let cache_gate = Arc::clone(&self.cache_gate);
        let artifact_lock = Arc::clone(&self.artifact_lock);
        self.context.spawn_cancellable(move |token| {
            let Some((width, height)) = image_header_dimensions(&path) else {
                // Unreadable headers keep the historical behaviour: the UI
                // decoder reports what it cannot display.
                return Ok(path);
            };
            if u64::from(width) * u64::from(height) <= DISPLAY_IMAGE_MAX_PIXELS {
                return Ok(path);
            }
            let superseded =
                || ServiceError::new(ErrorCode::Cancelled, "Image preview was superseded");
            if token.is_cancelled() {
                return Err(superseded());
            }
            let _cache_guard = cache_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _artifact_guard = artifact_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if token.is_cancelled() {
                return Err(superseded());
            }
            convert_native_image_preview(&path, &cache).map(|artifact| artifact.path)
        })
    }
}

/// Read an image's dimensions from its header without decoding pixels.
fn image_header_dimensions(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod display_image_tests {
    use super::*;
    use crate::ResourcePaths;

    #[test]
    fn small_images_stay_direct_and_large_ones_are_downscaled_once() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let small = temp.path().join("small.png");
        image::RgbImage::new(64, 48).save(&small).unwrap();
        assert_eq!(service.display_image(small.clone()).wait().unwrap(), small);

        let unreadable = temp.path().join("unreadable.png");
        fs::write(&unreadable, b"not an image").unwrap();
        assert_eq!(
            service.display_image(unreadable.clone()).wait().unwrap(),
            unreadable
        );

        let large = temp.path().join("large.png");
        image::RgbImage::new(2_400, 1_800).save(&large).unwrap();
        let shown = service.display_image(large.clone()).wait().unwrap();
        assert_ne!(shown, large);
        assert!(shown.starts_with(service.cache_dir()));
        let (width, height) = image::image_dimensions(&shown).unwrap();
        assert_eq!(width, IMAGE_PREVIEW_DIMENSION);
        assert!(height < IMAGE_PREVIEW_DIMENSION);
        assert_eq!(service.display_image(large).wait().unwrap(), shown);
    }

    #[test]
    fn huge_images_downscale_to_bounded_previews_and_thumbnails() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        // 48 megapixels: above the size macOS hands to ImageIO.
        let huge = temp.path().join("huge.png");
        image::GrayImage::from_fn(8_000, 6_000, |x, y| image::Luma([((x ^ y) % 256) as u8]))
            .save(&huge)
            .unwrap();
        assert_eq!(is_large_image(&huge), cfg!(target_os = "macos"));

        let thumbnail = service
            .thumbnail(huge.clone(), 256)
            .wait()
            .unwrap()
            .unwrap();
        assert_eq!(image::image_dimensions(&thumbnail).unwrap(), (256, 192));

        let shown = service.display_image(huge.clone()).wait().unwrap();
        assert!(shown.starts_with(service.cache_dir()));
        assert_eq!(
            image::image_dimensions(&shown).unwrap(),
            (IMAGE_PREVIEW_DIMENSION, 1_536)
        );

        let artifact = service.artifact(huge).wait().unwrap();
        assert_eq!(artifact.path, shown, "the preview reuses the cached decode");
        let expected_tool = if cfg!(target_os = "macos") {
            IMAGEIO_TOOL
        } else {
            "Explorie image decoder"
        };
        assert_eq!(artifact.tool, expected_tool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourcePaths;
    use std::io::Write;

    #[test]
    fn preview_forks_share_cache_gates_but_not_window_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let fork = service.fork_cancellation_scope();
        assert!(Arc::ptr_eq(&service.cache_gate, &fork.cache_gate));
        assert!(Arc::ptr_eq(&service.artifact_lock, &fork.artifact_lock));
        assert!(!Arc::ptr_eq(
            &service.helper_generation,
            &fork.helper_generation
        ));
    }

    fn write_test_image(path: &Path, width: u32, height: u32, color: [u8; 4]) {
        image::RgbaImage::from_pixel(width, height, image::Rgba(color))
            .save(path)
            .unwrap();
    }

    fn minimal_psd(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&4_u16.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8_u16.to_be_bytes());
        bytes.extend_from_slice(&3_u16.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        let pixel_count = usize::try_from(u64::from(width) * u64::from(height)).unwrap();
        for channel in color {
            bytes.extend(std::iter::repeat_n(channel, pixel_count));
        }
        bytes
    }

    fn minimal_pdf(page_count: usize) -> Vec<u8> {
        minimal_pdf_with_dimensions(page_count, 612, 792)
    }

    fn minimal_pdf_with_dimensions(page_count: usize, width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut objects = Vec::with_capacity(page_count + 2);
        objects.push("<</Type/Catalog/Pages 2 0 R>>".to_string());
        let kids = (0..page_count)
            .map(|index| format!("{} 0 R", index + 3))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push(format!("<</Type/Pages/Kids[{kids}]/Count {page_count}>>"));
        for _ in 0..page_count {
            objects.push(format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {width} {height}]>>"
            ));
        }
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj{object}endobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF",
                objects.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn text_preview_is_bounded_and_reports_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        File::create(&path).unwrap().write_all(b"abcdef").unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let preview = service.read_text(path, 4).wait().unwrap();
        assert_eq!(preview.text, "abcd");
        assert!(preview.truncated);
        assert_eq!(preview.language, None);
        assert!(!preview.wrapped);
        assert!(preview.highlights.is_empty());
    }

    #[test]
    fn pdf_pages_render_locally_and_cache_by_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("document.pdf");
        fs::write(&path, minimal_pdf(2)).unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let first = service.pdf_page(path.clone(), 0).wait().unwrap();
        assert_eq!(first.page_count, 2);
        assert_eq!(first.page_index, 0);
        assert_eq!((first.display_width, first.display_height), (612, 792));
        assert_eq!((first.pixel_width, first.pixel_height), (918, 1188));
        assert!(first.image_path.is_file());
        assert_eq!(
            &fs::read(&first.image_path).unwrap()[..8],
            b"\x89PNG\r\n\x1a\n"
        );

        let cached = service.pdf_page(path.clone(), 0).wait().unwrap();
        assert_eq!(cached.image_path, first.image_path);
        let second = service.pdf_page(path.clone(), 1).wait().unwrap();
        assert_eq!(second.page_index, 1);
        assert_ne!(second.image_path, first.image_path);

        fs::write(&path, minimal_pdf(1)).unwrap();
        let changed = service.pdf_page(path, 0).wait().unwrap();
        assert_eq!(changed.page_count, 1);
        assert_ne!(changed.image_path, first.image_path);
    }

    #[test]
    fn pdf_failures_are_bounded_and_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let missing = service
            .pdf_page(temp.path().join("missing.pdf"), 0)
            .wait()
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);

        let malformed = temp.path().join("malformed.pdf");
        fs::write(&malformed, b"not a PDF").unwrap();
        let error = service.pdf_page(malformed, 0).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);

        let oversized = temp.path().join("oversized.pdf");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_PDF_BYTES + 1)
            .unwrap();
        let error = service.pdf_page(oversized, 0).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);

        let huge_page = temp.path().join("huge-page.pdf");
        fs::write(&huge_page, minimal_pdf_with_dimensions(1, 20_000, 20_000)).unwrap();
        let error = service.pdf_page(huge_page, 0).wait().unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert!(error.message.contains("dimensions"));

        let encrypted = map_pdf_error(karet_pdf::PdfError::Encrypted);
        assert_eq!(encrypted.code, ErrorCode::Unsupported);
        assert!(encrypted.message.contains("Password-protected"));
    }

    #[test]
    fn helper_processes_are_terminated_at_the_deadline() {
        #[cfg(windows)]
        let mut helper = {
            let mut helper = command("powershell.exe");
            helper.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 5",
            ]);
            helper
        };
        #[cfg(not(windows))]
        let mut helper = {
            let mut helper = command("sh");
            helper.args(["-c", "sleep 5"]);
            helper
        };
        let started = Instant::now();

        let error = run_helper_with_timeout(&mut helper, Duration::from_millis(50), false)
            .expect_err("helper should time out");

        assert!(error.message.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn unix_helper_timeout_terminates_the_descendant_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let pid_path = temp.path().join("descendant.pid");
        let mut helper = command("sh");
        helper
            .arg("-c")
            .arg("sleep 30 & child=$!; printf '%s' \"$child\" > \"$1\"; wait")
            .arg("explorie-helper-test")
            .arg(&pid_path);

        let error = run_helper_with_timeout(&mut helper, Duration::from_millis(250), false)
            .expect_err("helper process tree should time out");
        assert!(error.message.contains("timed out"));

        let descendant_pid: i32 = fs::read_to_string(&pid_path)
            .expect("descendant pid should be recorded before timeout")
            .parse()
            .unwrap();
        // A killed process stays a zombie until its new parent reaps it, which
        // never happens under a PID 1 that doesn't reap (e.g. in a container).
        let is_zombie = |pid: i32| {
            fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
            })
        };
        let stopped = (0..100).any(|_| {
            let alive = (unsafe { libc::kill(descendant_pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
                && !is_zombie(descendant_pid);
            if alive {
                std::thread::sleep(Duration::from_millis(10));
            }
            !alive
        });
        assert!(
            stopped,
            "descendant process {descendant_pid} survived timeout"
        );
    }

    #[test]
    fn generated_artifact_cache_pruning_preserves_the_active_result() {
        let temp = tempfile::tempdir().unwrap();
        let protected = temp.path().join("active-image.png");
        fs::write(&protected, b"active").unwrap();
        for index in 0..(MAX_GENERATED_ARTIFACT_ENTRIES + 10) {
            fs::write(
                temp.path().join(format!("cached-{index:03}-image.png")),
                b"cached",
            )
            .unwrap();
        }

        prune_generated_artifact_cache(temp.path(), &protected);

        let remaining = fs::read_dir(temp.path()).unwrap().count();
        assert!(protected.exists());
        assert!(remaining <= MAX_GENERATED_ARTIFACT_ENTRIES);
    }

    #[test]
    fn code_preview_detects_language_and_returns_bounded_semantic_ranges() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.rs");
        fs::write(
            &path,
            "// local only\npub fn answer() -> u32 { 42 }\nlet label = \"explorie\";\n",
        )
        .unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let preview = service.read_text(path, 512 * 1024).wait().unwrap();
        assert_eq!(preview.language.as_deref(), Some("Rust"));
        assert!(!preview.wrapped);
        assert!(preview.highlights.len() < MAX_TEXT_HIGHLIGHTS);
        assert!(preview.highlights.iter().all(|highlight| {
            highlight.start < highlight.end
                && highlight.end <= preview.text.len()
                && preview.text.is_char_boundary(highlight.start)
                && preview.text.is_char_boundary(highlight.end)
        }));
        assert!(
            preview
                .highlights
                .iter()
                .any(|highlight| highlight.kind == TextHighlightKind::Comment)
        );
        assert!(
            preview
                .highlights
                .iter()
                .any(|highlight| highlight.kind == TextHighlightKind::Keyword)
        );
        assert!(
            preview
                .highlights
                .iter()
                .any(|highlight| highlight.kind == TextHighlightKind::String)
        );
    }

    #[test]
    fn markdown_wraps_and_unknown_text_remains_plain() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let markdown = temp.path().join("notes.md");
        fs::write(&markdown, "# Local notes\n\nA long paragraph").unwrap();
        let preview = service.read_text(markdown, 512 * 1024).wait().unwrap();
        assert!(preview.wrapped);
        assert_eq!(preview.language.as_deref(), Some("Markdown"));

        let plain = temp.path().join("events.log");
        fs::write(&plain, "one\ntwo\n").unwrap();
        let preview = service.read_text(plain, 512 * 1024).wait().unwrap();
        assert_eq!(preview.language, None);
        assert!(preview.highlights.is_empty());
    }

    #[test]
    fn legacy_code_aliases_resolve_to_bundled_local_syntaxes() {
        for (extension, source) in [
            ("tsx", "export const View = () => <div>Local</div>;"),
            ("jsonc", "{ // local\n  \"enabled\": true\n}"),
            ("pyw", "def main():\n    return 42\n"),
            ("csharp", "public class Local {}"),
            ("hpp", "struct Local { int value; };"),
            ("bash", "echo \"local\""),
            ("lock", "version = 3"),
        ] {
            let path = PathBuf::from(format!("preview.{extension}"));
            let (language, highlights) = highlight_text(&path, source);
            assert!(language.is_some(), "{extension} should resolve a syntax");
            assert!(
                highlights
                    .iter()
                    .all(|highlight| highlight.end <= source.len()),
                "{extension} ranges should stay inside the source"
            );
        }
    }

    #[test]
    fn cache_is_injected_and_clear_does_not_touch_sources() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        fs::write(&source, "keep").unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        fs::create_dir_all(service.cache_dir()).unwrap();
        fs::write(service.cache_dir().join("artifact"), "cache").unwrap();
        service.clear_cache().wait().unwrap();
        assert_eq!(fs::read_to_string(source).unwrap(), "keep");
        assert!(service.cache_dir().is_dir());
    }

    #[test]
    fn generated_artifact_cache_paths_track_source_identity_and_artifact_kind() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let source = temp.path().join("design.psd");
        fs::write(&source, b"first identity").unwrap();

        let first = cache_output(&cache, &source, "image", "png");
        assert_eq!(first, cache_output(&cache, &source, "image", "png"));
        assert_ne!(
            first,
            cache_output(&cache, &source, "document", "pdf"),
            "artifact kinds must not share a cache slot"
        );

        fs::write(&source, b"a different and longer source identity").unwrap();
        let changed = cache_output(&cache, &source, "image", "png");
        assert_ne!(
            first, changed,
            "a changed source must not reuse a stale generated artifact"
        );
        assert_eq!(changed.parent(), Some(cache.as_path()));
    }

    #[test]
    fn cache_identity_changes_when_a_same_size_source_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let replacement = temp.path().join("replacement.bin");
        fs::write(&source, b"first").unwrap();
        let first = cache_key(&source, "preview");

        fs::write(&replacement, b"other").unwrap();
        fs::remove_file(&source).unwrap();
        fs::rename(&replacement, &source).unwrap();

        assert_ne!(first, cache_key(&source, "preview"));
    }

    #[test]
    fn image_thumbnails_cover_legacy_formats_and_cache_by_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        for extension in ["png", "webp"] {
            let source = temp.path().join(format!("photo.{extension}"));
            write_test_image(&source, 320, 180, [40, 120, 220, 255]);
            let first = service
                .thumbnail(source.clone(), 128)
                .wait()
                .unwrap()
                .unwrap();
            let cached = service
                .thumbnail(source.clone(), 128)
                .wait()
                .unwrap()
                .unwrap();
            assert_eq!(first, cached);
            assert_eq!(image::image_dimensions(&first).unwrap(), (128, 72));
            assert_eq!(&fs::read(&first).unwrap()[..8], b"\x89PNG\r\n\x1a\n");

            write_test_image(&source, 240, 240, [220, 80, 60, 255]);
            let changed = service.thumbnail(source, 128).wait().unwrap().unwrap();
            assert_ne!(changed, first);
            assert_eq!(image::image_dimensions(changed).unwrap(), (128, 128));
        }
    }

    #[test]
    fn svg_preview_and_thumbnail_render_locally_at_bounded_dimensions() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("diagram.svg");
        fs::write(
            &source,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" viewBox="0 0 320 180"><rect width="320" height="180" fill="#183153"/><circle cx="160" cy="90" r="54" fill="#7cc7ff"/></svg>"##,
        )
        .unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let artifact = service.artifact(source.clone()).wait().unwrap();
        assert_eq!(artifact.kind, "image");
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(
            image::image_dimensions(&artifact.path).unwrap(),
            (2_048, 1_152)
        );

        let thumbnail = service.thumbnail(source, 128).wait().unwrap().unwrap();
        assert_eq!(image::image_dimensions(thumbnail).unwrap(), (128, 72));
    }

    #[test]
    fn extended_raster_images_convert_without_external_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        for extension in ["tiff", "qoi"] {
            let source = temp.path().join(format!("photo.{extension}"));
            write_test_image(&source, 320, 180, [40, 120, 220, 255]);
            let artifact = service.artifact(source).wait().unwrap();
            assert_eq!(artifact.kind, "image");
            assert_eq!(artifact.mime_type, "image/png");
            assert_eq!(image::image_dimensions(artifact.path).unwrap(), (320, 180));
        }
    }

    #[test]
    fn photoshop_previews_and_thumbnails_decode_without_external_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("design.psd");
        fs::write(&source, minimal_psd(320, 180, [40, 120, 220, 255])).unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let detection = service.detect(source.clone()).wait().unwrap();
        assert_eq!(detection.kind, DetectedPreviewKind::Image);
        assert_eq!(
            detection.mime_type.as_deref(),
            Some("image/vnd.adobe.photoshop")
        );

        let artifact = service.artifact(source.clone()).wait().unwrap();
        assert_eq!(artifact.kind, "image");
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.tool, "Explorie PSD decoder");
        assert_eq!(image::image_dimensions(artifact.path).unwrap(), (320, 180));

        let thumbnail = service.thumbnail(source, 128).wait().unwrap().unwrap();
        assert_eq!(image::image_dimensions(thumbnail).unwrap(), (128, 72));
    }

    #[test]
    fn malformed_and_oversized_svg_previews_fail_recoverably() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let malformed = temp.path().join("malformed.svg");
        fs::write(&malformed, "<svg><broken>").unwrap();
        assert_eq!(
            service.artifact(malformed).wait().unwrap_err().code,
            ErrorCode::InvalidInput
        );

        let oversized = temp.path().join("oversized.svg");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_SVG_BYTES + 1)
            .unwrap();
        assert_eq!(
            service.artifact(oversized).wait().unwrap_err().code,
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn preview_detection_uses_signatures_for_misnamed_files() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("misnamed.bin");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([20, 80, 180, 255]))
            .save_with_format(&image, image::ImageFormat::Png)
            .unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let detected = service.detect(image).wait().unwrap();
        assert_eq!(detected.kind, DetectedPreviewKind::Image);
        assert_eq!(detected.mime_type.as_deref(), Some("image/png"));

        let qoi = temp.path().join("also-misnamed.data");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([180, 80, 20, 255]))
            .save_with_format(&qoi, image::ImageFormat::Qoi)
            .unwrap();
        let artifact = service.artifact(qoi).wait().unwrap();
        assert_eq!(artifact.mime_type, "image/png");
        assert!(artifact.path.is_file());

        let audio = temp.path().join("track.data");
        fs::write(&audio, b"fLaC\0\0\0\x22").unwrap();
        assert_eq!(
            service.detect(audio).wait().unwrap().kind,
            DetectedPreviewKind::Audio
        );
    }

    #[test]
    fn fallback_archive_extensions_reach_archive_preview() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        for name in [
            "disk.iso",
            "backup.wim",
            "bundle.cab",
            "notes.xz",
            "notes.bz2",
        ] {
            let path = temp.path().join(name);
            fs::write(&path, b"\0\x01\x02\x03").unwrap();
            assert_eq!(
                service.detect(path).wait().unwrap().kind,
                DetectedPreviewKind::Archive
            );
        }
    }

    #[test]
    fn text_detection_and_preview_support_utf16_bom() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("notes.data");
        let mut bytes = vec![0xff, 0xfe];
        for word in "hello from utf16".encode_utf16() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        fs::write(&source, bytes).unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        assert_eq!(
            service.detect(source.clone()).wait().unwrap().kind,
            DetectedPreviewKind::Text
        );
        assert_eq!(
            service.read_text(source, 1024).wait().unwrap().text,
            "hello from utf16"
        );
    }

    #[test]
    fn unknown_binary_detection_returns_a_bounded_hex_sample() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("opaque.bin");
        fs::write(&source, (0_u8..=255).collect::<Vec<_>>()).unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let detected = service.detect(source).wait().unwrap();
        assert_eq!(detected.kind, DetectedPreviewKind::Unknown);
        let sample = detected.byte_sample.expect("hex preview");
        assert!(sample.starts_with("00000000"));
        assert_eq!(sample.lines().count(), HEX_PREVIEW_BYTES / 16);
    }

    #[test]
    fn video_thumbnail_uses_real_ffmpeg_when_available() {
        let Some(ffmpeg) = first_available_tool(&["ffmpeg"], "-version") else {
            eprintln!("skipping video thumbnail test: FFmpeg unavailable");
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("clip.mp4");
        let generated = command(&ffmpeg)
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=15",
                "-t",
                "1",
                "-c:v",
                "mpeg4",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(generated.success());

        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let thumbnail = service.thumbnail(source, 128).wait().unwrap().unwrap();
        let (width, height) = image::image_dimensions(&thumbnail).unwrap();
        assert!(width <= 128 && height <= 128);
        assert_eq!(&fs::read(thumbnail).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_shell_icons_cover_regular_files_and_folders_and_reuse_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("notes.txt");
        let folder = temp.path().join("folder");
        fs::write(&source, "local").unwrap();
        fs::create_dir(&folder).unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let file_icon = service.file_icon(source).wait().unwrap().unwrap();
        let folder_icon = service.file_icon(folder.clone()).wait().unwrap().unwrap();
        let cached_folder_icon = service.file_icon(folder).wait().unwrap().unwrap();

        assert_eq!(&fs::read(&file_icon).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&fs::read(&folder_icon).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(folder_icon, cached_folder_icon);
    }

    fn os(values: &[&str]) -> Vec<std::ffi::OsString> {
        values.iter().map(Into::into).collect()
    }

    #[test]
    fn imagemagick_commands_pin_the_decoder_limits_and_policy() {
        for (name, coder) in [
            ("photo.HEIC", "HEIC"),
            ("photo.heif", "HEIC"),
            ("photo.avif", "AVIF"),
            ("photo.jpegxl", "JXL"),
            ("raw.cr2", "CR2"),
            ("raw.CR3", "CR3"),
            ("raw.dng", "DNG"),
            ("raw.raf", "RAF"),
        ] {
            assert_eq!(imagemagick_coder(Path::new(name), None), Some(coder));
        }
        assert_eq!(
            imagemagick_coder(Path::new("misnamed.jpg"), Some("image/heif")),
            Some("HEIC")
        );
        for name in [
            "document.ps",
            "vector.svg",
            "script.mvg",
            "notes.txt",
            "noext",
        ] {
            assert_eq!(imagemagick_coder(Path::new(name), None), None, "{name}");
        }

        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("disguised postscript.cr2");
        let output = temp.path().join("thumb.png");
        let command = imagemagick_command(
            "magick",
            temp.path(),
            "CR2",
            &input,
            &["-thumbnail", "256x256>"],
            &output,
        )
        .unwrap();
        let arguments: Vec<_> = command.get_args().map(ToOwned::to_owned).collect();
        let mut expected = os(&[
            "-limit", "memory", "256MiB", "-limit", "map", "512MiB", "-limit", "disk", "1GiB",
            "-limit", "area", "128MP", "-limit", "width", "16KP", "-limit", "height", "16KP",
            "-limit", "time", "60",
        ]);
        expected.push(format!("CR2:{}", input.display()).into());
        expected.extend(os(&["-thumbnail", "256x256>"]));
        expected.push(format!("PNG:{}", output.display()).into());
        assert_eq!(arguments, expected);

        let config = temp.path().join("imagemagick");
        assert!(command.get_envs().any(
            |(key, value)| key == "MAGICK_CONFIGURE_PATH" && value == Some(config.as_os_str())
        ));
        let policy = fs::read_to_string(config.join("policy.xml")).unwrap();
        assert_eq!(policy, IMAGEMAGICK_POLICY);
        for denied in [
            r#"<policy domain="coder" rights="none" pattern="*"/>"#,
            r#"<policy domain="delegate" rights="none" pattern="*"/>"#,
            r#"<policy domain="path" rights="none" pattern="@*"/>"#,
            r#"<policy domain="resource" name="time" value="60"/>"#,
        ] {
            assert!(policy.contains(denied), "{denied}");
        }
        for module in [
            "PS", "EPS", "PDF", "XPS", "MVG", "MSL", "TEXT", "URL", "HTTP", "HTTPS", "FTP",
        ] {
            assert!(
                policy
                    .lines()
                    .filter(|line| line.contains(r#"domain="module" rights="none""#))
                    .any(|line| line.contains(&format!("{{{module},"))
                        || line.contains(&format!(",{module},"))
                        || line.contains(&format!(",{module}}}"))),
                "{module} is not denied"
            );
        }
        // The allow list must come after the blanket coder denial because the
        // last matching ImageMagick policy wins.
        let deny = policy.find(r#"domain="coder" rights="none""#).unwrap();
        assert!(policy.find(r#"domain="coder" rights="read""#).unwrap() > deny);
        assert!(
            policy
                .find(r#"domain="coder" rights="write" pattern="PNG""#)
                .unwrap()
                > deny
        );

        fs::write(config.join("policy.xml"), "<policymap/>").unwrap();
        prepare_imagemagick_config(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(config.join("policy.xml")).unwrap(),
            IMAGEMAGICK_POLICY
        );
    }

    #[test]
    fn libreoffice_uses_a_hardened_dedicated_profile() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("preview cache");
        let profile = prepare_libreoffice_profile(&cache).unwrap();
        assert_eq!(profile, cache.join("libreoffice-profile"));
        let registry = profile.join("user").join("registrymodifications.xcu");
        let seeded = fs::read_to_string(&registry).unwrap();
        for setting in [
            r#"<prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop>"#,
            r#"<prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop>"#,
            r#"<prop oor:name="SecureURL" oor:op="fuse"><value/></prop>"#,
            r#"<prop oor:name="DisableActiveContent" oor:op="fuse"><value>true</value></prop>"#,
        ] {
            assert!(seeded.contains(setting), "{setting}");
        }
        // Writer stores link updates as NEVER = 0; Calc uses 1 for never.
        assert!(seeded.contains(
            r#"<item oor:path="/org.openoffice.Office.Writer/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>0</value>"#
        ));
        assert!(seeded.contains(
            r#"<item oor:path="/org.openoffice.Office.Calc/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>1</value>"#
        ));

        // LibreOffice rewrites the file with its own additions; keep those as
        // long as every hardening entry survives, and repair it otherwise.
        let extended = seeded.replace(
            "</oor:items>",
            "<item oor:path=\"/org.openoffice.Setup/Office\"><prop oor:name=\"ooSetupInstCompleted\" oor:op=\"fuse\"><value>true</value></prop></item>\n</oor:items>",
        );
        fs::write(&registry, &extended).unwrap();
        prepare_libreoffice_profile(&cache).unwrap();
        assert_eq!(fs::read_to_string(&registry).unwrap(), extended);
        fs::write(
            &registry,
            extended.replace("<value>3</value>", "<value>0</value>"),
        )
        .unwrap();
        prepare_libreoffice_profile(&cache).unwrap();
        assert_eq!(fs::read_to_string(&registry).unwrap(), LIBREOFFICE_REGISTRY);

        let output = temp.path().join("out");
        let input = temp.path().join("report.docx");
        let arguments = libreoffice_arguments(&profile, &output, &input);
        let mut expected = vec![std::ffi::OsString::from(format!(
            "-env:UserInstallation={}",
            file_url(&profile)
        ))];
        expected.extend(os(&[
            "--headless",
            "--norestore",
            "--nolockcheck",
            "--nodefault",
            "--nologo",
            "--convert-to",
            "pdf",
            "--outdir",
        ]));
        expected.push(output.into());
        expected.push(input.into());
        assert_eq!(arguments, expected);
        assert!(file_url(&profile).contains("/preview%20cache/libreoffice-profile"));
    }

    #[test]
    fn file_urls_percent_encode_profile_paths() {
        #[cfg(unix)]
        {
            assert_eq!(
                file_url(Path::new("/Users/Zoë Q/Caches/x#1%/lo")),
                "file:///Users/Zo%C3%AB%20Q/Caches/x%231%25/lo"
            );
            assert_eq!(file_url(Path::new("/tmp/a\\b")), "file:///tmp/a%5Cb");
        }
        #[cfg(windows)]
        assert_eq!(
            file_url(Path::new(r"C:\Users\Zoë\AppData\lo")),
            "file:///C:/Users/Zo%C3%AB/AppData/lo"
        );
    }

    #[test]
    fn ffmpeg_frame_arguments_disable_stdin_and_network_protocols() {
        let input = Path::new("/videos/clip.mkv");
        let output = Path::new("/cache/frame.png");
        let arguments = ffmpeg_frame_arguments(input, "thumbnail", output);
        let mut expected = os(&[
            "-nostdin",
            "-hide_banner",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
            "-i",
        ]);
        expected.push(input.into());
        expected.extend(os(&["-frames:v", "1", "-vf", "thumbnail"]));
        expected.push(output.into());
        assert_eq!(arguments, expected);
    }

    static FORCED_CLOUD_PLACEHOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Whether a test marked `path` as a cloud placeholder.
    pub(super) fn forced_cloud_placeholder(path: &Path) -> bool {
        FORCED_CLOUD_PLACEHOLDERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|forced| forced == path)
    }

    fn force_cloud_placeholder(path: &Path) {
        FORCED_CLOUD_PLACEHOLDERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_path_buf());
    }

    fn cached_files_containing(cache: &Path, marker: &str) -> usize {
        fs::read_dir(cache).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(marker))
                .count()
        })
    }

    #[test]
    fn thumbnails_prefer_bundled_decoders_and_leave_documents_to_quick_look() {
        for (name, expected) in [
            ("photo.JPG", Some(ThumbnailSource::NativeImage)),
            ("scan.tiff", Some(ThumbnailSource::NativeImage)),
            ("design.psd", Some(ThumbnailSource::Psd)),
            ("diagram.svg", Some(ThumbnailSource::Svg)),
            ("photo.heic", Some(ThumbnailSource::ExternalImage)),
            ("raw.cr2", Some(ThumbnailSource::ExternalImage)),
            ("clip.mov", Some(ThumbnailSource::Video)),
            ("notes.txt", None),
        ] {
            assert_eq!(thumbnail_source(Path::new(name)), expected, "{name}");
        }
        for name in [
            "manual.pdf",
            "letter.docx",
            "report.pages",
            "font.otf",
            "scene.usdz",
        ] {
            let expected = cfg!(target_os = "macos").then_some(ThumbnailSource::QuickLook);
            assert_eq!(thumbnail_source(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn models_render_only_with_the_3d_backend_and_otherwise_keep_their_icon() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let scene = temp.path().join("triangle.obj");
        fs::write(&scene, "v -1 -1 0\nv 1 -1 0\nv 0 1 0\nf 1 2 3\n").unwrap();

        let thumbnail = service.thumbnail(scene.clone(), 128).wait().unwrap();
        assert_eq!(thumbnail.is_some(), cfg!(feature = "preview-3d"));
        let preview = service
            .model(scene, ModelCamera::default(), 480, 320)
            .wait();
        if cfg!(feature = "preview-3d") {
            assert_eq!(preview.unwrap().triangle_count, 1);
        } else {
            let error = preview.unwrap_err();
            assert_eq!(error.code, ErrorCode::Unsupported);
            assert_eq!(error.message, "3D previews aren't included in this build");
        }
    }

    #[test]
    fn cloud_placeholders_never_get_thumbnails() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let photo = temp.path().join("evicted.png");
        write_test_image(&photo, 320, 180, [40, 120, 220, 255]);
        let scene = temp.path().join("evicted.obj");
        fs::write(&scene, b"not a model").unwrap();
        force_cloud_placeholder(&photo);
        force_cloud_placeholder(&scene);

        assert_eq!(service.thumbnail(photo, 128).wait().unwrap(), None);
        assert_eq!(service.thumbnail(scene, 128).wait().unwrap(), None);
        assert_eq!(
            cached_files_containing(&service.cache_dir(), "-thumbnail-"),
            0
        );

        let local = temp.path().join("local.png");
        write_test_image(&local, 320, 180, [40, 120, 220, 255]);
        assert!(service.thumbnail(local, 128).wait().unwrap().is_some());
    }

    #[cfg(target_os = "macos")]
    fn assert_png_within(path: &Path, limit: u32) -> (u32, u32) {
        assert_eq!(&fs::read(path).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
        let (width, height) = image::image_dimensions(path).unwrap();
        assert!(width > 0 && height > 0 && width <= limit && height <= limit);
        (width, height)
    }

    /// Converts a PNG to HEIC with the system `sips` tool.
    #[cfg(target_os = "macos")]
    fn write_test_heic(directory: &Path, name: &str) -> PathBuf {
        write_test_heic_sized(directory, name, 320, 180)
    }

    #[cfg(target_os = "macos")]
    fn write_test_heic_sized(directory: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let png = directory.join(format!("{name}.png"));
        // A gradient, so the encoder produces real image data at any size.
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x / 64 + y / 64) % 256) as u8,
            ])
        })
        .save(&png)
        .unwrap();
        let heic = directory.join(format!("{name}.heic"));
        let converted = Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&png)
            .arg("--out")
            .arg(&heic)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(converted.success(), "sips could not create a HEIC");
        fs::remove_file(png).unwrap();
        heic
    }

    /// A short MPEG-4 clip transcoded with `avconvert` from a movie macOS
    /// ships, or `None` when neither is available.
    #[cfg(target_os = "macos")]
    fn write_test_video(directory: &Path) -> Option<PathBuf> {
        let source = [
            "/System/Applications/FindMy.app/Contents/Resources/BatterySwap_loop.mov",
            "/System/Library/CoreServices/ControlCenter.app/Contents/Resources/BentoGalleryIntroduction.mov",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())?;
        let clip = directory.join("clip.mp4");
        let converted = Command::new("avconvert")
            .args([
                "--preset",
                "PresetLowQuality",
                "--duration",
                "1",
                "--source",
            ])
            .arg(source)
            .arg("--output")
            .arg(&clip)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        (converted.success() && clip.is_file()).then_some(clip)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_thumbnails_cover_heic_pdf_and_video_without_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        let heic = write_test_heic(temp.path(), "photo");
        let thumbnail = service
            .thumbnail(heic.clone(), 128)
            .wait()
            .unwrap()
            .unwrap();
        assert_eq!(assert_png_within(&thumbnail, 128), (128, 72));
        assert_eq!(
            service.thumbnail(heic, 128).wait().unwrap().unwrap(),
            thumbnail,
            "system thumbnails are cached like native ones"
        );

        let pdf = temp.path().join("manual.pdf");
        fs::write(&pdf, minimal_pdf_with_dimensions(1, 400, 200)).unwrap();
        let thumbnail = service.thumbnail(pdf, 256).wait().unwrap().unwrap();
        let (width, height) = assert_png_within(&thumbnail, 256);
        assert!(width > height, "landscape page stays landscape");

        match write_test_video(temp.path()) {
            Some(video) => {
                let thumbnail = service.thumbnail(video, 128).wait().unwrap().unwrap();
                assert_png_within(&thumbnail, 128);
            }
            None => eprintln!("skipping video poster frame: no sample movie or avconvert"),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_waits_are_bounded_and_cancellable() {
        let temp = tempfile::tempdir().unwrap();
        let pdf = temp.path().join("slow.pdf");
        fs::write(&pdf, minimal_pdf(3)).unwrap();
        let output = temp.path().join("slow.png");

        assert_eq!(
            macos::quicklook_thumbnail(&pdf, 256, Duration::ZERO, &output, None),
            Err(macos::NativeImageError::TimedOut)
        );
        assert!(!output.exists(), "a late result must not be written");
        let service_error =
            quicklook_error(macos::NativeImageError::TimedOut, Duration::from_secs(5));
        assert!(service_error.message.contains("timed out after 5 seconds"));

        let generation = AtomicU64::new(2);
        assert_eq!(
            macos::quicklook_thumbnail(
                &pdf,
                256,
                QUICKLOOK_THUMBNAIL_TIMEOUT,
                &output,
                Some((&generation, 1))
            ),
            Err(macos::NativeImageError::Cancelled)
        );

        // The generator keeps working after abandoned requests. A busy test
        // machine can take longer than the production timeout to render it.
        macos::quicklook_thumbnail(&pdf, 256, Duration::from_secs(60), &output, None).unwrap();
        assert_png_within(&output, 256);

        let unsupported = temp.path().join("server.key");
        fs::write(
            &unsupported,
            "-----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        assert_eq!(
            macos::quicklook_thumbnail(
                &unsupported,
                256,
                QUICKLOOK_THUMBNAIL_TIMEOUT,
                &temp.path().join("key.png"),
                None
            ),
            Err(macos::NativeImageError::Unavailable),
            "files Quick Look can only draw as an icon get no thumbnail"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn large_heic_photos_preview_through_imageio_without_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        // Larger than the sizes Quick Look refuses for real photos, and
        // landscape so the fitted size shows the aspect ratio is kept.
        let heic = write_test_heic_sized(temp.path(), "wallpaper", 6_016, 4_000);

        let artifact = service.artifact(heic.clone()).wait().unwrap();
        assert_eq!(artifact.kind, "image");
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.tool, IMAGEIO_TOOL);
        assert_eq!(
            assert_png_within(&artifact.path, IMAGE_PREVIEW_DIMENSION),
            (IMAGE_PREVIEW_DIMENSION, 1_362)
        );
        assert!(artifact.path.starts_with(service.cache_dir()));
        assert_eq!(
            service.artifact(heic.clone()).wait().unwrap().path,
            artifact.path,
            "the decode is cached by source identity"
        );

        let thumbnail = service
            .thumbnail(heic.clone(), 256)
            .wait()
            .unwrap()
            .unwrap();
        assert_eq!(assert_png_within(&thumbnail, 256), (256, 170));

        // Quick Look, the fallback, answers at a size it accepts.
        let artifact = convert_quicklook_preview(&heic, &service.cache_dir(), None).unwrap();
        assert_eq!(artifact.tool, "Quick Look");
        let (width, height) = assert_png_within(&artifact.path, IMAGE_PREVIEW_DIMENSION);
        assert!(width > height, "landscape photo stays landscape");

        // Sources beyond the decode limits are refused from the header alone.
        let output = temp.path().join("refused.png");
        let limits = macos::SourceLimits {
            max_dimension: MAX_IMAGE_DECODE_DIMENSION,
            max_pixels: 1_000_000,
        };
        assert_eq!(
            macos::imageio_thumbnail(&heic, 256, limits, IMAGEIO_TIMEOUT, &output, None),
            Err(macos::NativeImageError::TooLarge)
        );
        assert!(!output.exists());
        let generation = AtomicU64::new(2);
        assert_eq!(
            macos::imageio_thumbnail(
                &heic,
                256,
                macos::SourceLimits {
                    max_dimension: MAX_IMAGE_DECODE_DIMENSION,
                    max_pixels: MAX_IMAGEIO_SOURCE_PIXELS,
                },
                IMAGEIO_TIMEOUT,
                &output,
                Some((&generation, 1))
            ),
            Err(macos::NativeImageError::Cancelled)
        );
        let text = temp.path().join("notes.heic");
        fs::write(&text, "not an image").unwrap();
        assert_eq!(
            imageio_image(&text, &output, 256, None).unwrap_err().code,
            ErrorCode::Unsupported
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_previews_retry_smaller_sizes_it_accepts() {
        use macos::NativeImageError::{Cancelled, TimedOut, Unavailable};

        let run = |outcomes: &[Result<(), macos::NativeImageError>]| {
            let mut tried = Vec::new();
            let result =
                quicklook_with_smaller_retries(Duration::from_secs(15), |size, timeout| {
                    assert!(timeout <= Duration::from_secs(15));
                    tried.push(size);
                    outcomes[tried.len() - 1]
                });
            (result, tried)
        };
        assert_eq!(
            run(&[Err(Unavailable), Ok(())]),
            (Ok(()), vec![IMAGE_PREVIEW_DIMENSION, 1_600])
        );
        assert_eq!(
            run(&[Err(Unavailable); 3]),
            (Err(Unavailable), QUICKLOOK_PREVIEW_DIMENSIONS.to_vec()),
            "files Quick Look cannot draw at any size keep failing"
        );
        assert_eq!(
            run(&[Err(TimedOut)]),
            (Err(TimedOut), vec![IMAGE_PREVIEW_DIMENSION])
        );
        assert_eq!(
            run(&[Err(Unavailable), Err(Cancelled)]),
            (Err(Cancelled), vec![IMAGE_PREVIEW_DIMENSION, 1_600])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_previews_stand_in_for_missing_helpers() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));

        // ImageIO decodes HEIC before any helper is needed, and Quick Look
        // still renders it when ImageIO cannot.
        let heic = write_test_heic(temp.path(), "preview");
        let artifact = service.artifact(heic.clone()).wait().unwrap();
        assert_eq!(artifact.kind, "image");
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.tool, IMAGEIO_TOOL);
        assert_eq!(
            assert_png_within(&artifact.path, IMAGE_PREVIEW_DIMENSION),
            (320, 180)
        );
        let artifact = convert_quicklook_preview(&heic, &service.cache_dir(), None).unwrap();
        assert_eq!(artifact.tool, "Quick Look");
        assert_eq!(
            assert_png_within(&artifact.path, IMAGE_PREVIEW_DIMENSION),
            (320, 180)
        );

        let text = temp.path().join("letter.txt");
        fs::write(&text, "Dear reader,\nQuick Look renders this letter.\n").unwrap();
        let docx = temp.path().join("letter.docx");
        let converted = Command::new("textutil")
            .args(["-convert", "docx"])
            .arg(&text)
            .arg("-output")
            .arg(&docx)
            .status()
            .unwrap();
        assert!(converted.success());
        let artifact = service.artifact(docx).wait().unwrap();
        if first_available_tool(&["soffice", "libreoffice"], "--version").is_none() {
            assert_eq!(artifact.kind, "image");
            assert_eq!(artifact.tool, "Quick Look");
            // Document generators decide how large a page thumbnail they render
            // (CI runners return far smaller pages than a desktop Mac), so only
            // check that a portrait page, not a square icon, came back.
            let (width, height) = assert_png_within(&artifact.path, IMAGE_PREVIEW_DIMENSION);
            assert!(height > width, "the first page renders in portrait");
        }

        // Files Quick Look cannot draw keep the helper's guidance.
        let broken = temp.path().join("broken.docx");
        fs::write(&broken, b"not a document").unwrap();
        if first_available_tool(&["soffice", "libreoffice"], "--version").is_none() {
            let error = service.artifact(broken).wait().unwrap_err();
            assert_eq!(error.code, ErrorCode::HelperMissing);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_icons_render_apps_folders_and_file_kinds() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let notes = temp.path().join("notes.txt");
        let other_notes = temp.path().join("other.TXT");
        fs::write(&notes, "local").unwrap();
        fs::write(&other_notes, "local").unwrap();
        let text_edit = PathBuf::from("/System/Applications/TextEdit.app");

        let app_icon = service
            .file_icon(text_edit.clone())
            .wait()
            .unwrap()
            .unwrap();
        let folder_icon = service.file_icon(folder).wait().unwrap().unwrap();
        let text_icon = service.file_icon(notes).wait().unwrap().unwrap();
        for icon in [&app_icon, &folder_icon, &text_icon] {
            assert_eq!(&fs::read(icon).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(
                image::image_dimensions(icon).unwrap(),
                (SYSTEM_ICON_PIXELS, SYSTEM_ICON_PIXELS)
            );
        }
        let app = image::open(&app_icon).unwrap().into_rgba8();
        assert!(
            app.pixels().any(|pixel| pixel.0[3] == 0),
            "icons keep transparency"
        );
        assert!(app.pixels().any(|pixel| pixel.0[3] == 255));
        assert_ne!(
            fs::read(&app_icon).unwrap(),
            fs::read(&folder_icon).unwrap()
        );
        assert_ne!(
            fs::read(&folder_icon).unwrap(),
            fs::read(&text_icon).unwrap()
        );

        // Plain files share their kind's icon; applications keep their own.
        assert_eq!(
            service.file_icon(other_notes).wait().unwrap().unwrap(),
            text_icon
        );
        assert!(text_icon.to_string_lossy().contains("-kind-icon."));
        assert!(!app_icon.to_string_lossy().contains("-kind-icon."));
        assert_eq!(
            service
                .file_icon(text_edit.clone())
                .wait()
                .unwrap()
                .unwrap(),
            app_icon
        );

        // A link shows its target's icon with Finder's alias arrow.
        let link = temp.path().join("Editor");
        std::os::unix::fs::symlink(&text_edit, &link).unwrap();
        let link_icon = service.file_icon(link).wait().unwrap().unwrap();
        assert!(!link_icon.to_string_lossy().contains("-kind-icon."));
        assert_eq!(
            image::image_dimensions(&link_icon).unwrap(),
            (SYSTEM_ICON_PIXELS, SYSTEM_ICON_PIXELS)
        );
        assert_ne!(fs::read(link_icon).unwrap(), fs::read(&app_icon).unwrap());

        assert_eq!(
            service
                .file_icon(temp.path().join("missing"))
                .wait()
                .unwrap(),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn item_specific_macos_icons_cover_packages_links_aliases_and_special_folders() {
        let temp = tempfile::tempdir().unwrap();
        let app =
            explorie_core::entry_for_path(Path::new("/System/Applications/TextEdit.app")).unwrap();
        assert!(app.is_package && has_item_specific_icon(&app));

        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        assert!(!has_item_specific_icon(
            &explorie_core::entry_for_path(&folder).unwrap()
        ));

        let home = dirs::home_dir().unwrap();
        for special in ["Desktop", "Documents", "Downloads", "Library"] {
            assert!(is_special_folder(&home.join(special)), "{special}");
        }
        assert!(is_special_folder(&home));
        assert!(is_special_folder(Path::new("/Applications")));
        assert!(is_special_folder(Path::new("/Volumes/Backup")));
        assert!(!is_special_folder(&home.join("Desktop/Projects")));
        assert!(!is_special_folder(&folder));

        let mut entry = explorie_core::entry_for_path(&temp.path().join("folder")).unwrap();
        entry.is_dir = false;
        for (name, has_xattrs, expected) in [
            ("Report alias", true, true),
            ("Report.pdf alias", true, true),
            ("Report alias", false, false),
            ("notes.txt", true, false),
        ] {
            entry.path = temp.path().join(name);
            entry.has_xattrs = has_xattrs;
            assert_eq!(
                has_item_specific_icon(&entry),
                expected,
                "{name} {has_xattrs}"
            );
        }
        entry.path = temp.path().join("notes.txt");
        entry.is_symlink = true;
        assert!(has_item_specific_icon(&entry));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cloud_placeholder_packages_get_kind_icons_without_being_read() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let evicted = temp.path().join("Evicted.app");
        let local = temp.path().join("Local.app");
        fs::create_dir(&evicted).unwrap();
        fs::create_dir(&local).unwrap();
        force_cloud_placeholder(&evicted);

        let evicted_icon = service.file_icon(evicted).wait().unwrap().unwrap();
        let local_icon = service.file_icon(local).wait().unwrap().unwrap();
        assert!(evicted_icon.to_string_lossy().contains("-kind-icon."));
        assert!(!local_icon.to_string_lossy().contains("-kind-icon."));
    }

    #[test]
    fn external_extensions_have_named_routes() {
        assert!(is_external_document(Path::new("file.docx")));
        assert!(is_external_video(Path::new("file.mkv")));
        assert!(is_external_image(Path::new("file.heic")));
        assert!(is_svg_image(Path::new("file.svg")));
        assert!(is_native_image(Path::new("file.tiff")));
        assert!(!is_external_image(Path::new("file.png")));
    }

    #[test]
    #[ignore]
    fn benchmark_parallel_thumbnails() {
        let temp = tempfile::tempdir().unwrap();
        let service = PreviewService::new(ServiceContext::new(ResourcePaths::test(temp.path())));
        let mut sources = Vec::new();
        for index in 0..4_u32 {
            let source = temp.path().join(format!("source-{index}.png"));
            image::RgbaImage::from_fn(3_000, 2_000, |x, y| {
                image::Rgba([
                    ((x + index * 17) % 255) as u8,
                    ((y + index * 31) % 255) as u8,
                    ((x + y + index * 47) % 255) as u8,
                    255,
                ])
            })
            .save(&source)
            .unwrap();
            sources.push(source);
        }
        let serial_started = Instant::now();
        for source in &sources {
            assert!(
                service
                    .thumbnail(source.clone(), 256)
                    .wait()
                    .unwrap()
                    .unwrap()
                    .exists()
            );
        }
        let serial = serial_started.elapsed();
        service.clear_cache().wait().unwrap();

        let started = Instant::now();
        let workers = sources
            .into_iter()
            .map(|source| {
                let service = service.clone();
                std::thread::spawn(move || service.thumbnail(source, 256).wait().unwrap().unwrap())
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert!(worker.join().unwrap().exists());
        }
        eprintln!(
            "thumbnail batch: serial {serial:?}, parallel {:?}",
            started.elapsed()
        );
    }
}
