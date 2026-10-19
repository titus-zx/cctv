//! YOLOv8 COCO object detection, followed by temporal movement confirmation.
//! No scene-change fallback: inference failures leave the clip unclassified.
use anyhow::{Context, Result, bail};
use std::{
    path::Path,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tract_onnx::prelude::*;

const SIDE: usize = 640;
const FRAME_BYTES: usize = SIDE * SIDE * 3;
const FPS: f64 = 2.0;
pub const VERSION: u8 = 2;
type Model = TypedRunnableModel<TypedModel>;
static MODEL: OnceLock<Arc<Model>> = OnceLock::new();

#[derive(Clone, Debug)]
struct Detection {
    class: usize,
    confidence: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    steps: u32,
}
fn overlap(a: &Detection, b: &Detection) -> f32 {
    let w = (a.x + a.w / 2.).min(b.x + b.w / 2.) - (a.x - a.w / 2.).max(b.x - b.w / 2.);
    let h = (a.y + a.h / 2.).min(b.y + b.h / 2.) - (a.y - a.h / 2.).max(b.y - b.h / 2.);
    let intersection = w.max(0.) * h.max(0.);
    intersection / (a.w * a.h + b.w * b.h - intersection).max(1.)
}
fn allowed(class: usize) -> bool {
    // People, vehicles, cats/dogs and larger COCO mammals. Excludes birds/insects.
    matches!(class, 0..=8 | 15..=23)
}
fn number(name: &str, default: f32) -> Result<f32> {
    let n = std::env::var(name)
        .ok()
        .map(|s| s.parse::<f32>())
        .transpose()?
        .unwrap_or(default);
    if !n.is_finite() || n <= 0. || n > 1. {
        bail!("{name} harus > 0 dan <= 1")
    }
    Ok(n)
}
fn model() -> Result<Arc<Model>> {
    if let Some(model) = MODEL.get() {
        return Ok(model.clone());
    }
    let path = std::env::var("MOTION_MODEL_PATH").unwrap_or_else(|_| "models/yolov8n.onnx".into());
    let model = tract_onnx::onnx()
        .model_for_path(&path)
        .with_context(|| format!("Model deteksi objek tidak dapat dibaca: {path}"))?
        .with_input_fact(0, f32::fact([1, 3, SIDE, SIDE]).into())?
        .into_optimized()?
        .into_runnable()?;
    let model = Arc::new(model);
    let _ = MODEL.set(model.clone());
    Ok(model)
}
fn decode(output: &Tensor, confidence: f32, min_area: f32) -> Result<Vec<Detection>> {
    let view = output.to_array_view::<f32>()?;
    let shape = view.shape();
    if shape.len() != 3 || shape[0] != 1 || shape[1] != 84 {
        bail!("Model harus YOLOv8 COCO [1,84,N], output: {shape:?}");
    }
    let mut found = vec![];
    for i in 0..shape[2] {
        let (class, score) = (0..80)
            .map(|c| (c, view[[0, c + 4, i]]))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        if !allowed(class) || !score.is_finite() || score < confidence {
            continue;
        }
        let (x, y, w, h) = (
            view[[0, 0, i]],
            view[[0, 1, i]],
            view[[0, 2, i]],
            view[[0, 3, i]],
        );
        if ![x, y, w, h].iter().all(|v| v.is_finite())
            || w <= 0.
            || h <= 0.
            || w * h < min_area * (SIDE * SIDE) as f32
        {
            continue;
        }
        found.push(Detection {
            class,
            confidence: score,
            x,
            y,
            w,
            h,
            steps: 0,
        });
    }
    found.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let mut kept: Vec<Detection> = vec![];
    for detection in found {
        if !kept
            .iter()
            .any(|a| a.class == detection.class && overlap(a, &detection) > 0.45)
        {
            kept.push(detection);
            if kept.len() == 100 {
                break;
            }
        }
    }
    Ok(kept)
}
fn confirm(previous: &[Detection], current: &mut [Detection]) -> f32 {
    let mut used = vec![false; previous.len()];
    let mut score = 0f32;
    for detection in current {
        let matched = previous
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                !used[*i] && p.class == detection.class && overlap(p, detection) >= 0.2
            })
            .max_by(|(_, a), (_, b)| overlap(a, detection).total_cmp(&overlap(b, detection)));
        if let Some((i, old)) = matched {
            used[i] = true;
            let distance = ((detection.x - old.x).powi(2) + (detection.y - old.y).powi(2)).sqrt();
            let moved = distance >= (old.w.min(old.h) * 0.05).max(3.);
            detection.steps = if moved { old.steps + 1 } else { 0 };
            if detection.steps >= 2 {
                score = score.max(detection.confidence);
            }
        }
    }
    score
}
/// Sample 2 FPS, streaming frames into inference to keep memory bounded.
/// Returns the time and confidence of confirmed movement, or (middle, 0) when still.
pub async fn analyze(video: &Path) -> Result<(f64, f32)> {
    let confidence = number("MOTION_CONFIDENCE", 0.5)?;
    if confidence < crate::storage::MOTION_THRESHOLD {
        bail!("MOTION_CONFIDENCE harus >= 0.5");
    }
    let area = number("MOTION_MIN_AREA", 0.002)?;
    let mut loaded_model = None;
    let mut command = crate::process::ffmpeg();
    command.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-i"])
        .arg(video).args(["-an", "-vf",
            "fps=2,scale=640:640:force_original_aspect_ratio=decrease,pad=640:640:(ow-iw)/2:(oh-ih)/2:color=0x727272",
            "-threads", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let work = async {
        let mut previous = vec![];
        let mut frame = vec![0u8; FRAME_BYTES];
        let mut count = 0usize;
        let mut best = (0., 0f32);
        loop {
            // Distinguish clean EOF from a truncated RGB frame.
            if stdout.read(&mut frame[..1]).await? == 0 {
                break;
            }
            stdout.read_exact(&mut frame[1..]).await?;
            let input = tract_ndarray::Array4::from_shape_fn((1, 3, SIDE, SIDE), |(_, c, y, x)| {
                frame[(y * SIDE + x) * 3 + c] as f32 / 255.
            })
            .into_tensor();
            let model = match &loaded_model {
                Some(model) => Arc::clone(model),
                None => {
                    let model = tokio::task::spawn_blocking(model).await??;
                    loaded_model = Some(model.clone());
                    model
                }
            };
            let mut current = tokio::task::spawn_blocking(move || -> Result<Vec<Detection>> {
                let outputs = model.run(tvec!(input.into()))?;
                decode(&outputs[0], confidence, area)
            })
            .await??;
            let score = confirm(&previous, &mut current);
            if std::env::var_os("MOTION_DEBUG").is_some() {
                eprintln!("motion frame={count} score={score} detections={current:?}");
            }
            if score > best.1 {
                best = (count as f64 / FPS, score);
            }
            previous = current;
            count += 1;
        }
        if !child.wait().await?.success() || count == 0 {
            bail!("Decode video untuk deteksi gagal");
        }
        if best.1 == 0. {
            best.0 = count as f64 / FPS / 2.;
        }
        Ok(best)
    };
    tokio::time::timeout(Duration::from_secs(300), work)
        .await
        .context("Deteksi objek terlalu lama")?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cat(x: f32) -> Detection {
        Detection {
            class: 15,
            confidence: 0.9,
            x,
            y: 100.,
            w: 80.,
            h: 60.,
            steps: 0,
        }
    }
    #[test]
    fn movement_requires_same_object_across_three_frames() {
        let a = vec![cat(100.)];
        let mut b = vec![cat(110.)];
        assert_eq!(confirm(&a, &mut b), 0.);
        let mut c = vec![cat(120.)];
        assert_eq!(confirm(&b, &mut c), 0.9);
        assert_eq!(confirm(&[], &mut [cat(130.)]), 0.);
    }
    #[test]
    fn stationary_jitter_and_different_classes_do_not_trigger() {
        let a = vec![cat(100.)];
        let mut b = vec![cat(101.)];
        assert_eq!(confirm(&a, &mut b), 0.);
        b[0].class = 16;
        assert_eq!(confirm(&a, &mut b), 0.);
    }
    #[test]
    fn output_filters_small_objects_birds_and_insects_and_suppresses_duplicates() {
        let mut output = tract_ndarray::Array3::<f32>::zeros((1, 84, 5));
        for (i, (class, size, confidence)) in [
            (15, 80., 0.9),
            (15, 80., 0.8),
            (16, 4., 0.9),
            (14, 80., 0.9),
            (0, 80., 0.2),
        ]
        .iter()
        .enumerate()
        {
            output[[0, 0, i]] = 100.;
            output[[0, 1, i]] = 100.;
            output[[0, 2, i]] = *size;
            output[[0, 3, i]] = *size;
            output[[0, class + 4, i]] = *confidence;
        }
        let detections = decode(&output.into_tensor(), 0.5, 0.002).unwrap();
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].class, 15);
        assert!(!allowed(14));
        assert!(allowed(16));
        assert!(allowed(0));
    }
}
