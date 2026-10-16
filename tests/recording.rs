use bytes::Bytes;
use cctv::{
    config::{Camera, Recording},
    process::{self, ManagedProcess, Output, Pace},
    protocol::{Codec, Frame},
    recording::{Recorder, finalized_mp4},
    storage::{Store, UploadQueue},
};
use std::{path::Path, process::Command, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
fn available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}
fn video(codec: &str) -> Vec<u8> {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=5",
        "-t",
        "2",
        "-threads",
        "1",
    ]);
    if codec == "h264" {
        command.args(["-g", "5", "-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    } else {
        command.args(["-c:v", "mjpeg"]);
    }
    let result = command.args(["-f", codec, "pipe:1"]).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
fn camera() -> Camera {
    serde_json::from_value(serde_json::json!({"deviceId":"123","ip":"127.0.0.1","password":"test","recordingEncoder":"cpu"})).unwrap()
}
fn settings(dir: &Path) -> Recording {
    serde_json::from_value(
        serde_json::json!({"enabled":true,"dir":dir,"segmentSeconds":60,"destination":"local"}),
    )
    .unwrap()
}
async fn recorder(dir: &Path) -> (Recorder, CancellationToken) {
    let settings = settings(dir);
    let stop = CancellationToken::new();
    let queue = UploadQueue::new(Store::new(&settings), true, stop.clone());
    (Recorder::new(camera(), settings, queue), stop)
}
#[tokio::test]
async fn shutdown_drains_and_finalizes_playable_h264_without_reencoding() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut recorder, stop) = recorder(dir.path()).await;
    recorder.start(Some(Codec::H264), false, 5.0).await.unwrap();
    recorder.write(&Frame {
        data: Bytes::from(video("h264")),
        codec: Codec::H264,
        keyframe: true,
    });
    recorder.shutdown().await;
    assert_eq!(recorder.status.segments, 1, "{:?}", recorder.status.error);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|s| s == "mp4"))
        .unwrap()
        .path();
    assert!(finalized_mp4(&file));
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&file)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert!(
        !std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(".partial-"))
    );
    stop.cancel();
}
#[tokio::test]
async fn alaw_audio_is_muxed_into_aac() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut recorder, stop) = recorder(dir.path()).await;
    recorder.start(Some(Codec::H264), true, 5.0).await.unwrap();
    recorder.write(&Frame {
        data: Bytes::from(video("h264")),
        codec: Codec::H264,
        keyframe: true,
    });
    for _ in 0..32 {
        let mut data = vec![0; 16];
        data.extend([0xd5; 480]);
        recorder.audio(Bytes::from(data));
    }
    recorder.shutdown().await;
    assert_eq!(recorder.status.segments, 1, "{:?}", recorder.status.error);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|s| s == "mp4"))
        .unwrap()
        .path();
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("h264") && text.contains("aac"), "{text}");
    stop.cancel();
}
#[tokio::test]
async fn paced_mjpeg_keeps_real_duration_without_25fps_duplication() {
    if !available() {
        return;
    }
    let source = video("mjpeg");
    let mut collector = process::JpegCollector::default();
    let images = collector.push(&source).unwrap();
    assert_eq!(images.len(), 10);
    let dir = tempfile::tempdir().unwrap();
    let (mut recorder, stop) = recorder(dir.path()).await;
    recorder
        .start(Some(Codec::Mjpeg), false, 5.0)
        .await
        .unwrap();
    for data in images {
        recorder.write(&Frame {
            data,
            codec: Codec::Mjpeg,
            keyframe: true,
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    recorder.shutdown().await;
    assert_eq!(recorder.status.segments, 1, "{:?}", recorder.status.error);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|s| s == "mp4"))
        .unwrap()
        .path();
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_name,nb_frames",
            "-of",
            "json",
        ])
        .arg(file)
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let duration = json["format"]["duration"]
        .as_str()
        .unwrap()
        .parse::<f64>()
        .unwrap();
    assert!((1.5..3.0).contains(&duration), "{json}");
    assert_eq!(json["streams"][0]["codec_name"], "h264");
    assert!(
        json["streams"][0]["nb_frames"]
            .as_str()
            .unwrap()
            .parse::<u32>()
            .unwrap()
            <= 12
    );
    stop.cancel();
}
#[tokio::test]
async fn process_with_unclosed_input_is_killed_reaped_and_bounded() {
    let (tx, mut events) = mpsc::channel(16);
    let mut command = tokio::process::Command::new("sleep");
    command.arg("60");
    let mut process =
        ManagedProcess::spawn(command, false, tx, Duration::from_millis(100), 1024).unwrap();
    assert!(!process.input.send(Bytes::from(vec![0; 1025])));
    process.shutdown().await;
    assert!(process.closed());
    assert!(matches!(events.recv().await, Some(Output::Closed(false))));
}

#[tokio::test]
async fn delayed_reader_preserves_accepted_video_beyond_three_seconds() {
    let (tx, mut events) = mpsc::channel(32);
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "sleep 4; cat"]);
    let mut process =
        ManagedProcess::spawn(command, false, tx, Duration::from_secs(2), 8 * 1024 * 1024).unwrap();
    let frame = Bytes::from(vec![0xab; 65536]);
    for chunk in frame.chunks(512) {
        assert!(process.input.send(Bytes::copy_from_slice(chunk)));
    }
    let mut received = vec![];
    tokio::time::timeout(Duration::from_secs(8), async {
        while received.len() < frame.len() {
            match events.recv().await {
                Some(Output::Stdout(data)) => received.extend_from_slice(&data),
                Some(Output::Closed(_)) | None => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    process.shutdown().await;
    assert_eq!(received.as_slice(), frame.as_ref());
    assert!(process.closed());
}

#[tokio::test]
async fn paced_input_spreads_camera_bursts_evenly() {
    let (tx, mut events) = mpsc::channel(256);
    let mut command = tokio::process::Command::new("cat");
    command.kill_on_drop(true);
    let pace = Pace {
        fps: 33.0,
        buffer: Duration::from_millis(500),
    };
    let mut process = ManagedProcess::spawn_paced(
        command,
        false,
        tx,
        Duration::from_secs(2),
        8 * 1024 * 1024,
        Some(pace),
    )
    .unwrap();
    // The camera delivers 20 frames at once, then nothing for 600ms (~33fps on average).
    let input = process.input.clone();
    tokio::spawn(async move {
        for _ in 0..5 {
            for _ in 0..20 {
                assert!(input.send(Bytes::from_static(b"frame")));
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
    });
    let (mut arrivals, mut total) = (vec![], 0);
    tokio::time::timeout(Duration::from_secs(10), async {
        while total < 100 * 5 {
            if let Some(Output::Stdout(data)) = events.recv().await {
                total += data.len();
                arrivals.push(tokio::time::Instant::now());
            }
        }
    })
    .await
    .unwrap();
    process.shutdown().await;
    let longest = arrivals
        .windows(2)
        .map(|w| w[1] - w[0])
        .max()
        .unwrap_or_default();
    // Unpaced, every burst is followed by a ~600ms stall.
    assert!(longest < Duration::from_millis(450), "stall of {longest:?}");
}
