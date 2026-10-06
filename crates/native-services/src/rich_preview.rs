//! Structured previews for documents, fonts, databases and tables.
//!
//! Fonts, email, SQLite and Parquet/Arrow need optional backends (the
//! `preview-fonts`, `preview-mail`, `preview-sqlite` and `preview-columnar`
//! cargo features). A build without one still routes those files here and
//! answers with an Unsupported error that names what is missing.

use crate::preview::TextHighlight;
use crate::{ErrorCode, ServiceError, ServiceResult};
#[cfg(feature = "preview-columnar")]
use arrow_cast::display::array_value_to_string;
#[cfg(feature = "preview-fonts")]
use fontdue::{Font, FontSettings};
#[cfg(feature = "preview-mail")]
use mail_parser::MessageParser;
#[cfg(feature = "preview-columnar")]
use parquet::file::reader::{FileReader, SerializedFileReader};
#[cfg(feature = "preview-sqlite")]
use rusqlite::config::DbConfig;
#[cfg(feature = "preview-sqlite")]
use rusqlite::limits::Limit;
#[cfg(feature = "preview-sqlite")]
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(feature = "preview-sqlite")]
use std::time::{Duration, Instant};
use zip::ZipArchive;

const MAX_RICH_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TEXT_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_BLOCKS: usize = 240;
pub(crate) const MAX_BLOCK_TEXT: usize = 8 * 1024;
#[cfg(any(feature = "preview-sqlite", feature = "preview-columnar"))]
const MAX_TABLE_COLUMNS: usize = 16;
#[cfg(any(feature = "preview-sqlite", feature = "preview-columnar"))]
const MAX_TABLE_ROWS: usize = 24;
#[cfg(feature = "preview-sqlite")]
const SQLITE_PREVIEW_BUDGET: Duration = Duration::from_secs(3);
#[cfg(feature = "preview-sqlite")]
const SQLITE_PROGRESS_INTERVAL: i32 = 10_000;
#[cfg(feature = "preview-sqlite")]
const MAX_SQLITE_VALUE_BYTES: i32 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RichBlockKind {
    Heading,
    #[default]
    Paragraph,
    Metadata,
    TableHeader,
    TableRow,
    Code,
    ListItem,
    Rule,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RichBlock {
    pub kind: RichBlockKind,
    /// The block's plain text; for styled blocks, its spans' text joined.
    pub text: String,
    /// Inline runs and their styles, for sources that have them (Markdown).
    /// Empty when the block is plain `text`.
    pub spans: Vec<RichSpan>,
    /// A heading's level (1–6), or how deeply a list item or code block sits
    /// inside lists.
    pub level: u8,
    /// A list item's bullet, number or task box.
    pub marker: Option<String>,
    /// How many block quotes enclose the block.
    pub quote_depth: u8,
    /// A Markdown table row's cells. Other tables keep the row in `text`.
    pub cells: Vec<Vec<RichSpan>>,
    /// Syntax highlights within `text`, for code blocks.
    pub highlights: Vec<TextHighlight>,
}

/// A run of inline text with one style.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RichSpan {
    pub text: String,
    pub style: RichSpanStyle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RichSpanStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strikethrough: bool,
    pub code: bool,
    /// The destination of a link, shown but never fetched by the preview.
    pub link: Option<String>,
    /// An image's alt text, which stands in for the image.
    pub image: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RichPreview {
    pub title: String,
    pub subtitle: String,
    pub blocks: Vec<RichBlock>,
    pub image_path: Option<PathBuf>,
}

pub fn preview(path: &Path, cache: &Path) -> ServiceResult<RichPreview> {
    validate_source(path)?;
    match extension(path).as_str() {
        "ttf" | "otf" | "woff" | "woff2" => font_preview(path, cache),
        "eml" => email_preview(path),
        "epub" => ebook_preview(path, cache, false),
        "cbz" => ebook_preview(path, cache, true),
        "sqlite" | "sqlite3" | "db" => sqlite_preview(path),
        "parquet" => parquet_preview(path),
        "arrow" | "feather" | "ipc" => arrow_preview(path),
        "md" | "markdown" => markdown_preview(path),
        "html" | "htm" | "xhtml" => html_preview(path),
        _ => Err(ServiceError::new(
            ErrorCode::Unsupported,
            "No structured preview provider is available for this file type",
        )),
    }
}

fn validate_source(path: &Path) -> ServiceResult<()> {
    let metadata = fs::metadata(path).map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Structured preview requires a regular file",
        ));
    }
    if metadata.len() > MAX_RICH_SOURCE_BYTES {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Structured preview is limited to files no larger than 256 MiB",
        ));
    }
    Ok(())
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn bounded_read(path: &Path, limit: u64) -> ServiceResult<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(128 * 1024) as usize);
    File::open(path)
        .map_err(ServiceError::from)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ServiceError::from)?;
    if bytes.len() as u64 > limit {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "This document is too large for a safe structured preview",
        ));
    }
    Ok(bytes)
}

fn block(kind: RichBlockKind, text: impl Into<String>) -> RichBlock {
    let mut text = text.into();
    if text.len() > MAX_BLOCK_TEXT {
        text.truncate(MAX_BLOCK_TEXT);
        text.push('…');
    }
    RichBlock {
        kind,
        text,
        ..RichBlock::default()
    }
}

pub(crate) fn text_blocks(text: &str) -> Vec<RichBlock> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .take(MAX_BLOCKS)
        .map(|paragraph| block(RichBlockKind::Paragraph, paragraph))
        .collect()
}

fn markdown_preview(path: &Path) -> ServiceResult<RichPreview> {
    let bytes = bounded_read(path, MAX_TEXT_SOURCE_BYTES)?;
    let source = String::from_utf8_lossy(&bytes);
    let mut blocks = crate::rich_markdown::markdown_blocks(&source);
    // A leading top-level heading becomes the title rather than repeating
    // beneath it.
    let title = if blocks
        .first()
        .is_some_and(|block| block.kind == RichBlockKind::Heading && block.level == 1)
    {
        blocks.remove(0).text
    } else {
        file_title(path)
    };
    Ok(RichPreview {
        title,
        subtitle: "Rendered Markdown · scripts and network access disabled".to_string(),
        blocks,
        image_path: None,
    })
}

fn html_preview(path: &Path) -> ServiceResult<RichPreview> {
    let bytes = bounded_read(path, MAX_TEXT_SOURCE_BYTES)?;
    let source = String::from_utf8_lossy(&bytes);
    let rendered = html2text::from_read(source.as_bytes(), 100).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to render HTML preview: {error}"),
        )
    })?;
    let title = tag_value(&source, "title").unwrap_or_else(|| file_title(path));
    Ok(RichPreview {
        title,
        subtitle: "Rendered HTML · scripts, remote assets, and network access disabled".to_string(),
        blocks: text_blocks(&rendered),
        image_path: None,
    })
}

#[cfg(feature = "preview-mail")]
fn email_preview(path: &Path) -> ServiceResult<RichPreview> {
    let bytes = bounded_read(path, MAX_TEXT_SOURCE_BYTES)?;
    let message = MessageParser::default().parse(&bytes).ok_or_else(|| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            "Unable to parse this email message",
        )
    })?;
    let mut blocks = Vec::new();
    if let Some(from) = message.from().and_then(|address| address.first()) {
        blocks.push(block(
            RichBlockKind::Metadata,
            format_address("From", from.name(), from.address()),
        ));
    }
    if let Some(to) = message.to().and_then(|address| address.first()) {
        blocks.push(block(
            RichBlockKind::Metadata,
            format_address("To", to.name(), to.address()),
        ));
    }
    if let Some(date) = message.date() {
        blocks.push(block(RichBlockKind::Metadata, format!("Date · {date}")));
    }
    blocks.push(block(
        RichBlockKind::Metadata,
        format!("Attachments · {}", message.attachment_count()),
    ));
    if let Some(body) = message.body_text(0) {
        blocks.extend(text_blocks(&body));
    } else if let Some(body) = message.body_html(0) {
        let rendered = html2text::from_read(body.as_bytes(), 100).map_err(|error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Unable to render the email body: {error}"),
            )
        })?;
        blocks.extend(text_blocks(&rendered));
    }
    blocks.truncate(MAX_BLOCKS);
    Ok(RichPreview {
        title: message.subject().unwrap_or("Untitled email").to_string(),
        subtitle: "Email message · remote content disabled".to_string(),
        blocks,
        image_path: None,
    })
}

#[cfg(not(feature = "preview-mail"))]
fn email_preview(_path: &Path) -> ServiceResult<RichPreview> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "Email previews aren't included in this build",
    ))
}

#[cfg(feature = "preview-mail")]
fn format_address(label: &str, name: Option<&str>, address: Option<&str>) -> String {
    match (name, address) {
        (Some(name), Some(address)) => format!("{label} · {name} <{address}>"),
        (Some(name), None) => format!("{label} · {name}"),
        (None, Some(address)) => format!("{label} · {address}"),
        (None, None) => label.to_string(),
    }
}

fn ebook_preview(path: &Path, cache: &Path, comic: bool) -> ServiceResult<RichPreview> {
    let file = File::open(path).map_err(ServiceError::from)?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to open ebook archive: {error}"),
        )
    })?;
    let mut image_names = Vec::new();
    let mut chapter_names = Vec::new();
    let mut package_name = None;
    for index in 0..archive.len().min(20_000) {
        let entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().to_string();
        let lower = name.to_ascii_lowercase();
        if is_archive_image(&lower) {
            image_names.push(name.clone());
        }
        if matches_extension(&lower, &["html", "htm", "xhtml"]) {
            chapter_names.push(name.clone());
        }
        if lower.ends_with(".opf") && package_name.is_none() {
            package_name = Some(name);
        }
    }
    image_names.sort_by_key(|name| {
        let lower = name.to_ascii_lowercase();
        (!lower.contains("cover"), lower)
    });
    chapter_names.sort();
    let page_count = image_names.len();
    let image_path = image_names
        .first()
        .and_then(|name| extract_archive_image(&mut archive, name, path, cache).ok());

    let mut title = file_title(path);
    let mut creator = None;
    if let Some(package_name) = package_name
        && let Ok(package) = read_zip_string(&mut archive, &package_name, 2 * 1024 * 1024)
    {
        title = tag_value(&package, "dc:title").unwrap_or(title);
        creator = tag_value(&package, "dc:creator");
    }
    let mut blocks = vec![block(
        RichBlockKind::Metadata,
        if comic {
            format!("Pages · {page_count}")
        } else {
            format!("Illustrations · {page_count}")
        },
    )];
    if let Some(creator) = creator {
        blocks.push(block(
            RichBlockKind::Metadata,
            format!("Creator · {creator}"),
        ));
    }
    if !comic
        && let Some(chapter) = chapter_names
            .iter()
            .find(|name| !name.to_ascii_lowercase().contains("nav"))
        && let Ok(source) = read_zip_string(&mut archive, chapter, 2 * 1024 * 1024)
        && let Ok(rendered) = html2text::from_read(source.as_bytes(), 100)
    {
        blocks.extend(text_blocks(&rendered).into_iter().take(12));
    }
    Ok(RichPreview {
        title,
        subtitle: if comic {
            "Comic book archive".to_string()
        } else {
            "EPUB ebook".to_string()
        },
        blocks,
        image_path,
    })
}

fn zip_error(error: zip::result::ZipError) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, format!("Invalid archive: {error}"))
}

fn read_zip_string(
    archive: &mut ZipArchive<File>,
    name: &str,
    limit: u64,
) -> ServiceResult<String> {
    let mut entry = archive.by_name(name).map_err(zip_error)?;
    if entry.size() > limit {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Ebook entry exceeds the preview safety limit",
        ));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).map_err(ServiceError::from)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn extract_archive_image(
    archive: &mut ZipArchive<File>,
    name: &str,
    source: &Path,
    cache: &Path,
) -> ServiceResult<PathBuf> {
    let mut entry = archive.by_name(name).map_err(zip_error)?;
    if entry.size() > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Ebook cover exceeds the preview safety limit",
        ));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes).map_err(ServiceError::from)?;
    let image = image::load_from_memory(&bytes).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to decode ebook cover: {error}"),
        )
    })?;
    let image = image.thumbnail(1_000, 1_000).into_rgba8();
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache.join(format!("{}-book-cover.png", source_cache_key(source)));
    image.save(&output).map_err(|error| {
        ServiceError::new(
            ErrorCode::Internal,
            format!("Unable to cache ebook cover: {error}"),
        )
    })?;
    Ok(output)
}

/// Open an untrusted database so that its schema cannot run side-effecting
/// functions, nothing can be written, oversized values fail instead of being
/// materialized, and every statement is interrupted once `budget` elapses.
#[cfg(feature = "preview-sqlite")]
fn open_untrusted_sqlite(path: &Path, budget: Duration) -> ServiceResult<Connection> {
    let open_error = |error: rusqlite::Error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to open SQLite database read-only: {error}"),
        )
    };
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(open_error)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
        .map_err(open_error)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(open_error)?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(open_error)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(open_error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQLITE_VALUE_BYTES)
        .map_err(open_error)?;
    let deadline = Instant::now() + budget;
    connection
        .progress_handler(
            SQLITE_PROGRESS_INTERVAL,
            Some(move || Instant::now() >= deadline),
        )
        .map_err(open_error)?;
    Ok(connection)
}

#[cfg(feature = "preview-sqlite")]
fn sqlite_preview(path: &Path) -> ServiceResult<RichPreview> {
    let connection = open_untrusted_sqlite(path, SQLITE_PREVIEW_BUDGET)?;
    let mut statement = connection
        .prepare(
            "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY type, name LIMIT 100",
        )
        .map_err(sqlite_error)?;
    let objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    drop(statement);

    let mut blocks = vec![block(
        RichBlockKind::Metadata,
        format!("Tables and views · {}", objects.len()),
    )];
    for (name, kind) in objects.iter().take(30) {
        blocks.push(block(
            RichBlockKind::Metadata,
            format!("{} · {name}", uppercase_first(kind)),
        ));
    }
    if let Some((table, _)) = objects.iter().find(|(_, kind)| kind == "table") {
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!(
                "SELECT * FROM \"{escaped}\" LIMIT {MAX_TABLE_ROWS}"
            ))
            .map_err(sqlite_error)?;
        let columns = statement
            .column_names()
            .iter()
            .take(MAX_TABLE_COLUMNS)
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        blocks.push(block(RichBlockKind::Heading, format!("Sample · {table}")));
        blocks.push(block(RichBlockKind::TableHeader, columns.join("  ·  ")));
        let column_count = columns.len();
        let rows = statement
            .query_map([], |row| {
                (0..column_count)
                    .map(|index| row.get_ref(index).map(sqlite_value))
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(sqlite_error)?;
        for row in rows {
            blocks.push(block(
                RichBlockKind::TableRow,
                row.map_err(sqlite_error)?.join("  ·  "),
            ));
        }
    }
    Ok(RichPreview {
        title: file_title(path),
        subtitle: "SQLite database · opened read-only".to_string(),
        blocks,
        image_path: None,
    })
}

#[cfg(not(feature = "preview-sqlite"))]
fn sqlite_preview(_path: &Path) -> ServiceResult<RichPreview> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "SQLite previews aren't included in this build",
    ))
}

#[cfg(feature = "preview-sqlite")]
fn sqlite_error(error: rusqlite::Error) -> ServiceError {
    ServiceError::new(
        ErrorCode::InvalidInput,
        format!("Unable to inspect SQLite database: {error}"),
    )
}

#[cfg(feature = "preview-sqlite")]
fn sqlite_value(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(value) => value.to_string(),
        ValueRef::Real(value) => value.to_string(),
        ValueRef::Text(value) => truncate_cell(&String::from_utf8_lossy(value)),
        ValueRef::Blob(value) => format!("<{} bytes>", value.len()),
    }
}

#[cfg(feature = "preview-columnar")]
fn parquet_preview(path: &Path) -> ServiceResult<RichPreview> {
    let reader = SerializedFileReader::new(File::open(path).map_err(ServiceError::from)?).map_err(
        |error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Unable to open Parquet file: {error}"),
            )
        },
    )?;
    let metadata = reader.metadata().file_metadata();
    let mut blocks = vec![
        block(
            RichBlockKind::Metadata,
            format!("Rows · {}", metadata.num_rows()),
        ),
        block(
            RichBlockKind::Metadata,
            format!("Row groups · {}", reader.num_row_groups()),
        ),
    ];
    let columns = metadata
        .schema_descr()
        .columns()
        .iter()
        .take(MAX_TABLE_COLUMNS)
        .map(|column| format!("{} ({:?})", column.name(), column.physical_type()))
        .collect::<Vec<_>>();
    blocks.push(block(RichBlockKind::TableHeader, columns.join("  ·  ")));
    let rows = reader.get_row_iter(None).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to read Parquet rows: {error}"),
        )
    })?;
    for row in rows.take(MAX_TABLE_ROWS) {
        let row = row.map_err(|error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Unable to decode Parquet row: {error}"),
            )
        })?;
        blocks.push(block(
            RichBlockKind::TableRow,
            truncate_cell(&row.to_string()),
        ));
    }
    Ok(RichPreview {
        title: file_title(path),
        subtitle: "Apache Parquet table · bounded sample".to_string(),
        blocks,
        image_path: None,
    })
}

#[cfg(feature = "preview-columnar")]
fn arrow_preview(path: &Path) -> ServiceResult<RichPreview> {
    let file = File::open(path).map_err(ServiceError::from)?;
    let mut reader = arrow_ipc::reader::FileReader::try_new(file, None).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to open Arrow IPC file: {error}"),
        )
    })?;
    let schema = reader.schema();
    let columns = schema
        .fields()
        .iter()
        .take(MAX_TABLE_COLUMNS)
        .map(|field| format!("{} ({})", field.name(), field.data_type()))
        .collect::<Vec<_>>();
    let mut sample_rows = Vec::new();
    let mut batches = 0usize;
    for batch in (&mut reader).take(4) {
        let batch = batch.map_err(|error| {
            ServiceError::new(
                ErrorCode::InvalidInput,
                format!("Unable to read Arrow record batch: {error}"),
            )
        })?;
        batches += 1;
        for row_index in 0..batch.num_rows() {
            if sample_rows.len() >= MAX_TABLE_ROWS {
                break;
            }
            let values = batch
                .columns()
                .iter()
                .take(MAX_TABLE_COLUMNS)
                .map(|array| {
                    array_value_to_string(array.as_ref(), row_index)
                        .map(|value| truncate_cell(&value))
                        .unwrap_or_else(|_| "<?>".to_string())
                })
                .collect::<Vec<_>>();
            sample_rows.push(values.join("  ·  "));
        }
        if sample_rows.len() >= MAX_TABLE_ROWS {
            break;
        }
    }
    let mut blocks = vec![
        block(
            RichBlockKind::Metadata,
            format!("Rows displayed · {}", sample_rows.len()),
        ),
        block(
            RichBlockKind::Metadata,
            format!("Record batches sampled · {batches}"),
        ),
        block(RichBlockKind::TableHeader, columns.join("  ·  ")),
    ];
    blocks.extend(
        sample_rows
            .into_iter()
            .map(|row| block(RichBlockKind::TableRow, row)),
    );
    Ok(RichPreview {
        title: file_title(path),
        subtitle: "Apache Arrow IPC table · bounded sample".to_string(),
        blocks,
        image_path: None,
    })
}

#[cfg(not(feature = "preview-columnar"))]
fn parquet_preview(_path: &Path) -> ServiceResult<RichPreview> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "Parquet previews aren't included in this build",
    ))
}

#[cfg(not(feature = "preview-columnar"))]
fn arrow_preview(_path: &Path) -> ServiceResult<RichPreview> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "Arrow previews aren't included in this build",
    ))
}

#[cfg(feature = "preview-fonts")]
fn font_preview(path: &Path, cache: &Path) -> ServiceResult<RichPreview> {
    let bytes = bounded_read(path, 64 * 1024 * 1024)?;
    let bytes = match extension(path).as_str() {
        "woff" => wuff::decompress_woff1(&bytes).map_err(font_decode_error)?,
        "woff2" => wuff::decompress_woff2(&bytes).map_err(font_decode_error)?,
        _ => bytes,
    };
    let font = Font::from_bytes(bytes, FontSettings::default()).map_err(|error| {
        ServiceError::new(
            ErrorCode::InvalidInput,
            format!("Unable to parse font: {error}"),
        )
    })?;
    let family = font.name().unwrap_or("Untitled font").to_string();
    let width = 1_080_u32;
    let height = 640_u32;
    let mut image = image::RgbaImage::from_pixel(width, height, image::Rgba([19, 23, 30, 255]));
    draw_font_line(&mut image, &font, "Aa", 96.0, 64, 150);
    draw_font_line(&mut image, &font, &family, 50.0, 64, 300);
    draw_font_line(
        &mut image,
        &font,
        "The quick brown fox jumps over the lazy dog.",
        30.0,
        64,
        400,
    );
    draw_font_line(&mut image, &font, "0123456789  !?&@#", 30.0, 64, 485);
    fs::create_dir_all(cache).map_err(ServiceError::from)?;
    let output = cache.join(format!("{}-font.png", source_cache_key(path)));
    image.save(&output).map_err(|error| {
        ServiceError::new(
            ErrorCode::Internal,
            format!("Unable to cache font preview: {error}"),
        )
    })?;
    Ok(RichPreview {
        title: family,
        subtitle: format!("{} font", extension(path).to_ascii_uppercase()),
        blocks: vec![block(
            RichBlockKind::Metadata,
            format!("Glyphs · {}", font.chars().len()),
        )],
        image_path: Some(output),
    })
}

#[cfg(not(feature = "preview-fonts"))]
fn font_preview(_path: &Path, _cache: &Path) -> ServiceResult<RichPreview> {
    Err(ServiceError::new(
        ErrorCode::Unsupported,
        "Font previews aren't included in this build",
    ))
}

#[cfg(feature = "preview-fonts")]
fn font_decode_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(
        ErrorCode::InvalidInput,
        format!("Unable to decode web font: {error}"),
    )
}

#[cfg(feature = "preview-fonts")]
fn draw_font_line(
    image: &mut image::RgbaImage,
    font: &Font,
    text: &str,
    size: f32,
    start_x: i32,
    baseline: i32,
) {
    let mut pen_x = start_x as f32;
    for character in text.chars() {
        let (metrics, bitmap) = font.rasterize(character, size);
        let glyph_x = pen_x.round() as i32 + metrics.xmin;
        let glyph_y = baseline - metrics.ymin - metrics.height as i32;
        for row in 0..metrics.height {
            for column in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + column];
                if alpha == 0 {
                    continue;
                }
                let x = glyph_x + column as i32;
                let y = glyph_y + row as i32;
                if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
                    continue;
                }
                let pixel = image.get_pixel_mut(x as u32, y as u32);
                let blend = f32::from(alpha) / 255.0;
                for channel in 0..3 {
                    pixel[channel] =
                        (f32::from(pixel[channel]) * (1.0 - blend) + 238.0 * blend) as u8;
                }
            }
        }
        pen_x += metrics.advance_width;
        if pen_x >= image.width() as f32 - 48.0 {
            break;
        }
    }
}

fn source_cache_key(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    if let Ok(metadata) = fs::metadata(path) {
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

fn tag_value(source: &str, tag: &str) -> Option<String> {
    let lower = source.to_ascii_lowercase();
    let open = format!("<{tag}");
    let start = lower.find(&open)?;
    let content_start = lower[start..].find('>')? + start + 1;
    let close = format!("</{tag}>");
    let content_end = lower[content_start..].find(&close)? + content_start;
    let value = source[content_start..content_end].trim();
    (!value.is_empty()).then(|| decode_basic_entities(value))
}

fn decode_basic_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn file_title(path: &Path) -> String {
    path.file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn matches_extension(path: &str, extensions: &[&str]) -> bool {
    extensions
        .iter()
        .any(|extension| path.ends_with(&format!(".{extension}")))
}

fn is_archive_image(path: &str) -> bool {
    matches_extension(path, &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
}

#[cfg(any(feature = "preview-sqlite", feature = "preview-columnar"))]
fn truncate_cell(value: &str) -> String {
    let mut value = value.replace(['\r', '\n'], " ");
    if value.chars().count() > 160 {
        value = value.chars().take(159).collect();
        value.push('…');
    }
    value
}

#[cfg(feature = "preview-sqlite")]
fn uppercase_first(value: &str) -> String {
    let mut characters = value.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn markdown_and_html_render_without_executing_embedded_content() {
        let temp = tempfile::tempdir().unwrap();
        let markdown = temp.path().join("readme.md");
        fs::write(&markdown, "# Preview\n\n**Safe** content").unwrap();
        let rendered = preview(&markdown, temp.path()).unwrap();
        assert_eq!(rendered.title, "Preview");
        assert!(
            rendered
                .blocks
                .iter()
                .any(|block| block.text.contains("Safe"))
        );

        let readme = temp.path().join("README.md");
        fs::write(
            &readme,
            "# Explorie test\n\nSome **markdown** with a [link](https://example.com).\n",
        )
        .unwrap();
        let rendered = preview(&readme, temp.path()).unwrap();
        assert_eq!(rendered.title, "Explorie test");
        assert_eq!(rendered.blocks.len(), 1, "the title is not repeated");
        assert_eq!(rendered.blocks[0].text, "Some markdown with a link.");
        assert!(rendered.blocks[0].spans.iter().any(|span| {
            span.style.link.as_deref() == Some("https://example.com") && span.text == "link"
        }));

        let html = temp.path().join("page.html");
        fs::write(
            &html,
            "<title>Offline</title><script>network()</script><p>Hello</p>",
        )
        .unwrap();
        let rendered = preview(&html, temp.path()).unwrap();
        assert_eq!(rendered.title, "Offline");
        assert!(rendered.subtitle.contains("network access disabled"));
    }

    #[test]
    #[cfg(feature = "preview-mail")]
    fn email_preview_extracts_headers_body_and_attachment_count() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("message.eml");
        fs::write(
            &path,
            "From: Ada <ada@example.com>\r\nTo: Lin <lin@example.com>\r\nSubject: Hello\r\n\r\nBody text",
        )
        .unwrap();
        let rendered = preview(&path, temp.path()).unwrap();
        assert_eq!(rendered.title, "Hello");
        assert!(
            rendered
                .blocks
                .iter()
                .any(|block| block.text.contains("ada@example.com"))
        );
        assert!(
            rendered
                .blocks
                .iter()
                .any(|block| block.text.contains("Body text"))
        );
    }

    #[test]
    #[cfg(feature = "preview-sqlite")]
    fn sqlite_preview_is_read_only_and_samples_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE items (name TEXT, count INTEGER)", [])
            .unwrap();
        connection
            .execute("INSERT INTO items VALUES ('alpha', 3)", [])
            .unwrap();
        drop(connection);
        let rendered = preview(&path, temp.path()).unwrap();
        assert!(
            rendered
                .blocks
                .iter()
                .any(|block| block.text.contains("items"))
        );
        assert!(
            rendered
                .blocks
                .iter()
                .any(|block| block.text.contains("alpha"))
        );
    }

    #[test]
    #[cfg(feature = "preview-sqlite")]
    fn untrusted_sqlite_connections_are_defensive_read_only_and_time_boxed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hostile.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (name TEXT);
                 CREATE TRIGGER audit AFTER INSERT ON items BEGIN SELECT 1; END;
                 CREATE VIEW everything AS SELECT * FROM items;",
            )
            .unwrap();
        drop(connection);

        let connection = open_untrusted_sqlite(&path, Duration::from_millis(250)).unwrap();
        assert!(
            connection
                .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
                .unwrap()
        );
        assert!(
            !connection
                .db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA)
                .unwrap()
        );
        let pragma = |name: &str| -> i64 {
            connection
                .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(pragma("trusted_schema"), 0);
        assert_eq!(pragma("query_only"), 1);
        assert_eq!(
            connection.limit(Limit::SQLITE_LIMIT_LENGTH).unwrap(),
            MAX_SQLITE_VALUE_BYTES
        );
        assert!(
            connection
                .execute("INSERT INTO items VALUES ('x')", [])
                .is_err()
        );
        assert!(
            connection
                .query_row("SELECT length(zeroblob(33554432))", [], |row| {
                    row.get::<_, i64>(0)
                })
                .is_err()
        );

        let started = Instant::now();
        let runaway = connection.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) \
             SELECT count(*) FROM n",
            [],
            |row| row.get::<_, i64>(0),
        );
        assert!(runaway.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A backend left out by its cargo feature still claims its files, and
    /// says so instead of failing as if the file were malformed.
    #[test]
    fn previews_left_out_of_the_build_are_unsupported() {
        let temp = tempfile::tempdir().unwrap();
        for (included, name) in [
            (cfg!(feature = "preview-fonts"), "font.woff2"),
            (cfg!(feature = "preview-mail"), "message.eml"),
            (cfg!(feature = "preview-sqlite"), "data.sqlite"),
            (cfg!(feature = "preview-columnar"), "table.parquet"),
            (cfg!(feature = "preview-columnar"), "table.feather"),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, b"not a real file").unwrap();
            let result = preview(&path, temp.path());
            if included {
                if let Err(error) = result {
                    assert!(
                        !error.message.contains("in this build"),
                        "{name}: {error:?}"
                    );
                }
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, ErrorCode::Unsupported, "{name}");
                assert!(
                    error.message.ends_with("aren't included in this build"),
                    "{name}: {error:?}"
                );
            }
        }
    }

    #[test]
    fn cbz_preview_extracts_the_first_page_without_writing_to_the_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("comic.cbz");
        let file = File::create(&path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("001.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        let mut page = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut page, image::ImageFormat::Png)
            .unwrap();
        archive.write_all(page.get_ref()).unwrap();
        archive.finish().unwrap();
        let rendered = preview(&path, temp.path()).unwrap();
        assert!(rendered.image_path.is_some_and(|path| path.is_file()));
        assert!(rendered.blocks[0].text.contains('1'));
    }
}
