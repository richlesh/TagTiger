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

/// Returns true if the file is "fast start" (web-optimized): its `moov` box
/// precedes its `mdat`, so a streaming client can read the sample tables before
/// the media. Equivalent to [`moov_precedes_mdat`] but named for the concept.
pub fn is_fast_start(path: impl AsRef<Path>) -> Result<bool> {
    moov_precedes_mdat(path)
}

/// Rewrite `src_path` into `dst_path` with `moov` moved to the **front**
/// (before `mdat`), producing a "fast start" / web-optimized layout:
///
///   [ftyp] [moov] [free padding] [mdat] [any trailing boxes]
///
/// The `stco`/`co64` chunk-offset tables inside `moov` are patched by the exact
/// byte delta that `mdat` moved. A small `free` box is written after `moov`;
/// this is conventional padding in fast-start files. Note that `mp4ameta` does
/// not reclaim adjacent `free` space when it grows `moov` (it shifts `mdat`
/// instead), so this padding does not, by itself, make later edits cheaper —
/// it is kept only for layout conventionality.
///
/// Streams large boxes (notably `mdat`) so memory stays bounded. Reports
/// progress via the callback as `(bytes_done, bytes_total)`.
///
/// Returns `Ok(false)` if no reordering is applicable (missing moov/mdat).
pub fn normalize_moov_first(
    src_path: &Path,
    dst_path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<bool> {
    /// Small conventional `free` box written after `moov`. Must be >= 8 (a
    /// `free` box header). Kept minimal since mp4ameta does not reclaim it.
    const FREE_PAD: u64 = 8;

    let mut src = File::open(src_path)?;
    let layout = scan_layout(&mut src)?;

    let Some(moov_idx) = layout.index_of(b"moov") else {
        return Ok(false);
    };
    let Some(mdat_idx) = layout.index_of(b"mdat") else {
        return Ok(false);
    };

    let moov = layout.boxes[moov_idx];
    let mdat = layout.boxes[mdat_idx];
    let old_mdat_start = mdat.start;

    // Read the entire moov into memory (metadata is small relative to mdat).
    let mut moov_bytes = vec![0u8; moov.len as usize];
    src.seek(SeekFrom::Start(moov.start))?;
    src.read_exact(&mut moov_bytes)?;

    // Determine the output box order: [ftyp?] [moov] [free FREE_PAD] then every
    // remaining box (mdat and any others such as pre-existing `free` boxes) in
    // their original relative order. We must compute where `mdat` actually
    // lands in this output — accounting for *every* box written before it, not
    // just ftyp+moov — so the chunk-offset delta is exact. Getting this wrong
    // (e.g. ignoring a pre-existing `free` box between ftyp and mdat) shifts
    // the offsets and produces a file that decodes to nothing.
    let ftyp_idx = layout.index_of(b"ftyp");

    // Walk the planned output, summing box lengths until we reach mdat, to get
    // its true new absolute start.
    let mut new_mdat_start = 0u64;
    if let Some(i) = ftyp_idx {
        new_mdat_start += layout.boxes[i].len;
    }
    new_mdat_start += moov.len; // moov (unchanged length; we only patch offsets)
    new_mdat_start += FREE_PAD; // our inserted free box
    for (i, b) in layout.boxes.iter().enumerate() {
        if i == moov_idx || Some(i) == ftyp_idx {
            continue;
        }
        if i == mdat_idx {
            break; // reached mdat in output order
        }
        new_mdat_start += b.len; // e.g. a pre-existing free box before mdat
    }

    let delta: i64 = new_mdat_start as i64 - old_mdat_start as i64;
    patch_chunk_offsets(&mut moov_bytes, delta)?;

    // Progress accounting: total bytes streamed = everything except moov (moov
    // is written from memory).
    let grand_total: u64 = layout
        .boxes
        .iter()
        .filter(|b| b.start != moov.start)
        .map(|b| b.len)
        .sum();
    let mut written = 0u64;

    let mut dst = File::create(dst_path)?;

    // 1) ftyp first, if present.
    if let Some(i) = ftyp_idx {
        let b = layout.boxes[i];
        src.seek(SeekFrom::Start(b.start))?;
        stream_copy(&mut src, &mut dst, b.len, written, grand_total, progress)?;
        written += b.len;
    }

    // 2) moov (from the patched in-memory copy).
    dst.write_all(&moov_bytes)?;

    // 3) free padding box: header (size + 'free') then zeroed body.
    write_free_box(&mut dst, FREE_PAD)?;

    // 4) every remaining box (mdat and any others) in original order.
    for (i, b) in layout.boxes.iter().enumerate() {
        if i == moov_idx || Some(i) == ftyp_idx {
            continue;
        }
        src.seek(SeekFrom::Start(b.start))?;
        stream_copy(&mut src, &mut dst, b.len, written, grand_total, progress)?;
        written += b.len;
    }

    dst.flush()?;
    Ok(true)
}

/// Write a `free` box of exactly `total_len` bytes (including its 8-byte
/// header). `total_len` must be >= 8.
fn write_free_box(dst: &mut File, total_len: u64) -> Result<()> {
    debug_assert!(total_len >= 8);
    let mut hdr = [0u8; 8];
    hdr[..4].copy_from_slice(&(total_len as u32).to_be_bytes());
    hdr[4..].copy_from_slice(b"free");
    dst.write_all(&hdr)?;
    // Body: total_len - 8 zero bytes, written in bounded chunks.
    let mut remaining = total_len - 8;
    let zeros = [0u8; 4096];
    while remaining > 0 {
        let n = remaining.min(zeros.len() as u64) as usize;
        dst.write_all(&zeros[..n])?;
        remaining -= n as u64;
    }
    Ok(())
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

    fn box_wrap(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
        let size = (8 + content.len()) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(fourcc);
        v.extend_from_slice(content);
        v
    }

    /// Building a moov-before-mdat file, normalizing it, and confirming the
    /// result is moov-after-mdat is the guarantee that makes a *second* save
    /// take the cheap in-place path instead of rewriting again.
    #[test]
    fn normalize_moves_moov_after_mdat_and_patches_offsets() {
        use std::io::Write;

        // Synthetic layout: ftyp | moov{trak/mdia/minf/stbl/stco} | mdat.
        // The single stco entry points at the mdat *content* absolute offset.
        let ftyp = box_wrap(b"ftyp", &[b'i', b's', b'o', b'm', 0, 0, 0, 0]);

        // Placeholder stco (offset patched below once we know mdat's position).
        let mut stco_content = vec![0u8, 0, 0, 0]; // version/flags
        stco_content.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
        stco_content.extend_from_slice(&0u32.to_be_bytes()); // placeholder offset
        let stco = box_wrap(b"stco", &stco_content);
        let stbl = box_wrap(b"stbl", &stco);
        let minf = box_wrap(b"minf", &stbl);
        let mdia = box_wrap(b"mdia", &minf);
        let trak = box_wrap(b"trak", &mdia);
        let mut moov = box_wrap(b"moov", &trak);

        let mdat_payload = b"MEDIA-SAMPLE-BYTES";
        let mdat = box_wrap(b"mdat", mdat_payload);

        // Original absolute offset of the mdat *content* (payload starts after
        // ftyp + moov + the 8-byte mdat header).
        let mdat_content_off = (ftyp.len() + moov.len() + 8) as u32;
        // Patch the stco entry (last 4 bytes of moov) to that offset.
        let n = moov.len();
        moov[n - 4..].copy_from_slice(&mdat_content_off.to_be_bytes());

        let dir = std::env::temp_dir();
        let src = dir.join(format!("tagtiger_norm_src_{}.mp4", std::process::id()));
        let dst = dir.join(format!("tagtiger_norm_dst_{}.mp4", std::process::id()));
        {
            let mut f = File::create(&src).unwrap();
            f.write_all(&ftyp).unwrap();
            f.write_all(&moov).unwrap();
            f.write_all(&mdat).unwrap();
        }

        // Precondition: moov precedes mdat (the shift path would trigger).
        assert!(moov_precedes_mdat(&src).unwrap());

        let mut prog = |_: u64, _: u64| {};
        let normalized = normalize_moov_last(&src, &dst, &mut prog).unwrap();
        assert!(normalized, "normalization should apply to a moov-first file");

        // Postcondition: moov now comes after mdat, so a subsequent write takes
        // the in-place path.
        assert!(
            !moov_precedes_mdat(&dst).unwrap(),
            "after normalization, moov must follow mdat"
        );

        // The stco offset must have been patched so it still points at the same
        // mdat content. In the output, mdat sits right after ftyp, so its
        // content begins at ftyp.len() + 8.
        let out = std::fs::read(&dst).unwrap();
        let mut layout_file = File::open(&dst).unwrap();
        let layout = scan_layout(&mut layout_file).unwrap();
        let moov_box = layout.boxes[layout.index_of(b"moov").unwrap()];
        // stco offset is the last 4 bytes of the relocated moov box.
        let patched_off = {
            let end = (moov_box.start + moov_box.len) as usize;
            u32::from_be_bytes([out[end - 4], out[end - 3], out[end - 2], out[end - 1]])
        };
        let expected_off = (ftyp.len() + 8) as u32;
        assert_eq!(
            patched_off, expected_off,
            "stco offset should track mdat's new absolute position"
        );
        // And it should indeed point at the preserved payload.
        assert_eq!(
            &out[patched_off as usize..patched_off as usize + mdat_payload.len()],
            mdat_payload
        );

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    /// Building a moov-last file, normalizing it to fast-start, and confirming
    /// the result is moov-first with a `free` pad and correctly patched chunk
    /// offsets is the guarantee behind the "Fast-start" checkbox.
    #[test]
    fn normalize_moves_moov_before_mdat_and_patches_offsets() {
        use std::io::Write;

        // Synthetic moov-last layout with a pre-existing `free` box between
        // ftyp and mdat — exactly what ffmpeg emits. This box must be counted
        // when computing mdat's new position, or the chunk offsets end up off
        // by its size and the file decodes to nothing.
        let ftyp = box_wrap(b"ftyp", &[b'i', b's', b'o', b'm', 0, 0, 0, 0]);
        let pre_free = box_wrap(b"free", &[0u8; 4]); // 12-byte free box
        let mdat_payload = b"MEDIA-SAMPLE-BYTES-XYZ";
        let mdat = box_wrap(b"mdat", mdat_payload);

        // Original mdat content offset = ftyp + pre_free + mdat header (8).
        let old_mdat_content_off = (ftyp.len() + pre_free.len() + 8) as u32;

        let mut stco_content = vec![0u8, 0, 0, 0];
        stco_content.extend_from_slice(&1u32.to_be_bytes()); // one entry
        stco_content.extend_from_slice(&old_mdat_content_off.to_be_bytes());
        let stco = box_wrap(b"stco", &stco_content);
        let stbl = box_wrap(b"stbl", &stco);
        let minf = box_wrap(b"minf", &stbl);
        let mdia = box_wrap(b"mdia", &minf);
        let trak = box_wrap(b"trak", &mdia);
        let moov = box_wrap(b"moov", &trak);

        let dir = std::env::temp_dir();
        let src = dir.join(format!("tagtiger_ff_src_{}.mp4", std::process::id()));
        let dst = dir.join(format!("tagtiger_ff_dst_{}.mp4", std::process::id()));
        {
            let mut f = File::create(&src).unwrap();
            f.write_all(&ftyp).unwrap();
            f.write_all(&pre_free).unwrap();
            f.write_all(&mdat).unwrap();
            f.write_all(&moov).unwrap();
        }

        // Precondition: not fast-start (moov after mdat).
        assert!(!is_fast_start(&src).unwrap());

        let mut prog = |_: u64, _: u64| {};
        let normalized = normalize_moov_first(&src, &dst, &mut prog).unwrap();
        assert!(normalized);

        // Postcondition: now fast-start (moov before mdat).
        assert!(
            is_fast_start(&dst).unwrap(),
            "after normalization, moov must precede mdat"
        );

        // The stco offset must now point at mdat's new content position, and
        // that position must still contain the original payload — this is the
        // key regression check: it fails if a box before mdat (the pre-existing
        // free) isn't accounted for in the delta.
        let mut df = File::open(&dst).unwrap();
        let layout = scan_layout(&mut df).unwrap();
        let out = std::fs::read(&dst).unwrap();
        let mdat_box = layout.boxes[layout.index_of(b"mdat").unwrap()];
        let new_content_off = (mdat_box.start + 8) as u32;
        let moov_box = layout.boxes[layout.index_of(b"moov").unwrap()];
        let end = (moov_box.start + moov_box.len) as usize;
        let patched_off =
            u32::from_be_bytes([out[end - 4], out[end - 3], out[end - 2], out[end - 1]]);
        assert_eq!(
            patched_off, new_content_off,
            "stco offset should track mdat's new absolute position"
        );
        assert_eq!(
            &out[patched_off as usize..patched_off as usize + mdat_payload.len()],
            mdat_payload
        );

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }
}
