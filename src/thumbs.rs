use crate::{
    process,
    storage::{MOTION_THRESHOLD, Store, parse_name},
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
fn parse_scores(output: &str) -> Vec<(f64, f32)> {
    let mut time = None;
    let mut scores = vec![];
    for line in output.lines() {
        if let Some(t) = line.split("pts_time:").nth(1) {
            time = t.trim().parse::<f64>().ok();
        } else if let Some(v) = line.strip_prefix("lavfi.scene_score=")
            && let (Some(t), Ok(v)) = (time, v.trim().parse::<f32>())
        {
            scores.push((t, v));
        }
    }
    scores
}
/// Picks the keyframe where the scene changed most; on a still clip, the middle one.
fn best_frame(scores: &[(f64, f32)]) -> Option<(f64, f32)> {
    let peak = scores
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if peak.1 >= MOTION_THRESHOLD {
        Some(peak)
    } else {
        Some((scores[scores.len() / 2].0, peak.1))
    }
}
async fn run(command: &mut tokio::process::Command, capture: bool) -> Result<String> {
    command
        .stdin(Stdio::null())
        .stdout(if capture { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .context("ffmpeg terlalu lama")??;
    if !output.status.success() {
        bail!("ffmpeg gagal")
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
/// Writes a thumbnail of the most active moment and returns the motion score. Only keyframes
/// are decoded, so this costs a fraction of a second per minute of video.
pub async fn analyze(video: &Path, output: &Path) -> Result<f32> {
    let mut scan = process::ffmpeg();
    scan.args(["-hide_banner", "-loglevel", "error", "-skip_frame", "nokey", "-i"])
        .arg(video)
        .args([
            "-an",
            "-vf",
            &format!("scale={THUMB_WIDTH}:-2,select=gte(scene\\,0),metadata=print:file=-"),
            "-f",
            "null",
            "-",
        ]);
    let scores = parse_scores(&run(&mut scan, true).await?);
    let (at, motion) = best_frame(&scores).context("Tidak ada keyframe.")?;
    let temp = output.with_file_name(format!(
        ".tmp-{}",
        output.file_name().context("nama thumbnail")?.to_string_lossy()
    ));
    let mut grab = process::ffmpeg();
    grab.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{at:.3}"), "-i"])
        .arg(video)
        .args([
            "-an",
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMB_WIDTH}:-2"),
            "-q:v",
            "6",
        ])
        .arg(&temp);
    let result = run(&mut grab, false).await;
    if result.is_err() || tokio::fs::rename(&temp, output).await.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
        bail!("Thumbnail gagal dibuat.")
    }
    Ok(motion)
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
        if let Ok(motion) = analyze(file, &output).await {
            self.index.set_motion(&name, motion);
        }
    }
}
