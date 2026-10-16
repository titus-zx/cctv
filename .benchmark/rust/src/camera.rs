use crate::{
    config::{Camera, Recording},
    process::{self, JpegCollector, ManagedProcess, Output},
    protocol::{self, Codec, Control, Event},
    recording::Recorder,
    storage::UploadQueue,
};
use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify, RwLock, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub enum CameraCommand {
    Reconnect,
    Ptz(String, oneshot::Sender<Result<()>>),
    Talk(Bytes, oneshot::Sender<Result<()>>),
}
pub struct CameraHandle {
    pub camera: RwLock<Camera>,
    pub status: Mutex<Value>,
    pub commands: mpsc::Sender<CameraCommand>,
    pub jpeg: broadcast::Sender<Bytes>,
    pub audio: broadcast::Sender<Bytes>,
    pub demand: AtomicUsize,
    pub demand_changed: Notify,
    pub queue: Option<UploadQueue>,
}
pub struct DemandGuard(pub Arc<CameraHandle>);
impl Drop for DemandGuard {
    fn drop(&mut self) {
        self.0.demand.fetch_sub(1, Ordering::AcqRel);
        self.0.demand_changed.notify_one();
    }
}
impl CameraHandle {
    pub fn subscribe(self: &Arc<Self>) -> (DemandGuard, broadcast::Receiver<Bytes>) {
        self.demand.fetch_add(1, Ordering::AcqRel);
        let receiver = self.jpeg.subscribe();
        self.demand_changed.notify_one();
        (DemandGuard(self.clone()), receiver)
    }
    pub async fn snapshot(self: &Arc<Self>) -> Result<Bytes> {
        let (_guard, mut receiver) = self.subscribe();
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .context("Gambar kamera belum tersedia.")?
            .context("Preview terputus.")
    }
    pub async fn public_status(&self, id: usize) -> Value {
        let camera = self.camera.read().await;
        let mut status = self.status.lock().await.clone();
        for (key, value) in [
            ("id", json!(id)),
            ("name", json!(camera.name)),
            ("ip", json!(camera.ip)),
            ("deviceId", json!(camera.device_id)),
            ("username", json!(camera.username)),
            ("mode", json!(camera.mode)),
            ("quality", json!(camera.quality)),
            ("v380Port", json!(camera.v380_port)),
            ("hasPassword", json!(!camera.password.is_empty())),
            ("viewers", json!(self.demand.load(Ordering::Acquire))),
        ] {
            status[key] = value
        }
        if let Some(queue) = &self.queue {
            let queue = queue.status.lock().await;
            let record = &mut status["recording"];
            if record.is_object() {
                record["uploaded"] = json!(queue.uploaded);
                record["pending"] = json!(queue.pending);
                if record["error"].is_null() {
                    record["error"] = json!(queue.error)
                }
            }
        }
        status
    }
    pub async fn ptz(&self, direction: String) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(CameraCommand::Ptz(direction, tx))
            .await?;
        tokio::time::timeout(Duration::from_secs(3), rx).await??
    }
    pub async fn talk(&self, data: Bytes) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(CameraCommand::Talk(data, tx)).await?;
        tokio::time::timeout(Duration::from_secs(3), rx).await??
    }
}
pub fn spawn(
    camera: Camera,
    recording: Option<Recording>,
    queue: Option<UploadQueue>,
    cancel: CancellationToken,
) -> (Arc<CameraHandle>, tokio::task::JoinHandle<()>) {
    let (commands, receiver) = mpsc::channel(16);
    let (jpeg, _) = broadcast::channel(2);
    let (audio, _) = broadcast::channel(32);
    let handle = Arc::new(CameraHandle {
        camera: RwLock::new(camera),
        status: Mutex::new(
            json!({"state":"idle","message":"Belum terhubung","frames":0,"lastFrameAt":null,"reconnects":0,"audio":false,"previewActive":false,"previewError":null,"lastConnectionError":null,"streamStats":null,"recording":null}),
        ),
        commands,
        jpeg,
        audio,
        demand: AtomicUsize::new(0),
        demand_changed: Notify::new(),
        queue,
    });
    let actor = handle.clone();
    let task = tokio::spawn(async move {
        run(actor, receiver, recording, cancel).await;
    });
    (handle, task)
}
fn preview(
    camera: &Camera,
    codec: Option<Codec>,
    sender: mpsc::Sender<Output>,
) -> Result<ManagedProcess> {
    let mut command = process::ffmpeg();
    command.args(["-hide_banner", "-loglevel", "error", "-threads", "1"]);
    if let Some(codec) = codec {
        command.args([
            "-flags",
            "low_delay",
            "-probesize",
            "32768",
            "-analyzeduration",
            "0",
            "-f",
            codec.name(),
            "-i",
            "pipe:0",
        ]);
    } else {
        command.args([
            "-rtsp_transport",
            "tcp",
            "-rw_timeout",
            "8000000",
            "-i",
            camera
                .rtsp_url
                .as_deref()
                .context("URL RTSP tidak tersedia.")?,
        ]);
    }
    command.args([
        "-an",
        "-vf",
        &format!("fps={},scale=w='min(640,iw)':h=-2", camera.preview_fps),
        "-threads",
        "1",
        "-c:v",
        "mjpeg",
        "-q:v",
        "5",
        "-f",
        "image2pipe",
        "pipe:1",
    ]);
    ManagedProcess::spawn(
        command,
        false,
        sender,
        Duration::from_secs(2),
        2 * 1024 * 1024,
    )
}
enum Input {
    Command(Option<CameraCommand>),
    Source(Option<Event>),
    SourceDone(Result<()>),
    Preview(Option<Output>),
    Recording(Option<Output>),
    Demand,
    Tick,
    Stop,
}
async fn run(
    handle: Arc<CameraHandle>,
    mut commands: mpsc::Receiver<CameraCommand>,
    recording: Option<Recording>,
    cancel: CancellationToken,
) {
    let mut camera = handle.camera.read().await.clone();
    let make_recorder = |camera: &Camera| {
        recording
            .as_ref()
            .filter(|r| r.enabled && camera.record)
            .and_then(|r| {
                handle
                    .queue
                    .clone()
                    .map(|q| Recorder::new(camera.clone(), r.clone(), q))
            })
    };
    let mut recorder = make_recorder(&camera);
    let (_, mut source_events) = mpsc::channel::<Event>(16);
    let (mut control_tx, _) = mpsc::channel(16);
    let mut source: Option<tokio::task::JoinHandle<Result<()>>> = None;
    let mut source_cancel = CancellationToken::new();
    let (_, mut preview_events) = mpsc::channel::<Output>(16);
    let mut decoder: Option<ManagedProcess> = None;
    let mut preview_stopping = false;
    let mut jpeg = JpegCollector::default();
    let mut reconnect_at = Instant::now();
    let mut failures = 0u32;
    let mut frame_count = 0u64;
    let mut last_frame = None;
    let mut first_frame = None;
    let mut sample_time = Instant::now();
    let mut sample_frames = 0u64;
    let mut fps = 10.0;
    let mut audio_seen = false;
    let mut last_jpeg = Instant::now() - Duration::from_secs(1);
    let mut preview_retry = Instant::now();
    let mut started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        let source_running = source.is_some();
        let input = tokio::select! {
            _=cancel.cancelled()=>Input::Stop,
            command=commands.recv()=>Input::Command(command),
            event=source_events.recv(),if source_running=>Input::Source(event),
            result=async{match source.as_mut(){Some(task)=>task.await,None=>std::future::pending().await}}=>Input::SourceDone(result.unwrap_or_else(|_|Err(anyhow::anyhow!("Koneksi kamera terputus.")))),
            event=async{if decoder.is_some(){preview_events.recv().await}else{std::future::pending().await}}=>Input::Preview(event),
            event=async{match recorder.as_mut(){Some(recorder)if recorder.process.is_some()=>recorder.events.recv().await,_=>std::future::pending().await}}=>Input::Recording(event),
            _=handle.demand_changed.notified()=>Input::Demand,
            _=tick.tick()=>Input::Tick,
        };
        match input {
            Input::Stop | Input::Command(None) => break,
            Input::Command(Some(CameraCommand::Reconnect)) => {
                source_cancel.cancel();
                if let Some(task) = source.take() {
                    let _ = task.await;
                }
                if let Some(process) = &decoder {
                    process.stop();
                    preview_stopping = true;
                }
                if let Some(recorder) = recorder.as_mut() {
                    recorder.shutdown().await;
                }
                camera = handle.camera.read().await.clone();
                recorder = make_recorder(&camera);
                reconnect_at = Instant::now();
                failures = 0;
            }
            Input::Command(Some(CameraCommand::Ptz(direction, reply))) => {
                let live = handle.status.lock().await["state"] == "live";
                let result = if !live || camera.mode == "rtsp" {
                    Err(anyhow::anyhow!("Kamera belum live."))
                } else {
                    protocol::ptz_packet(&direction).and_then(|_| {
                        control_tx
                            .try_send(Control::Ptz(direction))
                            .map_err(|_| anyhow::anyhow!("Kontrol kamera belum tersedia."))
                    })
                };
                let _ = reply.send(result);
            }
            Input::Command(Some(CameraCommand::Talk(data, reply))) => {
                let live = handle.status.lock().await["state"] == "live";
                let result = if !live || camera.mode == "rtsp" {
                    Err(anyhow::anyhow!("Kamera belum live."))
                } else if data.is_empty() || data.len() > 480 {
                    Err(anyhow::anyhow!("Ukuran audio tidak valid."))
                } else {
                    control_tx
                        .try_send(Control::Talk(data))
                        .map_err(|_| anyhow::anyhow!("Kontrol kamera belum tersedia."))
                };
                let _ = reply.send(result);
            }
            Input::Source(Some(Event::Status(message))) => {
                let mut s = handle.status.lock().await;
                s["state"] = json!("connecting");
                s["message"] = json!(message);
            }
            Input::Source(Some(Event::Info(info))) => {
                let mut s = handle.status.lock().await;
                if let Some(info) = info.as_object() {
                    for (k, v) in info {
                        s[k] = v.clone();
                    }
                }
            }
            Input::Source(Some(Event::Stats(stats))) => {
                handle.status.lock().await["streamStats"] = json!(stats)
            }
            Input::Source(Some(Event::Audio(data))) => {
                audio_seen = true;
                handle.status.lock().await["audio"] = json!(true);
                let _ = handle.audio.send(data.slice(16..));
                if let Some(recorder) = recorder.as_mut() {
                    recorder.audio(data);
                }
            }
            Input::Source(Some(Event::Frame(frame))) => {
                let now = Instant::now();
                last_frame = Some(now);
                failures = 0;
                frame_count += 1;
                if first_frame.is_none() {
                    first_frame = Some(now);
                    sample_time = now;
                    sample_frames = 0;
                }
                sample_frames += 1;
                if sample_time.elapsed() >= Duration::from_secs(3) {
                    fps = (sample_frames as f64 / sample_time.elapsed().as_secs_f64())
                        .clamp(1.0, 60.0);
                    sample_time = now;
                    sample_frames = 0;
                }
                {
                    let mut s = handle.status.lock().await;
                    s["state"] = json!("live");
                    s["message"] = json!("Video langsung");
                    s["frames"] = json!(frame_count);
                    s["lastFrameAt"] = json!(Utc::now().timestamp_millis());
                    s["codec"] = json!(frame.codec);
                }
                if let Some(recorder) = recorder.as_mut() {
                    if recorder.ready()
                        && frame.keyframe
                        && (audio_seen
                            || first_frame.is_some_and(|t| t.elapsed() > Duration::from_secs(3)))
                        && recorder
                            .start(Some(frame.codec), audio_seen, fps)
                            .await
                            .is_err()
                    {
                        recorder.status.error = Some("Recorder tidak dapat dijalankan.".into());
                    }
                    if recorder.process.is_some() || recorder.status.restarts > 0 {
                        recorder.write(&frame);
                    }
                }
                if handle.demand.load(Ordering::Acquire) > 0 {
                    if frame.codec == Codec::Mjpeg {
                        if last_jpeg.elapsed().as_secs_f64() >= 1.0 / camera.preview_fps as f64 {
                            publish(&handle, frame.data).await;
                            last_jpeg = now;
                        }
                    } else {
                        if decoder.is_none() && frame.keyframe && now >= preview_retry {
                            let (preview_tx, rx) = mpsc::channel(16);
                            preview_events = rx;
                            jpeg = JpegCollector::default();
                            match preview(&camera, Some(frame.codec), preview_tx.clone()) {
                                Ok(process) => {
                                    decoder = Some(process);
                                    preview_stopping = false;
                                }
                                Err(_) => {
                                    handle.status.lock().await["previewError"] =
                                        json!("FFmpeg preview tidak dapat dijalankan.");
                                    preview_retry = now + Duration::from_secs(3);
                                }
                            }
                        }
                        if let Some(process) = &decoder
                            && !preview_stopping
                            && !process.input.send(frame.data)
                        {
                            process.stop();
                            preview_stopping = true;
                            preview_retry = now + Duration::from_secs(3);
                            handle.status.lock().await["previewError"] =
                                json!("Preview terlalu lambat; rekaman tetap berjalan.");
                        }
                    }
                }
            }
            Input::SourceDone(result) => {
                source = None;
                source_cancel.cancel();
                let error = result
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "Koneksi kamera terputus.".into());
                failures = failures.saturating_add(1);
                reconnect_at = Instant::now()
                    + Duration::from_secs(
                        (3 * 2u64.pow(failures.saturating_sub(1).min(4))).min(30),
                    );
                if error.starts_with("AUTH_") {
                    reconnect_at = Instant::now() + Duration::from_secs(86400 * 365);
                }
                let mut s = handle.status.lock().await;
                s["state"] = json!("error");
                s["message"] = json!(error);
                s["lastConnectionError"] = s["message"].clone();
                s["reconnects"] = json!(s["reconnects"].as_u64().unwrap_or(0) + 1);
                if let Some(recorder) = recorder.as_mut() {
                    recorder.stop();
                }
                if let Some(process) = &decoder {
                    process.stop();
                    preview_stopping = true;
                }
            }
            Input::Preview(Some(Output::Stdout(data))) => {
                if !preview_stopping {
                    match jpeg.push(&data) {
                        Ok(images) => {
                            for image in images {
                                publish(&handle, image).await;
                                if camera.mode == "rtsp" {
                                    last_frame = Some(Instant::now());
                                }
                            }
                        }
                        Err(_) => {
                            if let Some(process) = &decoder {
                                process.stop();
                                preview_stopping = true;
                            }
                            handle.status.lock().await["previewError"] =
                                json!("Output preview tidak valid.");
                        }
                    }
                }
            }
            Input::Preview(Some(Output::Closed(_))) | Input::Preview(None) => {
                let expected = preview_stopping;
                decoder = None;
                preview_stopping = false;
                if !expected {
                    handle.status.lock().await["previewError"] =
                        json!("Preview berhenti; rekaman tetap berjalan.");
                    preview_retry = Instant::now() + Duration::from_secs(3);
                }
            }
            Input::Recording(Some(event)) => {
                if let Some(recorder) = recorder.as_mut() {
                    let progress = matches!(event, Output::Progress(_));
                    recorder.output(event).await;
                    if progress && camera.mode == "rtsp" {
                        last_frame = Some(Instant::now());
                        let mut s = handle.status.lock().await;
                        s["state"] = json!("live");
                        s["message"] = json!("Video langsung");
                        s["lastFrameAt"] = json!(Utc::now().timestamp_millis());
                    }
                }
            }
            Input::Demand | Input::Tick => {
                let wanted = handle.demand.load(Ordering::Acquire) > 0;
                if !wanted
                    && let Some(process) = &decoder
                    && !preview_stopping
                {
                    process.stop();
                    preview_stopping = true;
                }
                if camera.mode == "rtsp" {
                    if wanted && decoder.is_none() && Instant::now() >= preview_retry {
                        let (preview_tx, rx) = mpsc::channel(16);
                        preview_events = rx;
                        match preview(&camera, None, preview_tx.clone()) {
                            Ok(process) => {
                                decoder = Some(process);
                                preview_stopping = false;
                            }
                            Err(_) => preview_retry = Instant::now() + Duration::from_secs(3),
                        }
                    }
                    if let Some(recorder) = recorder.as_mut()
                        && recorder.ready()
                        && recorder.start(None, false, 10.0).await.is_err()
                    {
                        recorder.status.error =
                            Some("Recorder RTSP tidak dapat dijalankan.".into());
                    }
                } else if source.is_none() && Instant::now() >= reconnect_at {
                    let (source_events_tx, rx) = mpsc::channel(16);
                    source_events = rx;
                    let (tx, rx) = mpsc::channel(16);
                    control_tx = tx;
                    source_cancel = CancellationToken::new();
                    source = Some(tokio::spawn(protocol::run(
                        camera.clone(),
                        source_events_tx.clone(),
                        rx,
                        source_cancel.clone(),
                    )));
                    started = Instant::now();
                    last_frame = None;
                    first_frame = None;
                    audio_seen = false;
                    let mut s = handle.status.lock().await;
                    s["state"] = json!("connecting");
                    s["message"] = json!("Menghubungkan kamera…");
                    s["audio"] = json!(false);
                    s["streamStats"] = Value::Null;
                }
                if camera.mode != "rtsp"
                    && source.is_some()
                    && last_frame.unwrap_or(started).elapsed()
                        > Duration::from_secs(if camera.mode == "cloud" { 45 } else { 18 })
                {
                    source_cancel.cancel();
                }
                if let Some(recorder) = recorder.as_mut()
                    && recorder.active()
                    && recorder.last_progress.elapsed() > Duration::from_secs(20)
                {
                    recorder.restart("Recorder tidak menghasilkan video; menunggu keyframe baru.");
                }
            }
            _ => {}
        }
        {
            let mut s = handle.status.lock().await;
            s["previewActive"] = json!(decoder.is_some() && !preview_stopping);
            if let Some(recorder) = &recorder {
                s["recording"] = json!(recorder.status);
            }
        }
    }
    source_cancel.cancel();
    if let Some(source) = source {
        let _ = source.await;
    }
    if let Some(recorder) = recorder.as_mut() {
        recorder.shutdown().await;
    }
    if let Some(mut decoder) = decoder {
        decoder.shutdown().await;
    }
}
async fn publish(handle: &CameraHandle, jpeg: Bytes) {
    let mut s = handle.status.lock().await;
    if (s["width"].as_u64().unwrap_or(0) == 0 || s["height"].as_u64().unwrap_or(0) == 0)
        && let Some((width, height)) = process::jpeg_size(&jpeg)
    {
        s["width"] = json!(width);
        s["height"] = json!(height);
    }
    s["previewError"] = Value::Null;
    drop(s);
    let _ = handle.jpeg.send(jpeg);
}
