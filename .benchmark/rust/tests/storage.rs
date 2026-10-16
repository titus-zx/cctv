use cctv::{
    config::Recording,
    recording::finalized_mp4,
    storage::{self, ByteRange, RetentionStatus, Store, UploadQueue},
};
use chrono::{Local, TimeZone, Utc};
use std::{collections::HashSet, path::Path};
use tokio_util::sync::CancellationToken;
fn settings(dir: &Path) -> Recording {
    serde_json::from_value(
        serde_json::json!({"enabled":true,"destination":"local","dir":dir,"retentionDays":7}),
    )
    .unwrap()
}
fn mp4(path: &Path) {
    let mut data = vec![];
    for name in ["ftyp", "moov", "mdat"] {
        data.extend(9u32.to_be_bytes());
        data.extend(name.as_bytes());
        data.push(0);
    }
    std::fs::write(path, data).unwrap();
}
#[test]
fn strict_recording_names_dates_and_ranges() {
    assert!(storage::parse_name("cam123_20261001_000000.mp4", 20).is_some());
    for name in [
        "../cam123_20261001_000000.mp4",
        "cam123_20260230_000000.mp4",
        "cam123_20261001_246000.mp4",
        "cam123_20261001_000000_recovered.mp4",
    ] {
        assert!(storage::parse_name(name, 0).is_none(), "{name}")
    }
    assert_eq!(
        storage::byte_range(Some("bytes=2-6"), 10),
        ByteRange::Part { start: 2, end: 6 }
    );
    assert_eq!(
        storage::byte_range(Some("bytes=-3"), 10),
        ByteRange::Part { start: 7, end: 9 }
    );
    assert_eq!(
        storage::byte_range(Some("bytes=3-"), 10),
        ByteRange::Part { start: 3, end: 9 }
    );
    assert_eq!(
        storage::byte_range(Some("bytes=10-"), 10),
        ByteRange::Unsatisfiable
    );
    assert_eq!(
        storage::byte_range(Some("bytes=-0"), 10),
        ByteRange::Unsatisfiable
    );
    assert_eq!(
        storage::byte_range(Some("bytes=0-1,4-5"), 10),
        ByteRange::Full
    );
}
#[test]
fn mp4_requires_complete_moov_and_mdat() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("video.mp4");
    mp4(&file);
    assert!(finalized_mp4(&file));
    let data = std::fs::read(&file).unwrap();
    std::fs::write(&file, &data[..data.len() - 1]).unwrap();
    assert!(!finalized_mp4(&file));
    std::fs::write(&file, b"invalid").unwrap();
    assert!(!finalized_mp4(&file));
}
#[tokio::test]
async fn upload_recovery_ignores_partial_files_and_preserves_local_clips() {
    let dir = tempfile::tempdir().unwrap();
    let complete = dir.path().join("cam123_20261001_000000.mp4");
    mp4(&complete);
    std::fs::write(dir.path().join("cam123_20261002_000000.mp4"), b"broken").unwrap();
    std::fs::create_dir(dir.path().join(".partial-test")).unwrap();
    mp4(&dir.path().join(".partial-test/cam123_20261003_000000.mp4"));
    let stop = CancellationToken::new();
    let queue = UploadQueue::new(Store::new(&settings(dir.path())), true, stop.clone());
    queue.recover(dir.path()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(queue.status.lock().await.uploaded, 1);
    assert!(complete.exists());
    stop.cancel();
}
#[tokio::test]
async fn retention_preserves_pending_corrupt_other_camera_symlinks_and_active_files() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "cam123_20261001_000000.mp4",
        "cam123_20261007_120000.mp4",
        "cam123_20261002_000000.mp4",
        "cam999_20261001_000000.mp4",
    ] {
        mp4(&dir.path().join(name));
    }
    std::fs::write(dir.path().join("cam123_20261003_000000.mp4"), b"broken").unwrap();
    std::fs::create_dir(dir.path().join(".partial-test")).unwrap();
    mp4(&dir.path().join(".partial-test/cam123_20261001_000000.mp4"));
    std::os::unix::fs::symlink(
        dir.path().join("cam999_20261001_000000.mp4"),
        dir.path().join("cam123_20261004_000000.mp4"),
    )
    .unwrap();
    let store = Store::new(&settings(dir.path()));
    let stop = CancellationToken::new();
    let queue = UploadQueue::new(store.clone(), true, stop.clone());
    queue
        .queued
        .lock()
        .await
        .insert(dir.path().join("cam123_20261002_000000.mp4"));
    let ids = HashSet::from(["123".into()]);
    let now = Local
        .with_ymd_and_hms(2026, 10, 14, 12, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let mut status = RetentionStatus::default();
    storage::clean_retention(&store, &queue, &ids, 7.0, now, &mut status)
        .await
        .unwrap();
    assert_eq!(status.deleted_local, 1);
    assert!(!dir.path().join("cam123_20261001_000000.mp4").exists());
    for name in [
        "cam123_20261007_120000.mp4",
        "cam123_20261002_000000.mp4",
        "cam999_20261001_000000.mp4",
        "cam123_20261003_000000.mp4",
        "cam123_20261004_000000.mp4",
        ".partial-test",
    ] {
        assert!(dir.path().join(name).exists(), "{name}")
    }
    stop.cancel();
}
