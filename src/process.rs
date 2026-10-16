use anyhow::Result;
use bytes::Bytes;
use std::{
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixStream as StdUnixStream,
    },
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, watch},
};
use tokio_util::sync::CancellationToken;

pub enum Output {
    Stdout(Bytes),
    Progress(i64),
    Closed(bool),
}
/// Smooths bursty camera delivery before FFmpeg stamps packets with the wall clock.
/// The camera sends about one second of frames at once; without this the recording stalls
/// for a second and then fast-forwards.
#[derive(Clone, Copy)]
pub struct Pace {
    pub fps: f64,
    pub buffer: Duration,
}
#[derive(Clone)]
pub struct BufferedInput {
    sender: mpsc::Sender<(Instant, Bytes)>,
    queued: Arc<AtomicUsize>,
    limit: usize,
}
impl BufferedInput {
    pub fn send(&self, data: Bytes) -> bool {
        let len = data.len();
        if self
            .queued
            .try_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                queued.checked_add(len).filter(|n| *n <= self.limit)
            })
            .is_err()
        {
            return false;
        }
        if self.sender.try_send((Instant::now(), data)).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        true
    }
}
/// `video` paces each message by queue depth; `delay` only shifts messages (used for audio so
/// it stays aligned with the buffered video).
enum Timing {
    None,
    Video(Pace),
    Delay(Duration),
}
fn input<W: AsyncWrite + Unpin + Send + 'static>(
    mut writer: W,
    limit: usize,
    timing: Timing,
    cancel: CancellationToken,
) -> BufferedInput {
    let (sender, mut receiver) = mpsc::channel::<(Instant, Bytes)>(512);
    let queued = Arc::new(AtomicUsize::new(0));
    let count = queued.clone();
    tokio::spawn(async move {
        let mut next: Option<Instant> = None;
        let mut draining = false;
        loop {
            let (arrived, data) = if draining {
                match receiver.recv().await {
                    Some(item) => item,
                    None => break,
                }
            } else {
                tokio::select! {
                    _=cancel.cancelled()=>{receiver.close();draining=true;continue},
                    data=receiver.recv()=>match data{Some(data)=>data,None=>break}
                }
            };
            let slot = match timing {
                Timing::None => None,
                Timing::Delay(_) if draining => None,
                Timing::Delay(delay) => Some(arrived + delay),
                Timing::Video(pace) => {
                    let base = Duration::from_secs_f64(1.0 / pace.fps);
                    // Frames behind schedule by more than one step mean the queue ran dry: resync.
                    let slot = match next {
                        Some(next) if next + base >= Instant::now() => next,
                        Some(_) => Instant::now(),
                        None => arrived + pace.buffer,
                    };
                    // Aim to keep `buffer` worth of frames queued: speed up when deeper, slow down
                    // when shallower, by at most 30%, so a wrong fps estimate self-corrects.
                    let target = (pace.fps * pace.buffer.as_secs_f64()).max(1.0);
                    let factor = if draining {
                        // Shutdown: flush the buffered tail at double speed, still with distinct
                        // timestamps (identical ones would make the muxer drop frames).
                        0.5
                    } else {
                        (1.0 - 0.15 * (receiver.len() as f64 - target) / target).clamp(0.7, 1.3)
                    };
                    next = Some(slot + base.mul_f64(factor));
                    Some(slot)
                }
            };
            if let Some(slot) = slot {
                if draining {
                    tokio::time::sleep_until(slot.into()).await;
                } else {
                    tokio::select! {
                        _=cancel.cancelled()=>{}
                        _=tokio::time::sleep_until(slot.into())=>{}
                    }
                }
            }
            let write = tokio::time::timeout(Duration::from_secs(2), writer.write_all(&data));
            let ok = matches!(write.await, Ok(Ok(())));
            count.fetch_sub(data.len(), Ordering::AcqRel);
            if !ok {
                break;
            }
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), writer.shutdown()).await;
    });
    BufferedInput {
        sender,
        queued,
        limit,
    }
}
pub struct ManagedProcess {
    pub input: BufferedInput,
    pub audio: Option<BufferedInput>,
    stop: CancellationToken,
    finished: watch::Receiver<bool>,
}
impl ManagedProcess {
    pub fn spawn(
        command: Command,
        audio: bool,
        events: mpsc::Sender<Output>,
        grace: Duration,
        limit: usize,
    ) -> Result<Self> {
        Self::spawn_paced(command, audio, events, grace, limit, None)
    }
    pub fn spawn_paced(
        mut command: Command,
        audio: bool,
        events: mpsc::Sender<Output>,
        grace: Duration,
        limit: usize,
        pace: Option<Pace>,
    ) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut audio_parent = None;
        let mut audio_child: Option<OwnedFd> = None;
        if audio {
            let (child, parent) = StdUnixStream::pair()?;
            parent.set_nonblocking(true)?;
            audio_parent = Some(tokio::net::UnixStream::from_std(parent)?);
            audio_child = Some(child.into());
            let fd = audio_child.as_ref().unwrap().as_raw_fd();
            // Only async-signal-safe libc operations execute between fork and exec. The owned
            // descriptor stays alive through spawn and is duplicated into FFmpeg's audio fd.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn()?;
        drop(audio_child);
        let input_cancel = CancellationToken::new();
        let video_timing = pace.map_or(Timing::None, Timing::Video);
        let audio_timing = pace.map_or(Timing::None, |p| Timing::Delay(p.buffer));
        let video = input(
            child.stdin.take().unwrap(),
            limit,
            video_timing,
            input_cancel.clone(),
        );
        let audio = audio_parent
            .map(|writer| input(writer, 1024 * 1024, audio_timing, input_cancel.clone()));
        let mut stdout = child.stdout.take().unwrap();
        let out = events.clone();
        let stdout_task = tokio::spawn(async move {
            let mut buffer = vec![0; 65536];
            loop {
                match stdout.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out
                            .send(Output::Stdout(Bytes::copy_from_slice(&buffer[..n])))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let mut stderr = child.stderr.take().unwrap();
        let progress = events.clone();
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0; 4096];
            let mut pending = Vec::new();
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buffer[..n]);
                        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                            let line = String::from_utf8_lossy(&pending[..end]);
                            if let Some(value) = line
                                .strip_prefix("out_time_us=")
                                .and_then(|s| s.trim().parse::<i64>().ok())
                            {
                                let _ = progress.try_send(Output::Progress(value));
                            }
                            pending.drain(..=end);
                        }
                        // Raw stderr can contain RTSP credentials: never log or publish it.
                        if pending.len() > 8192 {
                            pending.clear();
                        }
                    }
                }
            }
        });
        let stop = CancellationToken::new();
        let stop_task = stop.clone();
        let (finished_sender, finished) = watch::channel(false);
        tokio::spawn(async move {
            let success = tokio::select! {
                result=child.wait()=>result.is_ok_and(|status|status.success()),
                _=stop_task.cancelled()=>{input_cancel.cancel();terminate(&mut child,grace).await}
            };
            input_cancel.cancel();
            // Drain final segment notifications before reporting closure.
            let _ = tokio::time::timeout(Duration::from_secs(2), stdout_task).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), stderr_task).await;
            let _ = events.send(Output::Closed(success)).await;
            let _ = finished_sender.send(true);
        });
        Ok(Self {
            input: video,
            audio,
            stop,
            finished,
        })
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }
    pub fn closed(&self) -> bool {
        *self.finished.borrow()
    }
    pub async fn shutdown(&mut self) {
        self.stop();
        while !*self.finished.borrow() {
            if self.finished.changed().await.is_err() {
                break;
            }
        }
    }
}
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
async fn terminate(child: &mut Child, grace: Duration) -> bool {
    if let Ok(result) = tokio::time::timeout(grace, child.wait()).await {
        return result.is_ok_and(|status| status.success());
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
    false
}
pub fn ffmpeg() -> Command {
    Command::new(std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".into()))
}
#[derive(Default)]
pub struct JpegCollector {
    buffer: Vec<u8>,
}
impl JpegCollector {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Bytes>> {
        self.buffer.extend_from_slice(bytes);
        let mut images = vec![];
        loop {
            let Some(start) = self.buffer.windows(2).position(|b| b == [0xff, 0xd8]) else {
                if self.buffer.len() > 1 {
                    self.buffer.drain(..self.buffer.len() - 1);
                }
                break;
            };
            if start > 0 {
                self.buffer.drain(..start);
            }
            let Some(end) = self.buffer[2..]
                .windows(2)
                .position(|b| b == [0xff, 0xd9])
                .map(|n| n + 4)
            else {
                break;
            };
            images.push(Bytes::from(self.buffer.drain(..end).collect::<Vec<_>>()));
        }
        if self.buffer.len() > 8 * 1024 * 1024 {
            anyhow::bail!("Output preview tidak valid.")
        }
        Ok(images)
    }
}
pub fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2;
    while offset + 4 <= jpeg.len() && jpeg[offset] == 0xff {
        let marker = jpeg[offset + 1];
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if marker == 0xff {
            offset += 1;
            continue;
        }
        let length = u16::from_be_bytes([jpeg[offset + 2], jpeg[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > jpeg.len() {
            break;
        }
        if [
            0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
        ]
        .contains(&marker)
            && length >= 8
        {
            return Some((
                u16::from_be_bytes([jpeg[offset + 7], jpeg[offset + 8]]) as u32,
                u16::from_be_bytes([jpeg[offset + 5], jpeg[offset + 6]]) as u32,
            ));
        }
        offset += length + 2;
    }
    None
}
