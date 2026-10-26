use crate::{
    process,
    storage::{Store, parse_name},
};
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};
use tokio::sync::Semaphore;

pub const THUMB_DIR: &str = ".thumbs";
pub const THUMB_WIDTH: u32 = 320;
/// Stale temp files and thumbnails younger than this are never treated as leftovers.
pub const GRACE: Duration = Duration::from_secs(3600);

pub fn thumb_dir(dir: &Path) -> PathBuf {
    dir.join(THUMB_DIR)
}
/// `cam1_20260101_100000.mp4` -> `<dir>/.thumbs/cam1_20260101_100000.jpg`
pub fn thumb_path(dir: &Path, name: &str) -> Option<PathBuf> {
    parse_name(name, 0)?;
    Some(thumb_dir(dir).join(name.replace(".mp4", ".jpg")))
}
pub fn clip_name(thumb: &str) -> Option<String> {
    let name = format!("{}.mp4", thumb.strip_suffix(".jpg")?);
    parse_name(&name, 0).map(|_| name)
}
async fn run(command: &mut tokio::process::Command, capture: bool) -> Result<String> {
    let stderr = tempfile::NamedTempFile::new()?;
    command
        .stdin(Stdio::null())
        .stdout(if capture { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::from(stderr.reopen()?))
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .context("ffmpeg terlalu lama")??;
    if !output.status.success() {
        bail!(
            "ffmpeg gagal ({}): {}",
            output.status,
            process::analysis_stderr(stderr.path()).await
        )
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
/// Thumbnail and confidence of confirmed movement by a recognized object.
pub async fn analyze(video: &Path, output: &Path) -> Result<f32> {
    let analysis = crate::motion::analyze(video).await;
    let at = match &analysis {
        Ok((at, _)) => *at,
        Err(error) => match error.downcast_ref::<crate::motion_gate::InsufficientFrames>() {
            Some(short) if short.count > 0 => short.at,
            _ => return Err(analysis.unwrap_err()),
        },
    };
    let temp = output.with_file_name(format!(
        ".tmp-{}",
        output.file_name().context("nama thumbnail")?.to_string_lossy()
    ));
    let mut grab = process::analysis_ffmpeg();
    grab.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-filter_threads", "1", "-y", "-ss", &format!("{at:.3}"), "-i"])
        .arg(video)
        .args([
            "-an",
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMB_WIDTH}:-2"),
            "-threads",
            "1",
            "-q:v",
            "6",
        ])
        .arg(&temp);
    let result = run(&mut grab, false).await;
    let result = async {
        result.context("Thumbnail gagal dibuat")?;
        tokio::fs::rename(&temp, output)
            .await
            .context("Thumbnail gagal disimpan")?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result?;
    // A short clip still gets a thumbnail, but its movement remains unclassified.
    analysis.map(|(_, motion)| motion)
}
static LIMIT: OnceLock<Semaphore> = OnceLock::new();
impl Store {
    /// Idempotent; one ffmpeg at a time so a backlog never starves recording on a small CPU.
    pub async fn ensure_thumb(&self, file: &Path) {
        let Some(name) = file.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        let Some(output) = thumb_path(&self.dir, &name) else {
            return;
        };
        if self.index.motion(&name).is_some() && output.exists() {
            return;
        }
        let _permit = LIMIT.get_or_init(|| Semaphore::new(1)).acquire().await;
        if tokio::fs::create_dir_all(thumb_dir(&self.dir)).await.is_err() {
            return;
        }
        match analyze(file, &output).await {
            Ok(motion) => self.index.set_motion(&name, motion),
            Err(error) => eprintln!("Analisis objek {name} gagal: {error:#}"),
        }
    }
}
