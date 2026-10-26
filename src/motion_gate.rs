//! Cheap, bounded keyframe scan. Geometry proposes a moment; only AI can label an object.
use anyhow::{bail, Context, Result};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

pub const SIDE: usize = 256;
const PIXELS: usize = SIDE * SIDE;
const MAX_FRAMES: usize = 180;
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Region {
    pub fn intersection(self, other: Self) -> f32 {
        ((self.x + self.w).min(other.x + other.w) - self.x.max(other.x)).max(0.)
            * ((self.y + self.h).min(other.y + other.h) - self.y.max(other.y)).max(0.)
    }
    pub fn area(self) -> f32 {
        self.w * self.h
    }
}
pub struct Scan {
    pub at: f64,
    pub regions: Vec<Region>,
}

fn regions(before: &[u8], after: &[u8], min_area: f32) -> Vec<Region> {
    let mut histogram = [0usize; 511];
    for (&a, &b) in before.iter().zip(after) {
        histogram[(b as i16 - a as i16 + 255) as usize] += 1;
    }
    let mut total = 0;
    let offset = histogram
        .iter()
        .position(|n| {
            total += n;
            total >= PIXELS / 2
        })
        .unwrap_or(255) as i16
        - 255;
    let mut mask: Vec<bool> = before
        .iter()
        .zip(after)
        .map(|(&a, &b)| {
            let delta = b as i16 - a as i16;
            delta != 0 && (delta - offset).abs() > 24
        })
        .collect();
    // Whole-frame lighting/camera changes are not an object proposal.
    if mask.iter().filter(|&&p| p).count() > PIXELS / 2 {
        return vec![];
    }
    let mut found = vec![];
    let minimum = (min_area * PIXELS as f32).ceil() as usize;
    let mut stack = Vec::new();
    for start in 0..PIXELS {
        if !mask[start] {
            continue;
        }
        mask[start] = false;
        stack.push(start);
        let (mut left, mut right, mut top, mut bottom, mut count) = (SIDE, 0, SIDE, 0, 0);
        while let Some(at) = stack.pop() {
            let (x, y) = (at % SIDE, at / SIDE);
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
            count += 1;
            for yy in y.saturating_sub(1)..=(y + 1).min(SIDE - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(SIDE - 1) {
                    let n = yy * SIDE + xx;
                    if mask[n] {
                        mask[n] = false;
                        stack.push(n);
                    }
                }
            }
        }
        let (w, h) = (right - left + 1, bottom - top + 1);
        if count >= minimum && w >= 6 && h >= 6 {
            found.push(Region {
                x: left as f32,
                y: top as f32,
                w: w as f32,
                h: h as f32,
            });
        }
    }
    found.sort_by(|a, b| b.area().total_cmp(&a.area()));
    found.truncate(32);
    found
}
fn persistent(previous: &[Region], current: &[Region]) -> bool {
    current.iter().any(|a| {
        previous.iter().any(|b| {
            // Changed leading/trailing edges move with the object and need not overlap exactly.
            let margin = (a.w.max(a.h).min(b.w.max(b.h)) * 0.5).max(6.);
            let nearby = Region {
                x: a.x - margin,
                y: a.y - margin,
                w: a.w + 2. * margin,
                h: a.h + 2. * margin,
            };
            nearby.intersection(*b) >= 0.05 * a.area().min(b.area())
        })
    })
}
#[derive(Debug)]
pub struct InsufficientFrames {
    pub at: f64,
    pub count: usize,
}
impl std::fmt::Display for InsufficientFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Frame temporal tidak cukup ({}/3); klip belum dianalisis",
            self.count
        )
    }
}
impl std::error::Error for InsufficientFrames {}

/// Sparse GOPs get a sampled full-decode pass within the same total budget.
/// Incomplete scans remain unknown, never a false negative.
pub async fn scan(video: &Path, min_area: f32) -> Result<Scan> {
    tokio::time::timeout(Duration::from_secs(4), async {
        match scan_pass(video, min_area, true).await {
            Err(error) if error.downcast_ref::<InsufficientFrames>().is_some() => {
                scan_pass(video, min_area, false).await
            }
            result => result,
        }
    })
    .await
    .context("Batas waktu pemindaian gerakan tercapai")?
}

async fn scan_pass(video: &Path, min_area: f32, keyframes_only: bool) -> Result<Scan> {
    let metadata = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut command = crate::process::analysis_ffmpeg();
    command.args(["-hide_banner", "-loglevel", "error", "-threads", "1"]);
    if keyframes_only {
        command.args(["-skip_frame", "nokey"]);
    }
    command.arg("-i")
        .arg(video).args(["-an","-filter_threads","1","-vf"])
        .arg(format!("select='isnan(prev_selected_t)+gte(t-prev_selected_t,0.5)',scale={SIDE}:{SIDE}:force_original_aspect_ratio=decrease,pad={SIDE}:{SIDE}:(ow-iw)/2:(oh-ih)/2,format=gray,boxblur=1:1,metadata=mode=add:key=cctv:value=frame,metadata=print:file={}",metadata.path().display()))
        .args(["-frames:v", &MAX_FRAMES.to_string(),"-fps_mode","passthrough","-threads","1","-f","rawvideo","-pix_fmt","gray","-"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::from(stderr.reopen()?)).kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().context("scan stdout")?;
    let work = async {
        let mut frame = vec![0u8; PIXELS];
        let mut previous: Option<Vec<u8>> = None;
        let mut prior_regions = vec![];
        let mut best = (0usize, 0f32, vec![]);
        let mut count = 0;
        loop {
            if stdout.read(&mut frame[..1]).await? == 0 {
                break;
            }
            stdout.read_exact(&mut frame[1..]).await?;
            if let Some(before) = &previous {
                let current = regions(before, &frame, min_area);
                let strength: f32 = current.iter().map(|r| r.area()).sum();
                if strength > best.1 && persistent(&prior_regions, &current) {
                    let mut changed = current.clone();
                    changed.extend_from_slice(&prior_regions);
                    // Middle of three keyframes still contains an object that entered then left.
                    best = (count - 1, strength, changed);
                }
                prior_regions = current;
            }
            previous = Some(frame.clone());
            count += 1;
        }
        let status = child.wait().await?;
        if !status.success() {
            bail!(
                "Pemindaian frame gagal ({status}): {}",
                crate::process::analysis_stderr(stderr.path()).await
            );
        }
        if count >= MAX_FRAMES {
            bail!("Batas keyframe tercapai; analisis belum lengkap");
        }
        let text = tokio::fs::read_to_string(metadata.path()).await?;
        let times: Vec<f64> = text
            .lines()
            .filter_map(|line| line.split("pts_time:").nth(1)?.trim().parse().ok())
            .collect();
        if times.len() != count {
            bail!("Timestamp keyframe tidak lengkap");
        }
        if count < 3 {
            return Err(InsufficientFrames {
                at: times.get(count / 2).copied().unwrap_or(0.),
                count,
            }
            .into());
        }
        let at = if best.2.is_empty() {
            times[count / 2]
        } else {
            times[best.0]
        };
        Ok(Scan {
            at,
            regions: best.2,
        })
    };
    // The outer timeout covers both passes; kill_on_drop also handles cancellation.
    let result = work.await;
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn square(frame: &mut [u8], x: usize, y: usize, size: usize) {
        for row in y..y + size {
            frame[row * SIDE + x..row * SIDE + x + size].fill(180);
        }
    }
    #[test]
    fn small_insects_noise_and_global_lighting_are_rejected() {
        let before = vec![80; PIXELS];
        let mut small = before.clone();
        square(&mut small, 100, 100, 4);
        assert!(regions(&before, &small, 0.002).is_empty());
        let brighter = vec![130; PIXELS];
        assert!(regions(&before, &brighter, 0.002).is_empty());
        let mut noise = before.clone();
        for i in (0..PIXELS).step_by(37) {
            noise[i] = 180;
        }
        assert!(regions(&before, &noise, 0.002).is_empty());
    }
    #[test]
    fn larger_moving_regions_require_two_consecutive_changes() {
        let mut first = vec![80; PIXELS];
        square(&mut first, 60, 60, 30);
        let mut second = vec![80; PIXELS];
        square(&mut second, 70, 60, 30);
        let mut third = vec![80; PIXELS];
        square(&mut third, 80, 60, 30);
        let a = regions(&first, &second, 0.002);
        let b = regions(&second, &third, 0.002);
        assert!(!a.is_empty());
        assert!(!persistent(&[], &a));
        assert!(persistent(&a, &b));
        assert!(regions(&third, &third, 0.002).is_empty());
    }
}
