//! Capturing EXIF for display, and the minimal block that survives stripping.

use serde_json::{Map, Value};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Values longer than this are dropped from the captured EXIF: they are
/// embedded thumbnails, vendor blobs or abuse, never something to display.
const MAX_VALUE_LEN: usize = 4096;

/// The file's EXIF as a JSON object of tag name to display value.
///
/// Dropped: `MakerNote` (opaque vendor data), anything over 4 KiB, the
/// embedded thumbnail's tags, and **every GPS tag** — the captured EXIF is
/// shown in the public viewer, and an uploader's location must never be.
pub(crate) fn capture(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;

    let mut map = Map::new();
    for field in exif.fields() {
        if field.ifd_num != exif::In::PRIMARY
            || field.tag == exif::Tag::MakerNote
            || field.tag.context() == exif::Context::Gps
        {
            continue;
        }
        let value = field.display_value().with_unit(&exif).to_string();
        if value.len() > MAX_VALUE_LEN {
            continue;
        }
        map.insert(field.tag.to_string(), Value::String(value));
    }
    if map.is_empty() {
        return None;
    }
    serde_json::to_string(&Value::Object(map)).ok()
}

/// The EXIF orientation, 1 (upright) when absent or unreadable.
pub(crate) fn orientation(path: &Path) -> u32 {
    let Ok(file) = File::open(path) else {
        return 1;
    };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .unwrap_or(1)
}

/// A TIFF block holding nothing but an orientation: what stripping leaves
/// behind, so a sideways phone photo is still displayed the right way up.
pub(crate) fn orientation_only(orientation: u32) -> Vec<u8> {
    let mut tiff = b"MM\x00\x2a".to_vec();
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 offset
    tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_be_bytes()); // count
    tiff.extend_from_slice(&(orientation as u16).to_be_bytes());
    tiff.extend_from_slice(&[0, 0]); // padding to four bytes
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    tiff
}
