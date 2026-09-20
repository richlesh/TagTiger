//! Streaming MP4 layout normalization used by the "shift" save path.
//!
//! When metadata growth would push `mdat` (moov-before-mdat layouts), we avoid
//! buffering the whole `mdat` in memory by streaming the file to a temp copy
//! with `moov` moved to the end. Because `mdat` changes absolute position, the
//! sample chunk-offset tables (`stco`/`co64`) inside the moved `moov` are
//! patched by the fixed byte delta. Afterwards the tag writer only edits the
//! trailing `moov` in place (cheap, no further shift).

use crate::error::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// A top-level box's position and size.
#[derive(Clone, Copy, Debug)]
struct TopBox {
    fourcc: [u8; 4],
    /// Absolute start offset of the box header.
    start: u64,
    /// Total box length in bytes (header + content).
    len: u64,
}

/// The scanned top-level layout of an MP4 file.
struct Layout {
    boxes: Vec<TopBox>,
}

impl Layout {
    fn index_of(&self, fourcc: &[u8; 4]) -> Option<usize> {
        self.boxes.iter().position(|b| &b.fourcc == fourcc)
    }
}

/// Read the box header at `pos`: returns (total_len, fourcc, header_len).
/// Handles the 64-bit `largesize` form. `size == 0` means "to end of file".
fn read_header(file: &mut File, pos: u64, file_len: u64) -> Result<(u64, [u8; 4], u64)> {
    file.seek(SeekFrom::Start(pos))?;
    let mut hdr = [0u8; 8];
    file.read_exact(&mut hdr)?;
    let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
    let fourcc = [hdr[4], hdr[5], hdr[6], hdr[7]];
    let mut header_len = 8u64;
    if size == 1 {
        let mut big = [0u8; 8];
        file.read_exact(&mut big)?;
        size = u64::from_be_bytes(big);
        header_len = 16;
    } else if size == 0 {
        size = file_len - pos;
    }
    Ok((size, fourcc, header_len))
}

/// Scan the top-level boxes of an MP4 file.
fn scan_layout(file: &mut File) -> Result<Layout> {
    let file_len = file.metadata()?.len();
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= file_len {
        let (len, fourcc, _hdr) = read_header(file, pos, file_len)?;
        if len < 8 || pos + len > file_len {
            break;
        }
        boxes.push(TopBox {
            fourcc,
            start: pos,
            len,
        });
        pos += len;
    }
    Ok(Layout { boxes })
}

/// Returns true if the file's `moov` box comes before its `mdat` box, i.e. a
/// metadata growth would require shifting `mdat`.
pub fn moov_precedes_mdat(path: impl AsRef<Path>) -> Result<bool> {
    let mut file = File::open(path)?;
    let layout = scan_layout(&mut file)?;
    let moov = layout.index_of(b"moov");
    let mdat = layout.index_of(b"mdat");
    match (moov, mdat) {
        (Some(m), Some(d)) => Ok(m < d),
        // If either is missing, assume no beneficial shift; let the caller fall
        // back to the in-place writer.
        _ => Ok(false),
    }
}

/// Copy `len` bytes from `src` (at its current position) to `dst`, in bounded
/// chunks, invoking `progress(copied_so_far, total)` periodically.
fn stream_copy(
    src: &mut File,
    dst: &mut File,
    len: u64,
    base: u64,
    grand_total: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    const CHUNK: usize = 1 << 20; // 1 MiB
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = len;
    let mut done = 0u64;
    while remaining > 0 {
        let n = remaining.min(CHUNK as u64) as usize;
        src.read_exact(&mut buf[..n])?;
        dst.write_all(&buf[..n])?;
        remaining -= n as u64;
        done += n as u64;
        progress(base + done, grand_total);
    }
    Ok(())
}

/// Stream-copy `src_path` to `dst_path` in bounded chunks, reporting progress
/// as `(bytes_done, bytes_total)`.
pub fn copy_file_with_progress(
    src_path: &Path,
    dst_path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let mut src = File::open(src_path)?;
    let total = src.metadata()?.len();
    let mut dst = File::create(dst_path)?;
    stream_copy(&mut src, &mut dst, total, 0, total, progress)?;
    dst.flush()?;
    Ok(())
}

/// Rewrite `src_path` into `dst_path` with `moov` moved to the end and its
/// `stco`/`co64` chunk offsets adjusted for the new `mdat` position. Streams
/// large boxes (notably `mdat`) so memory stays bounded. Reports progress via
/// the callback as `(bytes_done, bytes_total)`.
///
/// Returns `Ok(false)` if no reordering is applicable (no moov/mdat, or moov is
/// already last), in which case the caller should use the in-place path.
pub fn normalize_moov_last(
    src_path: &Path,
    dst_path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<bool> {
    let mut src = File::open(src_path)?;
    let layout = scan_layout(&mut src)?;

    let Some(moov_idx) = layout.index_of(b"moov") else {
        return Ok(false);
    };
    let Some(mdat_idx) = layout.index_of(b"mdat") else {
        return Ok(false);
    };
    if moov_idx > mdat_idx {
        return Ok(false); // moov already after mdat
    }

    let moov = layout.boxes[moov_idx];
    let old_moov_start = moov.start;

    // Read the entire moov into memory (metadata is small relative to mdat).
    let mut moov_bytes = vec![0u8; moov.len as usize];
    src.seek(SeekFrom::Start(moov.start))?;
    src.read_exact(&mut moov_bytes)?;

    // Total bytes to stream (everything except moov) for progress accounting.
    let grand_total: u64 = layout
        .boxes
        .iter()
        .filter(|b| b.start != old_moov_start)
        .map(|b| b.len)
        .sum();

    let mut dst = File::create(dst_path)?;
    let mut written = 0u64;

    // Write every non-moov top-level box in original order, streaming content.
    for b in layout.boxes.iter() {
        if b.start == old_moov_start {
            continue;
        }
        src.seek(SeekFrom::Start(b.start))?;
        stream_copy(&mut src, &mut dst, b.len, written, grand_total, progress)?;
        written += b.len;
    }

    // Now append moov at the end. Its new start is the current dst length.
    let new_moov_start = dst.stream_position()?;
    // The mdat content did not change position relative to file start EXCEPT
    // that moov (which used to precede it) is gone from the front. Compute how
    // far every post-moov box moved: everything after the old moov shifted up
    // by moov.len (moved earlier in the file).
    let delta: i64 = -(moov.len as i64);

    // Patch stco/co64 offsets inside the in-memory moov copy. Sample chunk
    // offsets are absolute file offsets into mdat; mdat moved by `delta`.
    patch_chunk_offsets(&mut moov_bytes, delta)?;

    dst.write_all(&moov_bytes)?;
    dst.flush()?;
    let _ = new_moov_start;

    Ok(true)
}

/// Walk the moov box bytes and add `delta` to every entry of every `stco`
/// (32-bit) and `co64` (64-bit) chunk-offset table found within.
///
/// `moov` here is the full outer box (header + children); we descend into it.
fn patch_chunk_offsets(moov: &mut [u8], delta: i64) -> Result<()> {
    // Recursive descent over container boxes to find stco/co64 anywhere.
    fn walk(buf: &mut [u8], delta: i64) -> Result<()> {
        let mut pos = 0usize;
        while pos + 8 <= buf.len() {
            let size =
                u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
            let fourcc = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
            // Only 32-bit sizes are expected inside moov; guard anyway.
            if size < 8 || pos + size > buf.len() {
                break;
            }
            let content = pos + 8;
            let content_end = pos + size;
            match &fourcc {
                b"stco" => patch_stco(&mut buf[content..content_end], delta)?,
                b"co64" => patch_co64(&mut buf[content..content_end], delta)?,
                // Container boxes that can hold stco/co64 (descend into them).
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"edts" => {
                    walk(&mut buf[content..content_end], delta)?
                }
                _ => {}
            }
            pos += size;
        }
        Ok(())
    }
    walk(moov, delta)
}

fn patch_stco(content: &mut [u8], delta: i64) -> Result<()> {
    // content: version(1) flags(3) entry_count(4) then entry_count * u32.
    if content.len() < 8 {
        return Err(Error::Other("stco too small".into()));
    }
    let count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]) as usize;
    let mut off = 8;
    for _ in 0..count {
        if off + 4 > content.len() {
            break;
        }
        let v = u32::from_be_bytes([
            content[off],
            content[off + 1],
            content[off + 2],
            content[off + 3],
        ]);
        let nv = (v as i64 + delta) as u32;
        content[off..off + 4].copy_from_slice(&nv.to_be_bytes());
        off += 4;
    }
    Ok(())
}

fn patch_co64(content: &mut [u8], delta: i64) -> Result<()> {
    if content.len() < 8 {
        return Err(Error::Other("co64 too small".into()));
    }
    let count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]) as usize;
    let mut off = 8;
    for _ in 0..count {
        if off + 8 > content.len() {
            break;
        }
        let v = u64::from_be_bytes([
            content[off],
            content[off + 1],
            content[off + 2],
            content[off + 3],
            content[off + 4],
            content[off + 5],
            content[off + 6],
            content[off + 7],
        ]);
        let nv = (v as i64 + delta) as u64;
        content[off..off + 8].copy_from_slice(&nv.to_be_bytes());
        off += 8;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_stco_offsets() {
        // version+flags(4) + count(4)=2 + two u32 offsets.
        let mut content = Vec::new();
        content.extend_from_slice(&[0, 0, 0, 0]); // version/flags
        content.extend_from_slice(&2u32.to_be_bytes()); // count
        content.extend_from_slice(&1000u32.to_be_bytes());
        content.extend_from_slice(&2000u32.to_be_bytes());
        patch_stco(&mut content, -100).unwrap();
        let o1 = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);
        let o2 = u32::from_be_bytes([content[12], content[13], content[14], content[15]]);
        assert_eq!(o1, 900);
        assert_eq!(o2, 1900);
    }

    #[test]
    fn patches_co64_offsets() {
        let mut content = Vec::new();
        content.extend_from_slice(&[0, 0, 0, 0]);
        content.extend_from_slice(&1u32.to_be_bytes());
        content.extend_from_slice(&5_000_000_000u64.to_be_bytes());
        patch_co64(&mut content, 256).unwrap();
        let o = u64::from_be_bytes([
            content[8],
            content[9],
            content[10],
            content[11],
            content[12],
            content[13],
            content[14],
            content[15],
        ]);
        assert_eq!(o, 5_000_000_256);
    }

    #[test]
    fn walks_nested_stco() {
        // moov { trak { mdia { minf { stbl { stco } } } } }
        fn box_wrap(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
            let size = (8 + content.len()) as u32;
            let mut v = Vec::new();
            v.extend_from_slice(&size.to_be_bytes());
            v.extend_from_slice(fourcc);
            v.extend_from_slice(content);
            v
        }
        let mut stco_content = Vec::new();
        stco_content.extend_from_slice(&[0, 0, 0, 0]);
        stco_content.extend_from_slice(&1u32.to_be_bytes());
        stco_content.extend_from_slice(&4000u32.to_be_bytes());
        let stco = box_wrap(b"stco", &stco_content);
        let stbl = box_wrap(b"stbl", &stco);
        let minf = box_wrap(b"minf", &stbl);
        let mdia = box_wrap(b"mdia", &minf);
        let trak = box_wrap(b"trak", &mdia);
        let mut moov = box_wrap(b"moov", &trak);
        // patch expects the moov *content* (children), so pass past the header.
        let content_start = 8;
        patch_chunk_offsets(&mut moov[content_start..], -500).unwrap();
        // Find the stco offset near the end and verify it changed.
        let last4 = moov.len() - 4;
        let v = u32::from_be_bytes([
            moov[last4],
            moov[last4 + 1],
            moov[last4 + 2],
            moov[last4 + 3],
        ]);
        assert_eq!(v, 3500);
    }
}
