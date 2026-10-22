//! Display dimensions of a HEIF or AVIF image, from its item properties.
//!
//! Neither format's size is "the first width and height in the file". The
//! primary item (`pitm`) has an image-size property (`ispe`) giving the
//! *coded* size, which encoders pad — an HEVC encoder turns 32x16 into
//! 64x64 — then a clean-aperture property (`clap`) crops it back, and a
//! rotation property (`irot`) may turn it on its side. The display size is
//! all three applied in that order.
//!
//! In HEIF those transformation properties are authoritative and any EXIF
//! orientation is informational only, so the caller must not apply EXIF
//! orientation on top.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// The `meta` box holds only item metadata; a legitimate one is small.
const MAX_META_LEN: u64 = 1024 * 1024;

fn bad(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.to_string())
}

/// A box's type and payload bounds within a byte slice.
struct BoxRef<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// Iterate the boxes laid end to end in `data`.
fn boxes(mut data: &[u8]) -> impl Iterator<Item = io::Result<BoxRef<'_>>> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let (header, size) = match size {
            0 => (8, data.len() as u64),
            1 => {
                let Some(large) = data.get(8..16) else {
                    return Some(Err(bad("truncated box")));
                };
                (16, u64::from_be_bytes(large.try_into().unwrap()))
            }
            n => (8, n),
        };
        let Ok(size) = usize::try_from(size) else {
            return Some(Err(bad("box too large")));
        };
        if size < header || size > data.len() {
            data = &[];
            return Some(Err(bad("box overruns its parent")));
        }
        let body = &data[header..size];
        data = &data[size..];
        Some(Ok(BoxRef { kind, body }))
    })
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> io::Result<Option<&'a [u8]>> {
    for b in boxes(data) {
        let b = b?;
        if &b.kind == kind {
            return Ok(Some(b.body));
        }
    }
    Ok(None)
}

fn u16_at(data: &[u8], at: usize) -> io::Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| bad("truncated field"))
}

fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .ok_or_else(|| bad("truncated field"))
}

/// Read the top-level `meta` box's payload without reading the media data.
fn read_meta(path: &Path) -> io::Result<Vec<u8>> {
    let mut r = BufReader::new(File::open(path)?);
    loop {
        let mut head = [0u8; 8];
        r.read_exact(&mut head)?;
        let mut size = u64::from(u32::from_be_bytes(head[0..4].try_into().unwrap()));
        let mut header = 8;
        if size == 1 {
            let mut large = [0u8; 8];
            r.read_exact(&mut large)?;
            size = u64::from_be_bytes(large);
            header = 16;
        }
        if size < header {
            return Err(bad("malformed top-level box"));
        }
        let body_len = size - header;
        if &head[4..8] == b"meta" {
            if body_len > MAX_META_LEN {
                return Err(bad("meta box is implausibly large"));
            }
            let mut body = vec![0u8; body_len as usize];
            r.read_exact(&mut body)?;
            return Ok(body);
        }
        r.seek(SeekFrom::Current(
            i64::try_from(body_len).map_err(|_| bad("box too large"))?,
        ))?;
    }
}

/// Display width and height of the primary image.
pub(crate) fn dimensions(path: &Path) -> io::Result<(u32, u32)> {
    let meta = read_meta(path)?;
    // `meta` is a full box: version and flags come first.
    let children = meta.get(4..).ok_or_else(|| bad("empty meta box"))?;

    let pitm = find(children, b"pitm")?.ok_or_else(|| bad("no primary item"))?;
    let primary = if pitm.first() == Some(&0) {
        u32::from(u16_at(pitm, 4)?)
    } else {
        u32_at(pitm, 4)?
    };

    let iprp = find(children, b"iprp")?.ok_or_else(|| bad("no item properties"))?;
    let ipco = find(iprp, b"ipco")?.ok_or_else(|| bad("no property container"))?;
    let properties: Vec<BoxRef<'_>> = boxes(ipco).collect::<io::Result<_>>()?;
    let associated = associations(iprp, primary)?;

    let mut size = None;
    let mut clean = None;
    let mut quarter_turns = 0u8;
    for index in associated {
        // Property indices are 1-based; 0 means "none".
        let Some(property) = index.checked_sub(1).and_then(|i| properties.get(i)) else {
            continue;
        };
        match &property.kind {
            b"ispe" => size = Some((u32_at(property.body, 4)?, u32_at(property.body, 8)?)),
            b"clap" => {
                let ratio = |n: usize| -> io::Result<u32> {
                    let num = u32_at(property.body, n)?;
                    let den = u32_at(property.body, n + 4)?.max(1);
                    Ok(((f64::from(num) / f64::from(den)).round()) as u32)
                };
                clean = Some((ratio(0)?, ratio(8)?));
            }
            b"irot" => quarter_turns = property.body.first().copied().unwrap_or(0) & 0x03,
            _ => {}
        }
    }

    let (mut width, mut height) = clean
        .or(size)
        .ok_or_else(|| bad("primary item has no size"))?;
    if quarter_turns % 2 == 1 {
        std::mem::swap(&mut width, &mut height);
    }
    Ok((width, height))
}

/// The property indices `ipma` associates with `item`.
fn associations(iprp: &[u8], item: u32) -> io::Result<Vec<usize>> {
    let mut indices = Vec::new();
    for b in boxes(iprp) {
        let b = b?;
        if &b.kind != b"ipma" {
            continue;
        }
        let body = b.body;
        let version = *body.first().ok_or_else(|| bad("empty ipma"))?;
        let wide_index = body.get(3).is_some_and(|flags| flags & 1 != 0);
        let entries = u32_at(body, 4)?;
        let mut at = 8;
        for _ in 0..entries {
            let id = if version < 1 {
                let id = u32::from(u16_at(body, at)?);
                at += 2;
                id
            } else {
                let id = u32_at(body, at)?;
                at += 4;
                id
            };
            let count = *body.get(at).ok_or_else(|| bad("truncated ipma"))?;
            at += 1;
            for _ in 0..count {
                // The top bit is "essential"; the rest is the index.
                let index = if wide_index {
                    let v = u16_at(body, at)?;
                    at += 2;
                    usize::from(v & 0x7FFF)
                } else {
                    let v = *body.get(at).ok_or_else(|| bad("truncated ipma"))?;
                    at += 1;
                    usize::from(v & 0x7F)
                };
                if id == item {
                    indices.push(index);
                }
            }
        }
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(kind: &[u8; 4], version: u8, flags: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, flags];
        payload.extend_from_slice(body);
        bx(kind, &payload)
    }

    fn ispe(w: u32, h: u32) -> Vec<u8> {
        full(b"ispe", 0, 0, &[w.to_be_bytes(), h.to_be_bytes()].concat())
    }

    /// A file whose primary item 2 has properties `props` (1-based indices
    /// into `ipco`), and an unrelated item 1 with a decoy size.
    fn file(ipco: &[Vec<u8>], primary_props: &[u8], decoy_prop: u8) -> tempfile::NamedTempFile {
        let pitm = full(b"pitm", 0, 0, &2u16.to_be_bytes());
        let mut ipma_body = 2u32.to_be_bytes().to_vec();
        ipma_body.extend_from_slice(&1u16.to_be_bytes());
        ipma_body.extend_from_slice(&[1, decoy_prop]);
        ipma_body.extend_from_slice(&2u16.to_be_bytes());
        ipma_body.push(primary_props.len() as u8);
        // The top bit marks a property essential and must be masked off.
        ipma_body.extend(primary_props.iter().map(|p| p | 0x80));
        let ipma = full(b"ipma", 0, 0, &ipma_body);
        let iprp = bx(b"iprp", &[bx(b"ipco", &ipco.concat()), ipma].concat());
        let meta = full(b"meta", 0, 0, &[pitm, iprp].concat());
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, &[ftyp, meta, bx(b"mdat", &[0; 16])].concat()).unwrap();
        f
    }

    #[test]
    fn the_primary_items_size_is_used_not_the_first_one() {
        let f = file(&[ispe(999, 999), ispe(40, 30)], &[2], 1);
        assert_eq!(dimensions(f.path()).unwrap(), (40, 30));
    }

    #[test]
    fn clean_aperture_then_rotation_are_applied() {
        // 64x64 coded, cropped to 32x16, turned a quarter: 16x32.
        let clap = bx(
            b"clap",
            &[32u32, 1, 16, 1, 0, 1, 0, 1]
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect::<Vec<_>>(),
        );
        let irot = bx(b"irot", &[1]);
        let f = file(&[ispe(64, 64), clap, irot, ispe(1, 1)], &[1, 2, 3], 4);
        assert_eq!(dimensions(f.path()).unwrap(), (16, 32));

        let half_turn = bx(b"irot", &[2]);
        let f = file(&[ispe(40, 30), half_turn], &[1, 2], 1);
        assert_eq!(
            dimensions(f.path()).unwrap(),
            (40, 30),
            "180° keeps the shape"
        );
    }

    #[test]
    fn malformed_boxes_are_errors_not_panics() {
        let mut overrun = bx(b"ftyp", b"heic");
        overrun.extend_from_slice(&[0, 0, 0, 200]);
        overrun.extend_from_slice(b"meta");
        overrun.extend_from_slice(&[0; 20]);
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, &overrun).unwrap();
        assert!(dimensions(f.path()).is_err());

        let no_size = file(&[bx(b"irot", &[1])], &[1], 1);
        assert!(dimensions(no_size.path()).is_err());
    }
}
