//! End-to-end check that repeatedly writing the same metadata to a fast-start
//! file does NOT keep shifting the media (`mdat`), and that the file stays
//! fast-start. Requires a real MP4 seed at `$TT_VERIFY_MP4` (produced by e.g.
//! `ffmpeg -f lavfi -i testsrc=... -movflags +faststart seed.mp4`). The test
//! is skipped when that env var is unset, so it never breaks CI.

use std::path::Path;

use tagtiger_core::artwork::EncodedArtwork;
use tagtiger_core::model::MediaMetadata;
use tagtiger_core::{mp4rewrite, tag};

/// Absolute file offset of the top-level `mdat` box, via a tiny box scan.
fn mdat_offset(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr).ok()?;
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let fourcc = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let mut header = 8u64;
        if size == 1 {
            let mut big = [0u8; 8];
            f.read_exact(&mut big).ok()?;
            size = u64::from_be_bytes(big);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        let _ = header;
        if &fourcc == b"mdat" {
            return Some(pos);
        }
        if size < 8 {
            break;
        }
        pos += size;
    }
    None
}

fn sample_meta() -> MediaMetadata {
    MediaMetadata {
        title: "Fast Start Repeat Test".into(),
        release_date: chrono::NaiveDate::from_ymd_opt(2001, 1, 1),
        summary: Some("Same metadata written repeatedly.".into()),
        ..Default::default()
    }
}

#[test]
fn repeat_faststart_write_does_not_shift_mdat() {
    let Ok(seed) = std::env::var("TT_VERIFY_MP4") else {
        eprintln!("skipping: set TT_VERIFY_MP4 to a real fast-start mp4 to run");
        return;
    };
    let seed = std::path::PathBuf::from(seed);
    assert!(seed.exists(), "TT_VERIFY_MP4 does not exist: {seed:?}");

    // Work on a copy so we don't mutate the seed.
    let work = std::env::temp_dir().join("tt_faststart_repeat_work.mp4");
    std::fs::copy(&seed, &work).unwrap();

    let meta = sample_meta();
    let no_art: Option<&EncodedArtwork> = None;

    // First write as fast-start (may rewrite once to normalize the layout).
    tag::write_to_file(&work, &meta, no_art, true).unwrap();
    assert!(
        mp4rewrite::is_fast_start(&work).unwrap(),
        "file should be fast-start after first write"
    );
    let off_after_first = mdat_offset(&work).expect("mdat present");

    // Second write of the SAME metadata: must not move mdat, and must stay
    // fast-start.
    tag::write_to_file(&work, &meta, no_art, true).unwrap();
    assert!(
        mp4rewrite::is_fast_start(&work).unwrap(),
        "file should remain fast-start after second write"
    );
    let off_after_second = mdat_offset(&work).expect("mdat present");
    assert_eq!(
        off_after_first, off_after_second,
        "second identical fast-start write must not shift mdat"
    );

    // Third write, still identical: still stable.
    tag::write_to_file(&work, &meta, no_art, true).unwrap();
    let off_after_third = mdat_offset(&work).expect("mdat present");
    assert_eq!(
        off_after_second, off_after_third,
        "third identical fast-start write must not shift mdat"
    );

    let _ = std::fs::remove_file(&work);
}

#[test]
fn toggle_to_moov_last_then_repeat_is_stable() {
    let Ok(seed) = std::env::var("TT_VERIFY_MP4") else {
        eprintln!("skipping: set TT_VERIFY_MP4 to a real fast-start mp4 to run");
        return;
    };
    let seed = std::path::PathBuf::from(seed);
    assert!(seed.exists(), "TT_VERIFY_MP4 does not exist: {seed:?}");

    let work = std::env::temp_dir().join("tt_faststart_toggle_work.mp4");
    std::fs::copy(&seed, &work).unwrap();

    let meta = sample_meta();
    let no_art: Option<&EncodedArtwork> = None;

    // Toggle OFF fast-start: file must become moov-last.
    tag::write_to_file(&work, &meta, no_art, false).unwrap();
    assert!(
        !mp4rewrite::is_fast_start(&work).unwrap(),
        "file should be moov-last after writing with fast_start=false"
    );
    let off_first = mdat_offset(&work).expect("mdat present");

    // Repeat identical moov-last write: mdat must not move.
    tag::write_to_file(&work, &meta, no_art, false).unwrap();
    assert!(!mp4rewrite::is_fast_start(&work).unwrap());
    let off_second = mdat_offset(&work).expect("mdat present");
    assert_eq!(
        off_first, off_second,
        "second identical moov-last write must not shift mdat"
    );

    let _ = std::fs::remove_file(&work);
}

/// Convert a (moov-last) seed to fast-start and leave the result at
/// `$TT_OUT_MP4` so an external tool (ffmpeg) can verify it still decodes.
/// Skipped unless both TT_VERIFY_MP4 and TT_OUT_MP4 are set.
#[test]
fn convert_to_faststart_writes_decodable_output() {
    let (Ok(seed), Ok(out)) = (std::env::var("TT_VERIFY_MP4"), std::env::var("TT_OUT_MP4")) else {
        eprintln!("skipping: set TT_VERIFY_MP4 and TT_OUT_MP4 to run");
        return;
    };
    let seed = std::path::PathBuf::from(seed);
    let out = std::path::PathBuf::from(out);
    std::fs::copy(&seed, &out).unwrap();

    let meta = sample_meta();
    let no_art: Option<&EncodedArtwork> = None;

    // Ensure it starts moov-last, then convert to fast-start.
    tag::write_to_file(&out, &meta, no_art, false).unwrap();
    assert!(!mp4rewrite::is_fast_start(&out).unwrap());
    tag::write_to_file(&out, &meta, no_art, true).unwrap();
    assert!(
        mp4rewrite::is_fast_start(&out).unwrap(),
        "output must be fast-start"
    );
    // The caller (test harness) runs ffmpeg on $TT_OUT_MP4 to confirm it
    // decodes to real (non-black) frames.
}
