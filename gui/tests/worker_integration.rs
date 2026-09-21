//! Integration smoke test for the background worker's file-load path — the
//! exact data the Slint controller consumes on `Event::FileLoaded`.
//!
//! Uses a tiny MP4 generated at test time via `ffmpeg`. If `ffmpeg` isn't on
//! PATH (e.g. minimal CI), the test is skipped rather than failing.

use std::process::Command;
use std::time::Duration;

// Pull in the worker module directly by path so we don't need the crate to be
// a library. Mirrors how the binary declares it.
#[path = "../src/worker.rs"]
mod worker;

use worker::{Event, Request, Worker};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn worker_loads_file_and_reports_metadata() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not available");
        return;
    }

    // Generate a tiny tagged MP4 in a temp dir.
    let dir = std::env::temp_dir().join("tagtiger_worker_it");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("Smoke Test (2001).mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=320x240:d=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-metadata",
            "title=Smoke Test Movie",
        ])
        .arg(&file)
        .output()
        .expect("run ffmpeg");
    assert!(status.status.success(), "ffmpeg failed to create test mp4");

    // Spawn the worker with no credential (offline path only) and open the file.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let worker = Worker::spawn(String::new(), || {});
    worker
        .tx
        .send(Request::OpenFile { path: file.clone() })
        .unwrap();

    // Await the FileLoaded event.
    let evt = worker
        .rx
        .recv_timeout(Duration::from_secs(20))
        .expect("worker should emit an event");

    match evt {
        Event::FileLoaded {
            file: loaded,
            meta,
            suggested_query,
            video_dimensions,
            ..
        } => {
            assert_eq!(loaded, file);
            // Title was written by ffmpeg; the worker reads it back.
            assert_eq!(meta.title, "Smoke Test Movie");
            // Suggested query is the existing title (non-empty).
            assert_eq!(suggested_query, "Smoke Test Movie");
            // Dimensions are detected from the video track.
            assert_eq!(video_dimensions, Some((320, 240)));
        }
        _ => panic!("expected FileLoaded, got a different event"),
    }

    let _ = std::fs::remove_file(&file);
}
