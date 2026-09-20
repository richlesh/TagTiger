//! Minimal ISO-BMFF (MP4) box walker to extract the video track's pixel
//! dimensions from `moov > trak > tkhd`, without pulling in a full demuxer.
//!
//! The track header (`tkhd`) stores `width` and `height` as 16.16 fixed-point
//! values near the end of the box. Audio tracks report 0×0, so the video
//! track is the one with the largest non-zero dimensions.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Read the video track dimensions (width, height) in pixels from an MP4/M4V
/// file. Returns `None` if no sized track is found or on any parse/IO error.
pub fn video_dimensions(path: impl AsRef<Path>) -> Option<(u32, u32)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    // Find the top-level `moov` box, then search within it for `tkhd` boxes.
    let moov = find_box(&mut file, 0, len, *b"moov")?;
    let mut best: Option<(u32, u32)> = None;
    collect_tkhd_dims(&mut file, moov.0, moov.1, &mut best);
    best
}

/// A box's content range: (content_start, content_end).
type Range = (u64, u64);

/// Scan boxes in `[start, end)` for the first box with `fourcc`, returning its
/// content range.
fn find_box(file: &mut File, start: u64, end: u64, fourcc: [u8; 4]) -> Option<Range> {
    let mut pos = start;
    while pos + 8 <= end {
        let (size, kind, content_start) = read_box_header(file, pos, end)?;
        let content_end = pos + size;
        if kind == fourcc {
            return Some((content_start, content_end.min(end)));
        }
        if size == 0 {
            break; // box extends to EOF; avoid infinite loop
        }
        pos += size;
    }
    None
}

/// Recursively collect `tkhd` dimensions within `moov`, keeping the largest.
fn collect_tkhd_dims(file: &mut File, start: u64, end: u64, best: &mut Option<(u32, u32)>) {
    let mut pos = start;
    while pos + 8 <= end {
        let Some((size, kind, content_start)) = read_box_header(file, pos, end) else {
            break;
        };
        let content_end = (pos + size).min(end);
        match &kind {
            b"trak" | b"mdia" | b"minf" => {
                // Container boxes: recurse.
                collect_tkhd_dims(file, content_start, content_end, best);
            }
            b"tkhd" => {
                if let Some((w, h)) = read_tkhd_dims(file, content_start, content_end) {
                    if w > 0 && h > 0 {
                        let area = (w as u64) * (h as u64);
                        let best_area = best.map(|(bw, bh)| (bw as u64) * (bh as u64)).unwrap_or(0);
                        if area > best_area {
                            *best = Some((w, h));
                        }
                    }
                }
            }
            _ => {}
        }
        if size == 0 {
            break;
        }
        pos += size;
    }
}

/// Read a box header at `pos`. Returns (total_size, fourcc, content_start).
/// Handles the 64-bit `largesize` form.
fn read_box_header(file: &mut File, pos: u64, end: u64) -> Option<(u64, [u8; 4], u64)> {
    if pos + 8 > end {
        return None;
    }
    file.seek(SeekFrom::Start(pos)).ok()?;
    let mut hdr = [0u8; 8];
    file.read_exact(&mut hdr).ok()?;
    let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
    let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
    let mut content_start = pos + 8;
    if size == 1 {
        // 64-bit largesize follows the fourcc.
        let mut big = [0u8; 8];
        file.read_exact(&mut big).ok()?;
        size = u64::from_be_bytes(big);
        content_start = pos + 16;
    }
    Some((size, kind, content_start))
}

/// Read width/height (integer part of the 16.16 fixed-point fields) from a
/// `tkhd` box's content range.
fn read_tkhd_dims(file: &mut File, content_start: u64, content_end: u64) -> Option<(u32, u32)> {
    file.seek(SeekFrom::Start(content_start)).ok()?;
    let mut version = [0u8; 1];
    file.read_exact(&mut version).ok()?;
    // width/height are the final 8 bytes of the tkhd content.
    if content_end < content_start + 8 {
        return None;
    }
    file.seek(SeekFrom::Start(content_end - 8)).ok()?;
    let mut wh = [0u8; 8];
    file.read_exact(&mut wh).ok()?;
    let width_fixed = u32::from_be_bytes([wh[0], wh[1], wh[2], wh[3]]);
    let height_fixed = u32::from_be_bytes([wh[4], wh[5], wh[6], wh[7]]);
    // Upper 16 bits are the integer pixel count.
    Some((width_fixed >> 16, height_fixed >> 16))
}
