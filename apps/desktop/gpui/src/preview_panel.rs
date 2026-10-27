use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use explorie_native_services::{
    DetectedPreviewKind, ImageMetadata, PdfPagePreview, PreviewArtifact, PreviewDetection,
    RichPreview, ServiceError,
};
use gpui::Task;

use crate::text_highlight::TextPreviewDocument;

const TEXT_PREVIEW_BYTES: u64 = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewRoute {
    Text,
    Audio,
    Video,
    Pdf,
    BlockedScript,
    DirectImage,
    GeneratedArtifact,
    Model,
    Rich,
    Archive,
    External,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewTab {
    Preview,
    Metadata,
    CustomFields,
}

impl PreviewTab {
    pub const ALL: [Self; 3] = [Self::Preview, Self::Metadata, Self::CustomFields];

    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Metadata => "Metadata",
            Self::CustomFields => "Custom Fields",
        }
    }
}

#[derive(Clone, Debug)]
pub enum PreviewContent {
    Text(Arc<TextPreviewDocument>),
    Audio,
    Video,
    Pdf {
        page: PdfPagePreview,
        tool: Option<String>,
    },
    BlockedScript,
    Image(PathBuf),
    Artifact(PreviewArtifact),
    /// A 3D model; the window's media player holds the model and its frames.
    Model,
    Rich(RichPreview),
    Archive,
    /// The file's contents live in the cloud; reading them for a preview
    /// would download the file, so only its metadata is shown.
    CloudPlaceholder,
    Fallback {
        detection: PreviewDetection,
        error: Option<ServiceError>,
    },
}

#[derive(Clone, Debug)]
pub enum PreviewState {
    Closed,
    Loading {
        path: PathBuf,
    },
    Ready {
        path: PathBuf,
        content: PreviewContent,
    },
    Failed {
        path: PathBuf,
        error: ServiceError,
    },
}

/// How long keyboard-driven selection has to settle before a burst of
/// selection changes starts loading the preview.
pub const KEYBOARD_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(75);

/// Deferred preview loading while keyboard selection moves quickly.
#[derive(Default)]
pub struct PreviewDebounce {
    /// When the last keyboard-driven selection change happened.
    pub last_keyboard_change: Option<Instant>,
    /// Fires the deferred preview once the selection settles.
    pub task: Option<Task<()>>,
}

#[derive(Clone, Debug)]
pub enum PhotoMetadataState {
    Unavailable,
    Loading {
        path: PathBuf,
    },
    Ready {
        path: PathBuf,
        metadata: ImageMetadata,
    },
    Failed {
        path: PathBuf,
        error: ServiceError,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinderTagColor {
    pub name: &'static str,
    pub index: u8,
    pub rgb: u32,
}

pub const FINDER_TAG_COLORS: [FinderTagColor; 8] = [
    FinderTagColor {
        name: "None",
        index: 0,
        rgb: 0x8e8e93,
    },
    FinderTagColor {
        name: "Gray",
        index: 1,
        rgb: 0x8e8e93,
    },
    FinderTagColor {
        name: "Green",
        index: 2,
        rgb: 0x34c759,
    },
    FinderTagColor {
        name: "Purple",
        index: 3,
        rgb: 0xaf52de,
    },
    FinderTagColor {
        name: "Blue",
        index: 4,
        rgb: 0x007aff,
    },
    FinderTagColor {
        name: "Yellow",
        index: 5,
        rgb: 0xffcc00,
    },
    FinderTagColor {
        name: "Red",
        index: 6,
        rgb: 0xff3b30,
    },
    FinderTagColor {
        name: "Orange",
        index: 7,
        rgb: 0xff9500,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinderTag {
    pub raw: String,
    pub name: String,
    pub color_index: u8,
}

impl FinderTag {
    pub fn parse(raw: String) -> Self {
        let tag = explorie_core::FinderTag::from_raw(&raw);
        Self {
            raw,
            name: tag.name,
            color_index: tag.color,
        }
    }

    pub fn color(&self) -> FinderTagColor {
        finder_tag_color(self.color_index)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FinderTagEditor {
    pub input: String,
    pub color_index: u8,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FinderTagsState {
    pub path: Option<PathBuf>,
    pub tags: Vec<FinderTag>,
    pub loading: bool,
    pub saving: bool,
    pub error: Option<String>,
    pub editor: Option<FinderTagEditor>,
}

impl FinderTagsState {
    pub fn unavailable(&mut self) {
        *self = Self::default();
    }

    pub fn start_loading(&mut self, path: PathBuf) {
        *self = Self {
            path: Some(path),
            loading: true,
            ..Self::default()
        };
    }

    pub fn set_tags(&mut self, path: PathBuf, tags: Vec<String>) {
        *self = Self {
            path: Some(path),
            tags: tags.into_iter().map(FinderTag::parse).collect(),
            ..Self::default()
        };
    }

    pub fn begin_add(&mut self) {
        if !self.loading && !self.saving {
            self.editor = Some(FinderTagEditor::default());
            self.error = None;
        }
    }

    pub fn cancel_add(&mut self) {
        self.editor = None;
        self.error = None;
    }

    pub fn cycle_color(&mut self, offset: i8) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.color_index = (i16::from(editor.color_index) + i16::from(offset))
            .rem_euclid(FINDER_TAG_COLORS.len() as i16) as u8;
        editor.error = None;
    }

    pub fn candidate_tags(&self) -> Result<Vec<String>, String> {
        let editor = self
            .editor
            .as_ref()
            .ok_or_else(|| "Open the tag editor first".to_string())?;
        let name = editor.input.trim();
        if name.is_empty() {
            return Err("Enter a tag name".to_string());
        }
        if name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err("Tag names must be 1–80 printable characters".to_string());
        }
        let raw = if editor.color_index == 0 {
            name.to_string()
        } else {
            format!("{name}\n{}", editor.color_index)
        };
        if self.tags.iter().any(|tag| tag.raw == raw) {
            return Err("That Finder tag is already applied".to_string());
        }
        let mut tags = self
            .tags
            .iter()
            .map(|tag| tag.raw.clone())
            .collect::<Vec<_>>();
        tags.push(raw);
        Ok(tags)
    }

    pub fn tags_without(&self, raw: &str) -> Vec<String> {
        self.tags
            .iter()
            .filter(|tag| tag.raw != raw)
            .map(|tag| tag.raw.clone())
            .collect()
    }
}

pub fn finder_tag_color(index: u8) -> FinderTagColor {
    FINDER_TAG_COLORS
        .get(usize::from(index))
        .copied()
        .unwrap_or(FINDER_TAG_COLORS[0])
}

impl PhotoMetadataState {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Unavailable => None,
            Self::Loading { path } | Self::Ready { path, .. } | Self::Failed { path, .. } => {
                Some(path)
            }
        }
    }
}

impl PreviewState {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Closed => None,
            Self::Loading { path } | Self::Ready { path, .. } | Self::Failed { path, .. } => {
                Some(path)
            }
        }
    }
}

pub fn cache_backed_preview_path(
    state: &PreviewState,
    preview_executable_scripts: bool,
) -> Option<&Path> {
    let path = state.path()?;
    // Large direct images display a downscaled copy from the preview cache.
    let downscaled_image = matches!(
        state,
        PreviewState::Ready {
            content: PreviewContent::Image(image),
            ..
        } if image != path
    );
    (downscaled_image
        || matches!(
            route(path, preview_executable_scripts),
            PreviewRoute::Pdf | PreviewRoute::GeneratedArtifact | PreviewRoute::Rich
        ))
    .then_some(path)
}

pub fn cache_reload_path(
    state: &PreviewState,
    current_generation: u64,
    clear_generation: u64,
    cleared_path: Option<&Path>,
    preview_executable_scripts: bool,
) -> Option<PathBuf> {
    if current_generation != clear_generation {
        return None;
    }
    let current_path = cache_backed_preview_path(state, preview_executable_scripts)?;
    (Some(current_path) == cleared_path).then(|| current_path.to_path_buf())
}

pub fn text_preview_bytes() -> u64 {
    TEXT_PREVIEW_BYTES
}

pub fn route(path: &Path, preview_executable_scripts: bool) -> PreviewRoute {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_executable_script(path) && !preview_executable_scripts {
        PreviewRoute::BlockedScript
    } else if extension == "pdf" {
        PreviewRoute::Pdf
    } else if matches!(
        extension.as_str(),
        "glb" | "gltf" | "obj" | "stl" | "ply" | "3mf" | "fbx"
    ) {
        PreviewRoute::Model
    } else if matches!(
        extension.as_str(),
        "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "eml"
            | "epub"
            | "cbz"
            | "sqlite"
            | "sqlite3"
            | "db"
            | "parquet"
            | "arrow"
            | "feather"
            | "ipc"
            | "md"
            | "markdown"
            | "html"
            | "htm"
            | "xhtml"
    ) {
        PreviewRoute::Rich
    } else if matches!(
        extension.as_str(),
        "mp3"
            | "wav"
            | "flac"
            | "ogg"
            | "opus"
            | "oga"
            | "m4a"
            | "m4b"
            | "aac"
            | "aif"
            | "aiff"
            | "caf"
            | "alac"
    ) {
        PreviewRoute::Audio
    } else if matches!(
        extension.as_str(),
        "txt"
            | "log"
            | "csv"
            | "ini"
            | "cfg"
            | "conf"
            | "env"
            | "json"
            | "json5"
            | "jsonc"
            | "yaml"
            | "yml"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "pyw"
            | "cs"
            | "csharp"
            | "sql"
            | "xml"
            | "css"
            | "scss"
            | "sass"
            | "java"
            | "go"
            | "rs"
            | "c"
            | "cpp"
            | "cxx"
            | "cc"
            | "hpp"
            | "hh"
            | "rb"
            | "php"
            | "swift"
            | "kt"
            | "kts"
            | "sh"
            | "bash"
            | "zsh"
            | "toml"
            | "lock"
            | "ps1"
            | "psm1"
            | "bat"
            | "cmd"
            | "vue"
            | "svelte"
            | "astro"
            | "graphql"
            | "gql"
            | "proto"
            | "diff"
            | "patch"
            | "rst"
            | "adoc"
            | "asciidoc"
            | "org"
            | "tex"
            | "bib"
            | "properties"
            | "gradle"
            | "cmake"
            | "kdl"
            | "ron"
            | "hcl"
            | "tf"
            | "tfvars"
            | "ndjson"
            | "jsonl"
            | "xsl"
            | "xslt"
            | "fish"
            | "lua"
            | "dart"
            | "zig"
            | "r"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "fs"
            | "fsx"
            | "vb"
            | "scala"
            | "clj"
            | "cljs"
            | "groovy"
    ) || matches!(
        file_name.as_str(),
        "dockerfile"
            | "makefile"
            | "cmakelists.txt"
            | ".gitignore"
            | ".gitattributes"
            | ".editorconfig"
            | ".npmrc"
            | ".yarnrc"
            | ".prettierrc"
            | ".eslintrc"
            | "license"
            | "readme"
    ) {
        PreviewRoute::Text
    } else if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"
    ) {
        PreviewRoute::DirectImage
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
        PreviewRoute::Video
    } else if matches!(
        extension.as_str(),
        "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "rtf"
            | "heic"
            | "heif"
            | "avif"
            | "jxl"
            | "jpegxl"
            | "tif"
            | "tiff"
            | "psd"
            | "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "orf"
            | "rw2"
            | "raf"
            | "svg"
            | "svgz"
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
    ) {
        PreviewRoute::GeneratedArtifact
    } else if explorie_core::archive::is_archive(path) {
        PreviewRoute::Archive
    } else {
        PreviewRoute::External
    }
}

pub fn route_with_detection(
    path: &Path,
    preview_executable_scripts: bool,
    detection: &PreviewDetection,
) -> PreviewRoute {
    let hinted = route(path, preview_executable_scripts);
    if hinted == PreviewRoute::BlockedScript {
        return hinted;
    }
    if matches!(hinted, PreviewRoute::Model | PreviewRoute::Rich) {
        return hinted;
    }
    if cfg!(target_os = "macos") && renders_through_quick_look(path, detection) {
        return PreviewRoute::GeneratedArtifact;
    }
    match detection.kind {
        DetectedPreviewKind::Pdf => PreviewRoute::Pdf,
        DetectedPreviewKind::Svg => PreviewRoute::GeneratedArtifact,
        DetectedPreviewKind::Image => match detection.mime_type.as_deref() {
            Some("image/png" | "image/jpeg" | "image/gif" | "image/bmp" | "image/webp") => {
                PreviewRoute::DirectImage
            }
            _ => PreviewRoute::GeneratedArtifact,
        },
        DetectedPreviewKind::Audio => PreviewRoute::Audio,
        DetectedPreviewKind::Video => PreviewRoute::Video,
        DetectedPreviewKind::Text if hinted == PreviewRoute::External => PreviewRoute::Text,
        DetectedPreviewKind::Archive if hinted != PreviewRoute::GeneratedArtifact => {
            PreviewRoute::Archive
        }
        DetectedPreviewKind::Unknown => PreviewRoute::External,
        _ => hinted,
    }
}

/// Files the macOS preview service renders with Quick Look whose containers
/// signature sniffing misreads: camera RAW (TIFF, MPEG-4 or unrecognized
/// signatures), legacy Office (OLE compound files) and iWork documents (ZIP).
/// Keynote's `key` extension only counts for ZIP content, so PEM private keys
/// keep their text preview.
fn renders_through_quick_look(path: &Path, detection: &PreviewDetection) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf" => true,
        "doc" | "xls" | "ppt" => detection.kind == DetectedPreviewKind::Unknown,
        "pages" | "numbers" | "key" => detection.kind == DetectedPreviewKind::Archive,
        _ => false,
    }
}

pub fn is_executable_script(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "ps1" | "psm1" | "bat" | "cmd"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_native_and_helper_backed_preview_types() {
        for name in [
            "disk.iso",
            "backup.WIM",
            "installer.msi",
            "bundle.cab",
            "notes.xz",
            "notes.bz2",
            "disk.dmg",
        ] {
            assert_eq!(
                route(Path::new(name), false),
                PreviewRoute::Archive,
                "{name}"
            );
        }
        assert_eq!(route(Path::new("code.rs"), false), PreviewRoute::Text);
        assert_eq!(route(Path::new("song.flac"), false), PreviewRoute::Audio);
        assert_eq!(route(Path::new("scene.glb"), false), PreviewRoute::Model);
        assert_eq!(route(Path::new("message.eml"), false), PreviewRoute::Rich);
        assert_eq!(route(Path::new("book.epub"), false), PreviewRoute::Rich);
        assert_eq!(route(Path::new("data.parquet"), false), PreviewRoute::Rich);
        assert_eq!(route(Path::new("manual.pdf"), false), PreviewRoute::Pdf);
        assert_eq!(route(Path::new("component.tsx"), false), PreviewRoute::Text);
        assert_eq!(route(Path::new("Dockerfile"), false), PreviewRoute::Text);
        assert_eq!(
            route(Path::new("photo.png"), false),
            PreviewRoute::DirectImage
        );
        assert_eq!(route(Path::new("clip.mp4"), false), PreviewRoute::Video);
        assert_eq!(
            route(Path::new("diagram.svg"), false),
            PreviewRoute::GeneratedArtifact
        );
        assert_eq!(
            route(Path::new("design.psd"), false),
            PreviewRoute::GeneratedArtifact
        );
        assert_eq!(
            route(Path::new("bundle.tar.gz"), false),
            PreviewRoute::Archive
        );
        assert_eq!(
            route(Path::new("unknown.bin"), false),
            PreviewRoute::External
        );
    }

    #[test]
    fn quick_look_documents_reach_generated_previews_on_macos() {
        let detection = |kind, mime: &str| PreviewDetection {
            kind,
            description: String::new(),
            mime_type: Some(mime.to_string()),
            byte_sample: None,
        };
        let zip = detection(DetectedPreviewKind::Archive, "application/zip");
        let unknown = detection(DetectedPreviewKind::Unknown, "application/octet-stream");
        let tiff = detection(DetectedPreviewKind::Image, "image/tiff");
        let mp4 = detection(DetectedPreviewKind::Video, "video/mp4");
        let text = detection(DetectedPreviewKind::Text, "text/plain");
        let quick_look = if cfg!(target_os = "macos") {
            PreviewRoute::GeneratedArtifact
        } else {
            PreviewRoute::Archive
        };
        for name in ["report.pages", "budget.numbers", "deck.key"] {
            assert_eq!(
                route_with_detection(Path::new(name), false, &zip),
                quick_look,
                "{name}"
            );
        }
        assert_eq!(
            route_with_detection(Path::new("server.key"), false, &text),
            PreviewRoute::Text
        );
        assert_eq!(
            route_with_detection(Path::new("photo.nef"), false, &tiff),
            PreviewRoute::GeneratedArtifact
        );
        let expected = |fallback| {
            if cfg!(target_os = "macos") {
                PreviewRoute::GeneratedArtifact
            } else {
                fallback
            }
        };
        assert_eq!(
            route_with_detection(Path::new("photo.cr3"), false, &mp4),
            expected(PreviewRoute::Video)
        );
        for name in ["photo.rw2", "letter.doc", "budget.xls", "deck.ppt"] {
            assert_eq!(
                route_with_detection(Path::new(name), false, &unknown),
                expected(PreviewRoute::External),
                "{name}"
            );
        }
    }

    #[test]
    fn structured_archives_and_models_keep_their_specific_route_after_detection() {
        let archive = PreviewDetection {
            kind: DetectedPreviewKind::Archive,
            description: "ZIP archive".to_string(),
            mime_type: Some("application/zip".to_string()),
            byte_sample: None,
        };
        assert_eq!(
            route_with_detection(Path::new("book.epub"), false, &archive),
            PreviewRoute::Rich
        );
        assert_eq!(
            route_with_detection(Path::new("part.3mf"), false, &archive),
            PreviewRoute::Model
        );
    }

    #[test]
    fn every_legacy_text_extension_routes_to_the_native_reader() {
        for extension in [
            "txt",
            "log",
            "csv",
            "ini",
            "cfg",
            "conf",
            "env",
            "json",
            "json5",
            "jsonc",
            "yaml",
            "yml",
            "ts",
            "tsx",
            "js",
            "jsx",
            "mjs",
            "cjs",
            "py",
            "pyw",
            "cs",
            "csharp",
            "sql",
            "xml",
            "css",
            "scss",
            "sass",
            "java",
            "go",
            "rs",
            "c",
            "cpp",
            "cxx",
            "cc",
            "hpp",
            "hh",
            "rb",
            "php",
            "swift",
            "kt",
            "kts",
            "sh",
            "bash",
            "zsh",
            "toml",
            "lock",
            "vue",
            "svelte",
            "astro",
            "graphql",
            "gql",
            "proto",
            "diff",
            "patch",
            "rst",
            "adoc",
            "asciidoc",
            "org",
            "tex",
            "bib",
            "properties",
            "gradle",
            "cmake",
            "kdl",
            "ron",
            "hcl",
            "tf",
            "tfvars",
            "ndjson",
            "jsonl",
            "xsl",
            "xslt",
            "fish",
            "lua",
            "dart",
            "zig",
            "r",
            "ex",
            "exs",
            "erl",
            "hrl",
            "fs",
            "fsx",
            "vb",
            "scala",
            "clj",
            "cljs",
            "groovy",
        ] {
            assert_eq!(
                route(Path::new(&format!("preview.{extension}")), false),
                PreviewRoute::Text,
                "{extension} should use the native text reader"
            );
        }
        assert_eq!(
            route(Path::new("photo.webp"), false),
            PreviewRoute::DirectImage
        );
        for name in [
            "Dockerfile",
            "Makefile",
            "CMakeLists.txt",
            ".gitignore",
            ".editorconfig",
            "LICENSE",
        ] {
            assert_eq!(route(Path::new(name), false), PreviewRoute::Text);
        }
    }

    #[test]
    fn vector_and_extended_raster_images_use_local_generated_previews() {
        for extension in [
            "svg", "svgz", "tif", "tiff", "ico", "tga", "dds", "hdr", "pnm", "pbm", "pgm", "ppm",
            "pam", "qoi",
        ] {
            assert_eq!(
                route(Path::new(&format!("image.{extension}")), false),
                PreviewRoute::GeneratedArtifact,
                "{extension} should use a local generated preview"
            );
        }
    }

    #[test]
    fn supported_audio_stays_embedded_and_unavailable_decoders_fall_back_locally() {
        for extension in ["mp3", "wav", "flac", "ogg", "opus", "oga", "m4a", "aac"] {
            assert_eq!(
                route(Path::new(&format!("track.{extension}")), false),
                PreviewRoute::Audio
            );
        }
        assert_eq!(route(Path::new("track.wma"), false), PreviewRoute::External);
    }

    #[test]
    fn every_legacy_video_extension_routes_to_the_native_player() {
        for extension in [
            "mp4", "webm", "m4v", "mov", "avi", "mkv", "wmv", "flv", "m2ts", "mts", "mpeg", "mpg",
            "3gp",
        ] {
            assert_eq!(
                route(Path::new(&format!("clip.{extension}")), false),
                PreviewRoute::Video,
                "{extension} should use the native video player"
            );
        }
    }

    #[test]
    fn executable_scripts_follow_the_persisted_preview_setting() {
        assert_eq!(
            route(Path::new("cleanup.ps1"), false),
            PreviewRoute::BlockedScript
        );
        assert_eq!(route(Path::new("cleanup.ps1"), true), PreviewRoute::Text);
        assert!(is_executable_script(Path::new("install.CMD")));
        assert!(!is_executable_script(Path::new("script.sh")));
    }

    #[test]
    fn cache_reload_targets_only_the_same_open_cache_backed_preview() {
        let pdf = PreviewState::Loading {
            path: PathBuf::from("manual.pdf"),
        };
        let generated = PreviewState::Loading {
            path: PathBuf::from("design.psd"),
        };
        let direct = PreviewState::Loading {
            path: PathBuf::from("photo.png"),
        };

        assert_eq!(
            cache_backed_preview_path(&pdf, false),
            Some(Path::new("manual.pdf"))
        );
        assert_eq!(
            cache_backed_preview_path(&generated, false),
            Some(Path::new("design.psd"))
        );
        assert_eq!(cache_backed_preview_path(&direct, false), None);
        assert_eq!(
            cache_reload_path(&pdf, 7, 7, Some(Path::new("manual.pdf")), false),
            Some(PathBuf::from("manual.pdf"))
        );
        assert_eq!(
            cache_reload_path(&pdf, 8, 7, Some(Path::new("manual.pdf")), false),
            None,
            "navigation must invalidate a late cache-clear completion"
        );
        assert_eq!(
            cache_reload_path(&generated, 7, 7, Some(Path::new("manual.pdf")), false),
            None,
            "a different generated preview must not be reopened"
        );
        assert_eq!(
            cache_reload_path(&direct, 7, 7, Some(Path::new("photo.png")), false),
            None,
            "direct source images survive cache clearing without a reload"
        );
        let downscaled = PreviewState::Ready {
            path: PathBuf::from("photo.png"),
            content: PreviewContent::Image(PathBuf::from("cache/photo-native-image.png")),
        };
        assert_eq!(
            cache_reload_path(&downscaled, 7, 7, Some(Path::new("photo.png")), false),
            Some(PathBuf::from("photo.png")),
            "a downscaled copy lives in the cache and must be regenerated"
        );
    }

    #[test]
    fn finder_tags_preserve_raw_color_suffixes_and_validate_editor_input() {
        let mut state = FinderTagsState::default();
        state.set_tags(
            PathBuf::from("design.psd"),
            vec!["Important\n6".to_string(), "Plain".to_string()],
        );
        assert_eq!(state.tags[0].name, "Important");
        assert_eq!(state.tags[0].color().name, "Red");
        assert_eq!(state.tags[1].color_index, 0);
        assert_eq!(finder_tag_color(99).name, "None");

        state.begin_add();
        assert_eq!(state.candidate_tags().unwrap_err(), "Enter a tag name");
        state.editor.as_mut().unwrap().input = "Review".to_string();
        state.cycle_color(4);
        assert_eq!(
            state.candidate_tags().unwrap(),
            vec![
                "Important\n6".to_string(),
                "Plain".to_string(),
                "Review\n4".to_string()
            ]
        );
        assert_eq!(
            state.tags_without("Important\n6"),
            vec!["Plain".to_string()]
        );
    }

    #[test]
    fn finder_tag_editor_cycles_colors_and_rejects_duplicates_and_controls() {
        let mut state = FinderTagsState::default();
        state.set_tags(PathBuf::from("notes.txt"), vec!["Work\n7".to_string()]);
        state.begin_add();
        state.cycle_color(-1);
        assert_eq!(state.editor.as_ref().unwrap().color_index, 7);
        state.editor.as_mut().unwrap().input = "Work".to_string();
        assert_eq!(
            state.candidate_tags().unwrap_err(),
            "That Finder tag is already applied"
        );
        state.editor.as_mut().unwrap().input = "bad\nname".to_string();
        assert_eq!(
            state.candidate_tags().unwrap_err(),
            "Tag names must be 1–80 printable characters"
        );
        state.cancel_add();
        assert!(state.editor.is_none());
    }
}
