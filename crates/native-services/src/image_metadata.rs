use crate::{ErrorCode, ServiceError, ServiceResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_UNDEFINED: u16 = 7;
const TYPE_SLONG: u16 = 9;
const TYPE_SRATIONAL: u16 = 10;

const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_MAKE: u16 = 0x010f;
const TAG_MODEL: u16 = 0x0110;
const TAG_DATETIME: u16 = 0x0132;
const TAG_IPTC: u16 = 0x83bb;
const TAG_PHOTOSHOP: u16 = 0x8649;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
const TAG_DATETIME_DIGITIZED: u16 = 0x9004;
const TAG_PIXEL_X: u16 = 0xa002;
const TAG_PIXEL_Y: u16 = 0xa003;

const GPS_LAT_REF: u16 = 0x0001;
const GPS_LAT: u16 = 0x0002;
const GPS_LON_REF: u16 = 0x0003;
const GPS_LON: u16 = 0x0004;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGps {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageMetadata {
    pub camera: Option<String>,
    pub taken_at: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub caption: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub gps: Option<ImageGps>,
}

impl ImageMetadata {
    pub fn has_photo_metadata(&self) -> bool {
        self.camera.is_some()
            || self.taken_at.is_some()
            || (self.width.is_some() && self.height.is_some())
            || self.caption.is_some()
            || !self.keywords.is_empty()
            || self.gps.is_some()
    }

    fn merge(&mut self, extra: Self) {
        self.camera = extra.camera.or_else(|| self.camera.take());
        self.taken_at = extra.taken_at.or_else(|| self.taken_at.take());
        self.width = extra.width.or(self.width);
        self.height = extra.height.or(self.height);
        self.caption = extra.caption.or_else(|| self.caption.take());
        if !extra.keywords.is_empty() {
            self.keywords = extra.keywords;
        }
        self.gps = extra.gps.or(self.gps);
    }
}

pub fn is_image_metadata_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "tif" | "tiff" | "png" | "webp")
    )
}

pub fn format_exif_date(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 10
        && bytes[4] == b':'
        && bytes[7] == b':'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        format!(
            "{}-{}-{}{}",
            &value[..4],
            &value[5..7],
            &value[8..10],
            &value[10..]
        )
    } else {
        value.to_string()
    }
}

pub(crate) fn load_image_metadata(path: &Path) -> ServiceResult<ImageMetadata> {
    let metadata = path.metadata().map_err(ServiceError::from)?;
    if !metadata.is_file() {
        return Err(ServiceError::new(
            ErrorCode::InvalidInput,
            "Image metadata requires a regular file",
        ));
    }
    let bytes = fs::read(path).map_err(ServiceError::from)?;
    Ok(parse_image_metadata(&bytes))
}

pub fn parse_image_metadata(bytes: &[u8]) -> ImageMetadata {
    if bytes.starts_with(&[0xff, 0xd8]) {
        parse_jpeg(bytes)
    } else if is_tiff(bytes) {
        parse_tiff(bytes)
    } else if is_png(bytes) {
        parse_png(bytes)
    } else if is_webp(bytes) {
        parse_webp(bytes)
    } else {
        ImageMetadata::default()
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

#[derive(Clone, Copy)]
struct IfdTag {
    kind: u16,
    count: u32,
    value_offset: usize,
}

fn is_tiff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"II\x2a\0") || bytes.starts_with(b"MM\0\x2a")
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn read_u16(bytes: &[u8], offset: usize, endian: Endian) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u16::from_le_bytes(bytes),
        Endian::Big => u16::from_be_bytes(bytes),
    })
}

fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(match endian {
        Endian::Little => u32::from_le_bytes(bytes),
        Endian::Big => u32::from_be_bytes(bytes),
    })
}

fn type_size(kind: u16) -> usize {
    match kind {
        TYPE_SHORT => 2,
        TYPE_LONG | TYPE_SLONG => 4,
        TYPE_RATIONAL | TYPE_SRATIONAL => 8,
        _ => 1,
    }
}

fn parse_jpeg(bytes: &[u8]) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    let mut offset = 2usize;
    while offset.checked_add(4).is_some_and(|end| end <= bytes.len()) {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if marker == 0xd8 || marker == 0x00 {
            offset += 2;
            continue;
        }
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        let Some(size) = read_u16(bytes, offset + 2, Endian::Big).map(usize::from) else {
            break;
        };
        let Some(payload_end) = offset
            .checked_add(2)
            .and_then(|value| value.checked_add(size))
        else {
            break;
        };
        if size < 2 || payload_end > bytes.len() {
            break;
        }
        let payload_start = offset + 4;
        let payload = &bytes[payload_start..payload_end];
        if marker == 0xe1 && payload.starts_with(b"Exif\0\0") {
            metadata.merge(parse_tiff(&payload[6..]));
        } else if marker == 0xed && payload.starts_with(b"Photoshop 3.0\0") {
            metadata.merge(parse_photoshop_irb(&payload[14..]));
        } else if (0xc0..=0xc3).contains(&marker) && payload.len() >= 5 {
            metadata.merge(ImageMetadata {
                height: read_u16(payload, 1, Endian::Big).map(u32::from),
                width: read_u16(payload, 3, Endian::Big).map(u32::from),
                ..ImageMetadata::default()
            });
        }
        offset = payload_end;
    }
    metadata
}

fn parse_png(bytes: &[u8]) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    let mut offset = 8usize;
    while offset.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let Some(length) = read_u32(bytes, offset, Endian::Big).map(|value| value as usize) else {
            break;
        };
        let Some(end) = offset
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
        else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        let kind = &bytes[offset + 4..offset + 8];
        let data = &bytes[offset + 8..offset + 8 + length];
        if kind == b"IHDR" && data.len() >= 8 {
            metadata.merge(ImageMetadata {
                width: read_u32(data, 0, Endian::Big),
                height: read_u32(data, 4, Endian::Big),
                ..ImageMetadata::default()
            });
        } else if kind == b"eXIf" && data.len() >= 8 {
            metadata.merge(parse_tiff(data));
        } else if matches!(kind, b"tEXt" | b"iTXt")
            && let Some(iptc) = parse_image_magick_iptc_text(data)
        {
            metadata.merge(iptc);
        }
        offset = end;
    }
    metadata
}

fn parse_webp(bytes: &[u8]) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let kind = &bytes[offset..offset + 4];
        let Some(size) = read_u32(bytes, offset + 4, Endian::Little).map(|value| value as usize)
        else {
            break;
        };
        let Some(data_end) = offset
            .checked_add(8)
            .and_then(|value| value.checked_add(size))
        else {
            break;
        };
        if data_end > bytes.len() {
            break;
        }
        let data = &bytes[offset + 8..data_end];
        if kind == b"EXIF" && data.len() >= 8 {
            let tiff = data.strip_prefix(b"Exif\0\0").unwrap_or(data);
            metadata.merge(parse_tiff(tiff));
        } else if kind == b"VP8X" && data.len() >= 10 {
            let width =
                1 + u32::from(data[4]) + (u32::from(data[5]) << 8) + (u32::from(data[6]) << 16);
            let height =
                1 + u32::from(data[7]) + (u32::from(data[8]) << 8) + (u32::from(data[9]) << 16);
            metadata.merge(ImageMetadata {
                width: Some(width),
                height: Some(height),
                ..ImageMetadata::default()
            });
        }
        offset = data_end.saturating_add(size % 2);
    }
    metadata
}

fn parse_tiff(bytes: &[u8]) -> ImageMetadata {
    let endian = if bytes.starts_with(b"II\x2a\0") {
        Endian::Little
    } else if bytes.starts_with(b"MM\0\x2a") {
        Endian::Big
    } else {
        return ImageMetadata::default();
    };
    let Some(ifd) = read_u32(bytes, 4, endian).map(|value| value as usize) else {
        return ImageMetadata::default();
    };
    collect_ifd(bytes, ifd, endian, 0)
}

fn collect_ifd(bytes: &[u8], offset: usize, endian: Endian, depth: u8) -> ImageMetadata {
    if depth > 6 {
        return ImageMetadata::default();
    }
    let Some(tags) = read_ifd(bytes, offset, endian) else {
        return ImageMetadata::default();
    };
    let make = read_ascii_tag(bytes, &tags, TAG_MAKE);
    let model = read_ascii_tag(bytes, &tags, TAG_MODEL);
    let mut metadata = ImageMetadata {
        camera: format_camera(make.as_deref(), model.as_deref()),
        taken_at: read_ascii_tag(bytes, &tags, TAG_DATETIME_ORIGINAL)
            .or_else(|| read_ascii_tag(bytes, &tags, TAG_DATETIME_DIGITIZED))
            .or_else(|| read_ascii_tag(bytes, &tags, TAG_DATETIME)),
        width: read_integer_tag(bytes, &tags, TAG_IMAGE_WIDTH, endian)
            .or_else(|| read_integer_tag(bytes, &tags, TAG_PIXEL_X, endian)),
        height: read_integer_tag(bytes, &tags, TAG_IMAGE_LENGTH, endian)
            .or_else(|| read_integer_tag(bytes, &tags, TAG_PIXEL_Y, endian)),
        ..ImageMetadata::default()
    };
    if let Some(iptc) = read_bytes_tag(bytes, &tags, TAG_IPTC) {
        metadata.merge(parse_iptc(iptc));
    }
    if let Some(photoshop) = read_bytes_tag(bytes, &tags, TAG_PHOTOSHOP) {
        metadata.merge(parse_photoshop_irb(photoshop));
    }
    if let Some(exif_offset) = read_integer_tag(bytes, &tags, TAG_EXIF_IFD, endian) {
        metadata.merge(collect_ifd(bytes, exif_offset as usize, endian, depth + 1));
    }
    if let Some(gps_offset) = read_integer_tag(bytes, &tags, TAG_GPS_IFD, endian)
        && let Some(gps_tags) = read_ifd(bytes, gps_offset as usize, endian)
    {
        metadata.gps = read_gps(bytes, &gps_tags, endian);
    }
    metadata
}

fn read_ifd(bytes: &[u8], offset: usize, endian: Endian) -> Option<HashMap<u16, IfdTag>> {
    let count = usize::from(read_u16(bytes, offset, endian)?);
    if count == 0 || count > 128 {
        return None;
    }
    let mut tags = HashMap::with_capacity(count);
    for index in 0..count {
        let entry = offset.checked_add(2)?.checked_add(index.checked_mul(12)?)?;
        if entry.checked_add(12)? > bytes.len() {
            break;
        }
        let tag = read_u16(bytes, entry, endian)?;
        let kind = read_u16(bytes, entry + 2, endian)?;
        let count = read_u32(bytes, entry + 4, endian)?;
        let length = type_size(kind).checked_mul(count as usize)?;
        let value_offset = if length <= 4 {
            entry + 8
        } else {
            read_u32(bytes, entry + 8, endian)? as usize
        };
        tags.insert(
            tag,
            IfdTag {
                kind,
                count,
                value_offset,
            },
        );
    }
    Some(tags)
}

fn tag_bytes<'a>(bytes: &'a [u8], tag: &IfdTag) -> Option<&'a [u8]> {
    let length = type_size(tag.kind).checked_mul(tag.count as usize)?;
    if length == 0 {
        return None;
    }
    bytes.get(tag.value_offset..tag.value_offset.checked_add(length)?)
}

fn read_ascii_tag(bytes: &[u8], tags: &HashMap<u16, IfdTag>, id: u16) -> Option<String> {
    let tag = tags.get(&id)?;
    if !matches!(tag.kind, TYPE_ASCII | TYPE_UNDEFINED | TYPE_BYTE) {
        return None;
    }
    decode_text(tag_bytes(bytes, tag)?)
}

fn read_integer_tag(
    bytes: &[u8],
    tags: &HashMap<u16, IfdTag>,
    id: u16,
    endian: Endian,
) -> Option<u32> {
    let tag = tags.get(&id)?;
    match tag.kind {
        TYPE_SHORT => read_u16(bytes, tag.value_offset, endian).map(u32::from),
        TYPE_LONG | TYPE_SLONG => read_u32(bytes, tag.value_offset, endian),
        _ => None,
    }
}

fn read_bytes_tag<'a>(bytes: &'a [u8], tags: &HashMap<u16, IfdTag>, id: u16) -> Option<&'a [u8]> {
    tag_bytes(bytes, tags.get(&id)?)
}

fn format_camera(make: Option<&str>, model: Option<&str>) -> Option<String> {
    match (make, model) {
        (Some(make), Some(model))
            if model
                .to_ascii_lowercase()
                .starts_with(&make.to_ascii_lowercase()) =>
        {
            Some(model.to_string())
        }
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(make), None) => Some(make.to_string()),
        (None, Some(model)) => Some(model.to_string()),
        (None, None) => None,
    }
}

fn read_gps(bytes: &[u8], tags: &HashMap<u16, IfdTag>, endian: Endian) -> Option<ImageGps> {
    let latitude = read_gps_coordinate(bytes, tags.get(&GPS_LAT)?, endian)?;
    let longitude = read_gps_coordinate(bytes, tags.get(&GPS_LON)?, endian)?;
    let latitude = if read_ascii_tag(bytes, tags, GPS_LAT_REF).as_deref() == Some("S") {
        -latitude
    } else {
        latitude
    };
    let longitude = if read_ascii_tag(bytes, tags, GPS_LON_REF).as_deref() == Some("W") {
        -longitude
    } else {
        longitude
    };
    (latitude.is_finite() && longitude.is_finite()).then_some(ImageGps {
        latitude,
        longitude,
    })
}

fn read_gps_coordinate(bytes: &[u8], tag: &IfdTag, endian: Endian) -> Option<f64> {
    if tag.count < 3 || !matches!(tag.kind, TYPE_RATIONAL | TYPE_SRATIONAL) {
        return None;
    }
    let degrees = read_rational(bytes, tag.value_offset, endian)?;
    let minutes = read_rational(bytes, tag.value_offset + 8, endian)?;
    let seconds = read_rational(bytes, tag.value_offset + 16, endian)?;
    Some(degrees + minutes / 60.0 + seconds / 3600.0)
}

fn read_rational(bytes: &[u8], offset: usize, endian: Endian) -> Option<f64> {
    let numerator = read_u32(bytes, offset, endian)?;
    let denominator = read_u32(bytes, offset + 4, endian)?;
    (denominator != 0).then_some(f64::from(numerator) / f64::from(denominator))
}

fn parse_photoshop_irb(bytes: &[u8]) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    let mut offset = 0usize;
    while offset.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        if !bytes[offset..].starts_with(b"8BIM") {
            offset += 1;
            continue;
        }
        let Some(kind) = read_u16(bytes, offset + 4, Endian::Big) else {
            break;
        };
        let mut cursor = offset + 6;
        let name_length = usize::from(bytes[cursor]);
        cursor = cursor.saturating_add(1 + name_length);
        if name_length % 2 == 0 {
            cursor = cursor.saturating_add(1);
        }
        let Some(size) = read_u32(bytes, cursor, Endian::Big).map(|value| value as usize) else {
            break;
        };
        cursor = cursor.saturating_add(4);
        let Some(end) = cursor.checked_add(size) else {
            break;
        };
        let Some(data) = bytes.get(cursor..end) else {
            break;
        };
        if kind == 0x0404 {
            metadata.merge(parse_iptc(data));
        }
        offset = end.saturating_add(size % 2);
    }
    metadata
}

fn parse_iptc(bytes: &[u8]) -> ImageMetadata {
    let mut metadata = ImageMetadata::default();
    let mut offset = 0usize;
    while offset.checked_add(5).is_some_and(|end| end <= bytes.len()) {
        if bytes[offset] != 0x1c {
            offset += 1;
            continue;
        }
        let record = bytes[offset + 1];
        let dataset = bytes[offset + 2];
        let mut length = usize::from(read_u16(bytes, offset + 3, Endian::Big).unwrap_or(0));
        let mut data_start = offset + 5;
        if length >= 0x8000 {
            let extended = length & 0x7fff;
            let Some(length_bytes) = bytes.get(data_start..data_start.saturating_add(extended))
            else {
                break;
            };
            length = 0;
            for byte in length_bytes {
                let Some(next) = length
                    .checked_mul(256)
                    .and_then(|value| value.checked_add(usize::from(*byte)))
                else {
                    return metadata;
                };
                length = next;
            }
            data_start = data_start.saturating_add(extended);
        }
        let Some(end) = data_start.checked_add(length) else {
            break;
        };
        let Some(data) = bytes.get(data_start..end) else {
            break;
        };
        if record == 2
            && let Some(text) = decode_text(data)
        {
            match dataset {
                120 => metadata.caption = Some(text),
                5 if metadata.caption.is_none() => metadata.caption = Some(text),
                25 => metadata.keywords.push(text),
                _ => {}
            }
        }
        offset = end;
    }
    metadata
}

fn parse_image_magick_iptc_text(bytes: &[u8]) -> Option<ImageMetadata> {
    let text = String::from_utf8_lossy(bytes);
    let marker = "raw profile type iptc";
    let index = text.to_ascii_lowercase().find(marker)?;
    let remainder = &text[index + marker.len()..];
    let remainder = remainder
        .split_once('\n')
        .map_or(remainder, |(_, rest)| rest);
    let hex: String = remainder.chars().filter(char::is_ascii_hexdigit).collect();
    if hex.len() < 10 {
        return None;
    }
    let mut raw = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        raw.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
    }
    Some(parse_iptc(&raw))
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn append_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn append_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn little_tiff_with_exif_and_gps() -> Vec<u8> {
        let make = b"Canon\0";
        let model = b"EOS R5\0";
        let date = b"2024:03:15 14:30:00\0";
        let ifd0_offset = 8u32;
        let ifd0_size = 2 + 5 * 12 + 4;
        let make_offset = ifd0_offset + ifd0_size;
        let model_offset = make_offset + make.len() as u32;
        let exif_offset = model_offset + model.len() as u32;
        let exif_size = 2 + 12 + 4;
        let date_offset = exif_offset + exif_size;
        let gps_offset = date_offset + date.len() as u32;
        let gps_size = 2 + 4 * 12 + 4;
        let lat_offset = gps_offset + gps_size;
        let lon_offset = lat_offset + 24;

        let mut bytes = b"II\x2a\0\x08\0\0\0".to_vec();
        append_u16(&mut bytes, 5);
        for (tag, kind, count, value) in [
            (TAG_MAKE, TYPE_ASCII, make.len() as u32, make_offset),
            (TAG_MODEL, TYPE_ASCII, model.len() as u32, model_offset),
            (TAG_IMAGE_WIDTH, TYPE_LONG, 1, 4032),
            (TAG_EXIF_IFD, TYPE_LONG, 1, exif_offset),
            (TAG_GPS_IFD, TYPE_LONG, 1, gps_offset),
        ] {
            append_u16(&mut bytes, tag);
            append_u16(&mut bytes, kind);
            append_u32(&mut bytes, count);
            append_u32(&mut bytes, value);
        }
        append_u32(&mut bytes, 0);
        bytes.extend_from_slice(make);
        bytes.extend_from_slice(model);

        append_u16(&mut bytes, 1);
        append_u16(&mut bytes, TAG_DATETIME_ORIGINAL);
        append_u16(&mut bytes, TYPE_ASCII);
        append_u32(&mut bytes, date.len() as u32);
        append_u32(&mut bytes, date_offset);
        append_u32(&mut bytes, 0);
        bytes.extend_from_slice(date);

        append_u16(&mut bytes, 4);
        for (tag, kind, count, value) in [
            (
                GPS_LAT_REF,
                TYPE_ASCII,
                2,
                u32::from_le_bytes([b'N', 0, 0, 0]),
            ),
            (GPS_LAT, TYPE_RATIONAL, 3, lat_offset),
            (
                GPS_LON_REF,
                TYPE_ASCII,
                2,
                u32::from_le_bytes([b'W', 0, 0, 0]),
            ),
            (GPS_LON, TYPE_RATIONAL, 3, lon_offset),
        ] {
            append_u16(&mut bytes, tag);
            append_u16(&mut bytes, kind);
            append_u32(&mut bytes, count);
            append_u32(&mut bytes, value);
        }
        append_u32(&mut bytes, 0);
        for (numerator, denominator) in [(37, 1), (30, 1), (0, 1), (122, 1), (15, 1), (0, 1)] {
            append_u32(&mut bytes, numerator);
            append_u32(&mut bytes, denominator);
        }
        bytes
    }

    fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let size = (payload.len() + 2) as u16;
        let mut segment = vec![0xff, marker];
        segment.extend_from_slice(&size.to_be_bytes());
        segment.extend_from_slice(payload);
        segment
    }

    fn metadata_jpeg() -> Vec<u8> {
        let tiff = little_tiff_with_exif_and_gps();
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&tiff);
        let iptc = [
            b"\x1c\x02\x78\0\x0eHarbor at dusk".as_slice(),
            b"\x1c\x02\x19\0\x05ocean".as_slice(),
            b"\x1c\x02\x19\0\x06travel".as_slice(),
        ]
        .concat();
        let mut photoshop = b"Photoshop 3.0\0".to_vec();
        photoshop.extend_from_slice(b"8BIM\x04\x04\0\0");
        photoshop.extend_from_slice(&(iptc.len() as u32).to_be_bytes());
        photoshop.extend_from_slice(&iptc);
        if iptc.len() % 2 == 1 {
            photoshop.push(0);
        }
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend(jpeg_segment(0xe1, &exif));
        jpeg.extend(jpeg_segment(0xed, &photoshop));
        jpeg.extend(jpeg_segment(0xc0, &[8, 0x0b, 0xd0, 0x0f, 0xc0]));
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        jpeg
    }

    #[test]
    fn parses_legacy_exif_iptc_dimensions_and_gps_without_exposing_by_default() {
        let metadata = parse_image_metadata(&metadata_jpeg());
        assert_eq!(metadata.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(metadata.taken_at.as_deref(), Some("2024:03:15 14:30:00"));
        assert_eq!((metadata.width, metadata.height), (Some(4032), Some(3024)));
        assert_eq!(metadata.caption.as_deref(), Some("Harbor at dusk"));
        assert_eq!(metadata.keywords, ["ocean", "travel"]);
        assert_eq!(
            metadata.gps,
            Some(ImageGps {
                latitude: 37.5,
                longitude: -122.25,
            })
        );
        assert!(metadata.has_photo_metadata());
        assert_eq!(
            format_exif_date(metadata.taken_at.as_deref().unwrap()),
            "2024-03-15 14:30:00"
        );
    }

    #[test]
    fn png_webp_tiff_and_malformed_inputs_are_recoverable() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            (
                parse_image_metadata(&png).width,
                parse_image_metadata(&png).height
            ),
            (Some(640), Some(480))
        );

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0, 0xff, 0, 0, 0x7f, 0, 0]);
        assert_eq!(
            (
                parse_image_metadata(&webp).width,
                parse_image_metadata(&webp).height
            ),
            (Some(256), Some(128))
        );
        assert_eq!(
            parse_image_metadata(&little_tiff_with_exif_and_gps()).width,
            Some(4032)
        );
        assert_eq!(
            parse_image_metadata(b"not an image"),
            ImageMetadata::default()
        );
        assert_eq!(
            parse_image_metadata(&[0xff, 0xd8, 0xff]),
            ImageMetadata::default()
        );
    }

    #[test]
    fn service_reads_files_and_reports_missing_sources() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("photo.jpg");
        fs::write(&image, metadata_jpeg()).unwrap();
        let metadata = load_image_metadata(&image).unwrap();
        assert_eq!(metadata.camera.as_deref(), Some("Canon EOS R5"));

        let error = load_image_metadata(&temp.path().join("missing.jpg")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }
}
