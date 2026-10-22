#!/usr/bin/env bash
# Regenerate the tiny real media files in fixtures/media/.
#
# The outputs are committed, so this only needs to run when a fixture is added
# or changed. Needs ffmpeg (with libx264, libvpx, libwebp, libmp3lame), vips
# with HEIF support, and python3. Every file is deliberately a few KB at most:
# the media tests read them on every run.
set -euo pipefail
cd "$(dirname "$0")/../fixtures/media"

ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }
src="testsrc=size=32x16:rate=10"

# Stills: 32x16, so a swapped width and height is visible in a test.
ff -f lavfi -i "$src" -frames:v 1 -q:v 8 still.jpg
ff -f lavfi -i "$src" -frames:v 1 still.png
ff -f lavfi -i "$src" -frames:v 1 still.gif
ff -f lavfi -i "$src" -frames:v 1 -c:v libwebp -q:v 30 still.webp
vips heifsave still.png still.avif --compression av1 --Q 30
vips heifsave still.png still.heic --compression hevc --Q 30 2>/dev/null \
  || echo "no HEVC encoder in this vips; skipping still.heic" >&2

# Animations: three frames each.
ff -f lavfi -i "$src" -frames:v 3 anim.gif
ff -f lavfi -i "$src" -frames:v 3 -plays 0 -f apng anim.png
ff -f lavfi -i "$src" -frames:v 3 -c:v libwebp_anim -loop 0 -q:v 30 anim.webp

# Video: one second; a rotated copy whose display size is 16x32; a WebM.
ff -f lavfi -i "$src" -f lavfi -i "sine=frequency=440:duration=1" -t 1 \
  -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -b:a 32k -shortest clip.mp4
ff -display_rotation 90 -i clip.mp4 -c copy rotated.mp4
ff -f lavfi -i "$src" -t 1 -c:v libvpx -b:v 50k clip.webm

# Audio: one second of tone.
ff -f lavfi -i "sine=frequency=440:duration=1" -c:a libmp3lame -b:a 32k tone.mp3
ff -f lavfi -i "sine=frequency=440:duration=1" -ar 8000 -ac 1 tone.wav
ff -f lavfi -i "sine=frequency=440:duration=1" -c:a aac -b:a 32k tone.m4a

python3 - <<'PY'
import struct, zlib

# A JPEG whose EXIF says "rotate 90° clockwise" (orientation 6): stored
# 32x16, displayed 16x32. Built by splicing an APP1 segment in after SOI.
jpeg = open("still.jpg", "rb").read()
tiff = b"MM\x00\x2a" + struct.pack(">I", 8)                 # big-endian TIFF header
tiff += struct.pack(">H", 1)                                  # one IFD entry
tiff += struct.pack(">HHIHH", 0x0112, 3, 1, 6, 0)             # Orientation = 6 (SHORT)
tiff += struct.pack(">I", 0)                                  # no next IFD
app1 = b"Exif\x00\x00" + tiff
segment = b"\xff\xe1" + struct.pack(">H", len(app1) + 2) + app1
open("rotated.jpg", "wb").write(jpeg[:2] + segment + jpeg[2:])

# A JPEG carrying identifying metadata, for metadata stripping and EXIF
# capture: a camera make, a GPS position, an oversized description (over the
# 4 KiB capture limit) and orientation 6, which stripping must keep.
def tiff_ifd(entries, start, extra_base):
    """entries: (tag, type, count, payload bytes). Values over 4 bytes go
    after the IFD; returns (ifd bytes, extra bytes)."""
    ifd = struct.pack(">H", len(entries))
    extra = b""
    end = start + 2 + 12 * len(entries) + 4
    for tag, kind, count, payload in entries:
        if len(payload) <= 4:
            ifd += struct.pack(">HHI", tag, kind, count) + payload.ljust(4, b"\x00")
        else:
            ifd += struct.pack(">HHII", tag, kind, count, end + len(extra))
            extra += payload + (b"\x00" if len(payload) % 2 else b"")
    return ifd + struct.pack(">I", 0), extra

make = b"HaloidCam\x00"
description = b"A" * 5000 + b"\x00"
# IFD0 at 8. Its size is fixed by its entry count, so the GPS IFD's offset
# can be computed before IFD0 is built.
ifd0_entries = 4
ifd0_len = 2 + 12 * ifd0_entries + 4 + len(make) + (len(make) % 2) + len(description) + (len(description) % 2)
gps_offset = 8 + ifd0_len
ifd0, extra0 = tiff_ifd([
    (0x010E, 2, len(description), description),     # ImageDescription
    (0x010F, 2, len(make), make),                   # Make
    (0x0112, 3, 1, struct.pack(">H", 6)),           # Orientation
    (0x8825, 4, 1, struct.pack(">I", gps_offset)),  # GPSInfo IFD pointer
], 8, 0)
latitude = struct.pack(">IIIIII", 51, 1, 30, 1, 0, 1)   # 51° 30' 0"
gps, extra_gps = tiff_ifd([
    (0x0001, 2, 2, b"N\x00"),                       # GPSLatitudeRef
    (0x0002, 5, 3, latitude),                       # GPSLatitude
], gps_offset, 0)
tiff = b"MM\x00\x2a" + struct.pack(">I", 8) + ifd0 + extra0 + gps + extra_gps
app1 = b"Exif\x00\x00" + tiff
segment = b"\xff\xe1" + struct.pack(">H", len(app1) + 2) + app1
comment = b"secret comment"
com = b"\xff\xfe" + struct.pack(">H", len(comment) + 2) + comment
open("exif.jpg", "wb").write(jpeg[:2] + segment + com + jpeg[2:])

# A decompression bomb: a PNG whose header claims 100000x100000 pixels
# (10 gigapixels) over a few bytes of data. Probing must reject it from the
# header alone, never by decoding.
def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
ihdr = struct.pack(">IIBBBBB", 100_000, 100_000, 8, 0, 0, 0, 0)
idat = zlib.compress(b"\x00" * 64)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")
open("bomb.png", "wb").write(png)

# Things that must be refused: an SVG, and an SVG wearing a .jpg name.
svg = b'<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>'
open("xss.svg", "wb").write(svg)
open("svg-named.jpg", "wb").write(svg)
open("not-media.bin", "wb").write(b"\x00\x01\x02 just some bytes that are no format at all")
PY

ls -la
