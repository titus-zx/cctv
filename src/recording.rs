use crate::{
    config::{Camera, Recording},
    process::{self, ManagedProcess, Output, Pace},
    protocol::{Codec, Frame},
    storage::{UploadQueue, parse_name},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::sync::{OnceCell, mpsc};

pub fn finalized_mp4(file: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(file) else {
        return false;
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let (mut offset, mut moov, mut mdat) = (0u64, false, false);
    let mut header = [0u8; 16];
    while offset + 8 <= size {
        if file.seek(SeekFrom::Start(offset)).is_err() || file.read_exact(&mut header[..8]).is_err()
        {
            return false;
        }
        let mut length = u32::from_be_bytes(header[..4].try_into().unwrap()) as u64;
        let mut minimum = 8;
        if length == 1 {
            if offset + 16 > size || file.read_exact(&mut header[8..]).is_err() {
                return false;
            }
            length = u64::from_be_bytes(header[8..].try_into().unwrap());
            minimum = 16;
        } else if length == 0 {
            length = size - offset;
        }
        if length < minimum || length > size - offset {
            return false;
        }
        if &header[4..8] == b"moov" && length > minimum {
            moov = true
        }
        if &header[4..8] == b"mdat" && length > minimum {
            mdat = true
        }
        offset += length;
    }
    offset == size && moov && mdat
}
static HARDWARE: OnceCell<bool> = OnceCell::const_new();
pub async fn hardware_available() -> bool {
    *HARDWARE
        .get_or_init(|| async {
            if !Path::new("/dev/dri/renderD128").exists() {
                return false;
            }
            let mut command = process::ffmpeg();
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-vaapi_device",
                    "/dev/dri/renderD128",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=size=64x64:rate=1",
                    "-vf",
                    "format=nv12,hwupload",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "h264_vaapi",
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let Ok(mut child) = command.spawn() else {
                return false;
            };
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => status.success(),
                _ => {
                    let _ = child.kill().await;
                    false
                }
            }
        })
        .await
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatus {
    pub segments: u64,
    pub dropped_frames: u64,
    pub restarts: u64,
    pub encoder: Option<String>,
    pub error: Option<String>,
}
pub struct Recorder {
    pub process: Option<ManagedProcess>,
    pub status: RecorderStatus,
    pub events: mpsc::Receiver<Output>,
    pub last_progress: Instant,
    staging: Option<PathBuf>,
    pending: String,
    settings: Recording,
    camera: Camera,
    queue: UploadQueue,
    software_only: bool,
    stopping: bool,
}
impl Recorder {
    pub fn new(camera: Camera, settings: Recording, queue: UploadQueue) -> Self {
        let (_, events) = mpsc::channel(64);
        Self {
            process: None,
            status: RecorderStatus::default(),
            events,
            last_progress: Instant::now(),
            staging: None,
            pending: String::new(),
            settings,
            camera,
            queue,
            software_only: false,
            stopping: false,
        }
    }
    pub fn active(&self) -> bool {
        self.process.is_some() && !self.stopping
    }
    pub fn ready(&self) -> bool {
        self.process.is_none()
    }
    pub async fn start(&mut self, codec: Option<Codec>, audio: bool, fps: f64) -> Result<()> {
        if self.process.is_some() {
            return Ok(());
        }
        let audio = audio && self.settings.audio && self.camera.audio && codec.is_some();
        let hardware = codec == Some(Codec::Mjpeg)
            && self.camera.recording_encoder != "cpu"
            && !self.software_only
            && hardware_available().await;
        let encoder = if codec != Some(Codec::Mjpeg) {
            "copy"
        } else if hardware {
            "h264_vaapi"
        } else {
            "libx264"
        };
        let fps = self.camera.recording_fps.unwrap_or(fps).clamp(1.0, 60.0);
        tokio::fs::create_dir_all(&self.settings.dir).await?;
        let staging = tempfile::Builder::new()
            .prefix(".partial-")
            .tempdir_in(&self.settings.dir)?
            .keep();
        let mut command = process::ffmpeg();
        command
            .current_dir(&staging)
            .args(["-hide_banner", "-loglevel", "error"]);
        if hardware {
            command.args(["-vaapi_device", "/dev/dri/renderD128"]);
        }
        if let Some(codec) = codec {
            command.args([
                "-use_wallclock_as_timestamps",
                "1",
                "-fflags",
                "+genpts",
                "-probesize",
                "32768",
                "-analyzeduration",
                "0",
                "-thread_queue_size",
                "64",
            ]);
            if codec == Codec::Mjpeg {
                command.args([
                    "-probesize",
                    "32768",
                    "-analyzeduration",
                    "0",
                    "-framerate",
                    &fps.to_string(),
                ]);
            }
            command.args(["-f", codec.name(), "-i", "pipe:0"]);
            if audio {
                command.args([
                    "-use_wallclock_as_timestamps",
                    "1",
                    "-thread_queue_size",
                    "512",
                    "-f",
                    "alaw",
                    "-ar",
                    "8000",
                    "-ac",
                    "1",
                    "-i",
                    "pipe:3",
                ]);
            }
        } else {
            command.args([
                "-rtsp_transport",
                "tcp",
                "-rw_timeout",
                "8000000",
                "-i",
                self.camera
                    .rtsp_url
                    .as_deref()
                    .context("URL RTSP tidak tersedia")?,
            ]);
        }
        if audio {
            command.args(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "32k"]);
        } else {
            command.arg("-an");
        }
        match encoder {
            "copy" => {
                command.args(["-c:v", "copy"]);
            }
            "h264_vaapi" => {
                command.args([
                    "-vf",
                    "format=nv12,hwupload",
                    "-c:v",
                    "h264_vaapi",
                    "-qp",
                    "22",
                    "-g",
                    &(fps * 2.0).round().to_string(),
                ]);
            }
            _ => {
                command.args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    &self.camera.recording_crf.to_string(),
                    "-threads",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                    "-g",
                    &(fps * 2.0).round().to_string(),
                ]);
            }
        }
        if codec == Some(Codec::Mjpeg) {
            command.args(["-fps_mode", "vfr"]);
        }
        let id = if self.camera.device_id.is_empty() {
            "rtsp"
        } else {
            &self.camera.device_id
        };
        command.args([
            "-progress",
            "pipe:2",
            "-f",
            "segment",
            "-segment_time",
            &self.settings.segment_seconds.to_string(),
            "-segment_format",
            "mp4",
            "-segment_format_options",
            "movflags=+faststart",
            "-reset_timestamps",
            "1",
            "-strftime",
            "1",
            "-segment_list",
            "pipe:1",
            "-segment_list_type",
            "flat",
            &format!("cam{id}_%Y%m%d_%H%M%S.mp4"),
        ]);
        let (sender, events) = mpsc::channel(64);
        // Camera frames arrive in ~1s bursts; pace them so FFmpeg's wall-clock timestamps are even.
        let pace = codec.map(|_| Pace {
            fps,
            buffer: Duration::from_millis(1500),
        });
        let process = ManagedProcess::spawn_paced(
            command,
            audio,
            sender,
            Duration::from_secs(5),
            8 * 1024 * 1024,
            pace,
        )?;
        self.process = Some(process);
        self.events = events;
        self.staging = Some(staging);
        self.pending.clear();
        self.stopping = false;
        self.last_progress = Instant::now();
        self.status.encoder = Some(encoder.into());
        Ok(())
    }
    pub fn write(&mut self, frame: &Frame) {
        if self.stopping || self.process.is_none() {
            self.status.dropped_frames += 1;
            return;
        }
        if !self
            .process
            .as_ref()
            .unwrap()
            .input
            .send(frame.data.clone())
        {
            self.status.dropped_frames += 1;
            self.restart("Buffer recorder penuh; menunggu keyframe berikutnya.");
        }
    }
    pub fn audio(&mut self, data: bytes::Bytes) {
        if self.stopping || data.len() <= 16 {
            return;
        }
        if let Some(audio) = self.process.as_ref().and_then(|p| p.audio.as_ref())
            && !audio.send(data.slice(16..))
        {
            self.restart("Buffer audio penuh; menunggu keyframe berikutnya.");
        }
    }
    pub fn restart(&mut self, reason: &str) {
        if self.stopping {
            return;
        }
        self.status.restarts += 1;
        self.status.error = Some(reason.into());
        self.stop();
    }
    pub fn stop(&mut self) {
        self.stopping = true;
        if let Some(process) = &self.process {
            process.stop();
        }
    }
    pub async fn output(&mut self, event: Output) {
        match event {
            Output::Progress(_) => self.last_progress = Instant::now(),
            Output::Stdout(bytes) => {
                self.pending.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(end) = self.pending.find('\n') {
                    let name = self.pending[..end].trim().to_owned();
                    self.pending.drain(..end + 1);
                    self.finalize(&name).await;
                }
            }
            Output::Closed(success) => {
                if !success && self.status.encoder.as_deref() == Some("h264_vaapi") {
                    self.software_only = true;
                }
                if !success && !self.stopping {
                    self.status.restarts += 1;
                    self.status.error =
                        Some("Recorder berhenti; menunggu keyframe berikutnya.".into());
                    if self.status.encoder.as_deref() == Some("h264_vaapi") {
                        self.software_only = true;
                        self.status.error = Some("Encoder GPU gagal; fallback ke CPU.".into());
                    }
                }
                self.process = None;
                self.stopping = false;
                if let Some(staging) = self.staging.take() {
                    let _ = tokio::fs::remove_dir(staging).await;
                }
            }
        }
    }
    async fn finalize(&mut self, name: &str) {
        if parse_name(name, 0).is_none() {
            return;
        }
        let Some(staging) = &self.staging else { return };
        let source = staging.join(name);
        let target = self.settings.dir.join(name);
        if !finalized_mp4(&source) {
            self.status.error = Some(format!("MP4 belum selesai: {name}"));
            return;
        }
        // Hard links preserve completed files and never overwrite an existing timestamp.
        match tokio::fs::hard_link(&source, &target).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(&source).await;
                self.status.segments += 1;
                self.status.error = None;
                self.queue.add(target).await;
            }
            Err(_) => self.status.error = Some(format!("Finalisasi rekaman gagal: {name}")),
        }
    }
    pub async fn shutdown(&mut self) {
        self.stop();
        while self.process.is_some() {
            match tokio::time::timeout(Duration::from_secs(9), self.events.recv()).await {
                Ok(Some(event)) => self.output(event).await,
                _ => break,
            }
        }
    }
}
