//! Probing images from their headers alone.
//!
//! Nothing here decodes a pixel. Dimensions come from the format header,
//! orientation from EXIF, animation from walking the container's chunks or
//! blocks. That is what makes the decompression-bomb guard sound: a 69-byte
//! PNG claiming ten gigapixels is measured, refused by the caller's limit,
//! and never allocated.

use crate::sniff::Sniffed;
use haloid_core::MediaError;
use haloid_core::media::ProbeResult;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub(crate) fn probe(path: &Path, sniffed: Sniffed) -> Result<ProbeResult, MediaError> {
    let header = |e: &dyn std::fmt::Display| {
        MediaError::ProbeFailed(format!("could not read image header: {e}"))
    };
    let (width, height) = if matches!(sniffed.extension, "heic" | "heif" | "avif") {
        // HEIF's own crop and rotation are already applied, and they are
        // authoritative over any EXIF orientation: see `heif`.
        crate::heif::dimensions(path).map_err(|e| header(&e))?
    } else {
        let size = imagesize::size(path).map_err(|e| header(&e))?;
        let width = u32::try_from(size.width).unwrap_or(u32::MAX);
        let height = u32::try_from(size.height).unwrap_or(u32::MAX);
        // EXIF orientations 5 to 8 turn the image on its side. The recorded
        // dimensions are the ones a browser will render.
        if matches!(crate::exif::orientation(path), 5..=8) {
            (height, width)
        } else {
            (width, height)
        }
    };

    let frames = match sniffed.extension {
        "gif" => gif_frames(path),
        "png" => png_frames(path),
        "webp" => webp_frames(path),
        _ => Ok(1),
    }
    .map_err(|e| MediaError::ProbeFailed(format!("could not read image structure: {e}")))?;
    let is_animated = frames > 1;

    let mime = if sniffed.extension == "png" && is_animated {
        "image/apng"
    } else {
        sniffed.mime
    };

    Ok(ProbeResult {
        kind: sniffed.kind,
        mime: mime.to_string(),
        extension: sniffed.extension.to_string(),
        width,
        height,
        duration_ms: None,
        is_animated,
        frame_count: is_animated.then_some(frames),
    })
}

fn read_u8(r: &mut impl Read) -> std::io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn skip(r: &mut impl Seek, n: u64) -> std::io::Result<()> {
    r.seek(SeekFrom::Current(i64::try_from(n).unwrap_or(i64::MAX)))?;
    Ok(())
}

/// Count a GIF's image descriptors, skipping every data sub-block unread.
fn gif_frames(path: &Path) -> std::io::Result<u32> {
    let mut r = BufReader::new(File::open(path)?);
    let mut header = [0u8; 13];
    r.read_exact(&mut header)?;
    // Logical screen descriptor flags: a global colour table of 2^(n+1)
    // three-byte entries follows if the top bit is set.
    if header[10] & 0x80 != 0 {
        skip(&mut r, 3 << ((header[10] & 0x07) + 1))?;
    }

    let mut frames = 0u32;
    loop {
        match read_u8(&mut r) {
            Ok(0x2C) => {
                frames += 1;
                let mut descriptor = [0u8; 9];
                r.read_exact(&mut descriptor)?;
                if descriptor[8] & 0x80 != 0 {
                    skip(&mut r, 3 << ((descriptor[8] & 0x07) + 1))?;
                }
                read_u8(&mut r)?; // LZW minimum code size
                skip_sub_blocks(&mut r)?;
            }
            Ok(0x21) => {
                read_u8(&mut r)?; // extension label
                skip_sub_blocks(&mut r)?;
            }
            // The trailer, or a file cut short after its frames: either way
            // the frames seen are the frames there are.
            Ok(0x3B) | Err(_) => break,
            Ok(_) => break,
        }
    }
    Ok(frames.max(1))
}

fn skip_sub_blocks(r: &mut BufReader<File>) -> std::io::Result<()> {
    loop {
        let len = read_u8(r)?;
        if len == 0 {
            return Ok(());
        }
        skip(r, u64::from(len))?;
    }
}

/// An APNG declares its frame count in an `acTL` chunk before the first
/// `IDAT`; a plain PNG has none.
fn png_frames(path: &Path) -> std::io::Result<u32> {
    let mut r = BufReader::new(File::open(path)?);
    skip(&mut r, 8)?; // signature
    loop {
        let mut head = [0u8; 8];
        if r.read_exact(&mut head).is_err() {
            return Ok(1);
        }
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        match &head[4..8] {
            b"acTL" => {
                let mut frames = [0u8; 4];
                r.read_exact(&mut frames)?;
                return Ok(u32::from_be_bytes(frames).max(1));
            }
            b"IDAT" | b"IEND" => return Ok(1),
            _ => skip(&mut r, u64::from(len) + 4)?, // payload and CRC
        }
    }
}

/// A WebP is animated if its `VP8X` header says so; each frame is an `ANMF`
/// chunk.
fn webp_frames(path: &Path) -> std::io::Result<u32> {
    let mut r = BufReader::new(File::open(path)?);
    skip(&mut r, 12)?; // RIFF, size, WEBP
    let mut frames = 0u32;
    loop {
        let mut head = [0u8; 8];
        if r.read_exact(&mut head).is_err() {
            break;
        }
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        if &head[..4] == b"ANMF" {
            frames += 1;
        }
        // Chunks are padded to an even length.
        skip(&mut r, u64::from(len) + u64::from(len & 1))?;
    }
    Ok(frames.max(1))
}
