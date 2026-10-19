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
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        while queue.status.lock().await.uploaded != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("recovered clip should upload after analysis drains");
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

#[tokio::test]
async fn index_persists_lists_by_day_and_prunes_vanished_clips() {
    use cctv::index::Index;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".index.json");
    let index = Index::new(Some(path.clone()));
    assert!(!index.ready());
    index.sync(vec![
        ("cam1_20200101_100000.mp4".into(), 10),
        ("cam1_20200101_110000.mp4".into(), 20),
        ("cam1_20200102_100000.mp4".into(), 30),
        ("notaclip.mp4".into(), 1),
    ]);
    assert!(index.ready());
    assert_eq!(index.day("20200101").len(), 2);
    assert_eq!(index.size("cam1_20200102_100000.mp4"), Some(30));
    assert_eq!(index.size("notaclip.mp4"), None);
    index.persist().await;
    // A restart serves the page immediately from disk.
    let reloaded = Index::new(Some(path));
    assert!(reloaded.ready());
    assert_eq!(reloaded.day("20200101").len(), 2);
    // A later listing drops old clips that vanished (e.g. retention) but keeps brand-new ones.
    let now = chrono::Local::now().format("cam1_%Y%m%d_%H%M%S.mp4").to_string();
    reloaded.add(&now, 5);
    reloaded.sync(vec![("cam1_20200101_100000.mp4".into(), 10)]);
    assert_eq!(reloaded.day("20200101").len(), 1);
    assert!(reloaded.day("20200102").is_empty());
    assert_eq!(reloaded.size(&now), Some(5));
}

fn old(path: &Path) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
}
#[tokio::test]
async fn thumbnails_and_index_entries_follow_retention() {
    let dir = tempfile::tempdir().unwrap();
    let thumbs = dir.path().join(".thumbs");
    std::fs::create_dir(&thumbs).unwrap();
    let store = Store::new(&settings(dir.path()));
    let expired = "cam123_20261001_000000";
    let fresh = "cam123_20261013_100000";
    for name in [expired, fresh] {
        mp4(&dir.path().join(format!("{name}.mp4")));
        std::fs::write(thumbs.join(format!("{name}.jpg")), b"jpg").unwrap();
        store.index.add(&format!("{name}.mp4"), 27);
        store.index.set_motion(&format!("{name}.mp4"), 0.5);
    }
    // Orphans: the clip itself is gone. Only the one past the grace period is removed.
    std::fs::write(thumbs.join("cam123_20261012_100000.jpg"), b"jpg").unwrap();
    old(&thumbs.join("cam123_20261012_100000.jpg"));
    std::fs::write(thumbs.join("cam123_20261012_110000.jpg"), b"jpg").unwrap();
    std::fs::write(thumbs.join(".tmp-cam123_20261012_120000.jpg"), b"x").unwrap();
    old(&thumbs.join(".tmp-cam123_20261012_120000.jpg"));
    let stop = CancellationToken::new();
    let queue = UploadQueue::new(store.clone(), true, stop.clone());
    let now = Local
        .with_ymd_and_hms(2026, 10, 14, 12, 0, 0)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let mut status = RetentionStatus::default();
    storage::clean_retention(
        &store,
        &queue,
        &HashSet::from(["123".into()]),
        7.0,
        now,
        &mut status,
    )
    .await
    .unwrap();
    let left = |name: &str| thumbs.join(name).exists();
    assert!(!left(&format!("{expired}.jpg")));
    assert!(left(&format!("{fresh}.jpg")));
    assert!(!left("cam123_20261012_100000.jpg"));
    assert!(left("cam123_20261012_110000.jpg"));
    assert!(!left(".tmp-cam123_20261012_120000.jpg"));
    assert_eq!(store.index.motion(&format!("{expired}.mp4")), None);
    assert_eq!(store.index.motion(&format!("{fresh}.mp4")), Some(0.5));
    stop.cancel();
}
#[tokio::test]
async fn thumbnail_rejects_scene_changes_without_recognized_objects() {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        return;
    }
    let model = std::env::var("MOTION_MODEL_PATH").unwrap_or_else(|_| "models/yolov8n.onnx".into());
    if !Path::new(&model).exists() {
        eprintln!("Model unavailable; run sh tools/fetch-motion-model.sh to enable ONNX integration test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let make = |name: &str, source: &str| {
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", source, "-t", "2", "-g", "10"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(dir.path().join(name))
            .status()
            .unwrap();
        assert!(status.success());
    };
    make("cam1_20260101_100000.mp4", "color=c=gray:size=320x240:rate=10");
    make("cam1_20260101_110000.mp4", "testsrc=size=320x240:rate=10");
    let store = Store::new(&settings(dir.path()));
    for name in ["cam1_20260101_100000.mp4", "cam1_20260101_110000.mp4"] {
        store.index.add(name, 1);
        store.ensure_thumb(&dir.path().join(name)).await;
        let thumb = cctv::thumbs::thumb_path(dir.path(), name).unwrap();
        assert!(thumb.metadata().unwrap().len() > 100, "{name}");
    }
    let still = store.index.motion("cam1_20260101_100000.mp4").unwrap();
    let moving = store.index.motion("cam1_20260101_110000.mp4").unwrap();
    assert!(still < storage::MOTION_THRESHOLD, "{still}");
    assert!(moving < storage::MOTION_THRESHOLD, "scene change alone must not count: {moving}");
}

#[test]
fn legacy_scene_scores_are_not_object_detections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".index.json");
    std::fs::write(&path, r#"{"clips":{"cam1_20260101_100000.mp4":{"size":10,"motion":0.9}}}"#).unwrap();
    let index = cctv::index::Index::new(Some(path));
    assert_eq!(index.motion("cam1_20260101_100000.mp4"), None);
    index.set_motion("cam1_20260101_100000.mp4", 0.9);
    assert_eq!(index.motion("cam1_20260101_100000.mp4"), Some(0.9));
}

/// Optional real-image smoke test: MOTION_TEST_IMAGE=/path/to/dog.jpg cargo test real_dog -- --nocapture
#[tokio::test]
async fn real_dog_must_move_before_being_labelled_motion() {
    let Ok(image) = std::env::var("MOTION_TEST_IMAGE") else { return; };
    let dir = tempfile::tempdir().unwrap();
    for moving in [false, true] {
        let video = dir.path().join(if moving { "moving.mp4" } else { "still.mp4" });
        let filter = if moving {
            "scale=320:320:force_original_aspect_ratio=decrease,pad=960:640:x=400:y=160:color=gray,crop=640:640:x=320-40*t:y=0"
        } else {
            "scale=320:320:force_original_aspect_ratio=decrease,pad=640:640:x=160:y=160:color=gray"
        };
        let status = std::process::Command::new("ffmpeg").args(["-v","error","-loop","1","-i"])
            .arg(&image).args(["-vf",filter,"-t","3","-r","4","-c:v","libx264","-pix_fmt","yuv420p"])
            .arg(&video).status().unwrap();
        assert!(status.success());
        let (at, score) = cctv::motion::analyze(&video).await.unwrap();
        eprintln!("real dog moving={moving}: at={at}, confidence={score}");
        if moving { assert!(score >= storage::MOTION_THRESHOLD); }
        else { assert_eq!(score, 0.); }
    }
}
