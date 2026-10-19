//! Persistent movement gate, followed by one YOLOv8 COCO object check.
//! No scene-change fallback: inference failures leave the clip unclassified.
use anyhow::{bail, Context, Result};
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
pub const VERSION: u8 = 3;
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
/// One AI image per clip, after persistent movement passes the cheap size gate.
pub async fn analyze(video: &Path) -> Result<(f64, f32)> {
    let confidence = number("MOTION_CONFIDENCE", 0.5)?;
    if confidence < crate::storage::MOTION_THRESHOLD {
        bail!("MOTION_CONFIDENCE harus >= 0.5");
    }
    let area = number("MOTION_MIN_AREA", 0.002)?;
    let scan = crate::motion_gate::scan(video, area).await?;
    if scan.regions.is_empty() {
        return Ok((scan.at, 0.));
    }
    static AI: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    // The permit stays INSIDE the worker even when the caller times out. Never start
    // another inference beside a timed-out operation on this small CPU.
    let permit = AI
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .context("AI masih sibuk; klip belum dianalisis")?;
    let mut grab = crate::process::analysis_ffmpeg();
    grab.args(["-hide_banner","-loglevel","error","-threads","1","-ss",&format!("{:.3}",scan.at),"-i"])
        .arg(video).args(["-an","-frames:v","1","-filter_threads","1","-vf",
        "scale=640:640:force_original_aspect_ratio=decrease,pad=640:640:(ow-iw)/2:(oh-ih)/2:color=0x727272",
        "-threads","1","-f","rawvideo","-pix_fmt","rgb24","-"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = grab.spawn()?;
    let mut stdout = child.stdout.take().context("AI frame stdout")?;
    let mut frame = vec![0u8; FRAME_BYTES];
    let read = async {
        stdout.read_exact(&mut frame).await?;
        // Closing the read side before wait prevents a malformed encoder blocking on output.
        drop(stdout);
        if !child.wait().await?.success() {
            bail!("Frame AI gagal didekode");
        }
        Ok::<_, anyhow::Error>(())
    };
    match tokio::time::timeout(Duration::from_secs(3), read).await {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!("Batas decode frame AI tercapai");
        }
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    let regions = scan.regions;
    std::thread::Builder::new()
        .name("motion-ai".into())
        .spawn(move || {
            let _permit = permit;
            // A dedicated thread avoids changing the priority of Tokio's shared worker pool.
            unsafe {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            }
            let result = (|| -> Result<f32> {
                let input =
                    tract_ndarray::Array4::from_shape_fn((1, 3, SIDE, SIDE), |(_, c, y, x)| {
                        frame[(y * SIDE + x) * 3 + c] as f32 / 255.
                    })
                    .into_tensor();
                let outputs = model()?.run(tvec!(input.into()))?;
                let detections = decode(&outputs[0], confidence, area)?;
                Ok(verified_score(&detections, &regions))
            })();
            let _ = send.send(result);
        })?;
    let score = tokio::time::timeout(Duration::from_secs(8), receive)
        .await
        .context("Batas waktu AI tercapai; klip belum dianalisis")???;
    Ok((scan.at, score))
}
fn verified_score(detections: &[Detection], regions: &[crate::motion_gate::Region]) -> f32 {
    use crate::motion_gate::{Region, SIDE as GATE_SIDE};
    let ratio = SIDE as f32 / GATE_SIDE as f32;
    detections
        .iter()
        .filter(|d| {
            let object = Region {
                x: d.x - d.w / 2.,
                y: d.y - d.h / 2.,
                w: d.w,
                h: d.h,
            };
            regions.iter().any(|r| {
                let changed = Region {
                    x: r.x * ratio,
                    y: r.y * ratio,
                    w: r.w * ratio,
                    h: r.h * ratio,
                };
                object.intersection(changed) >= 0.15 * object.area().min(changed.area())
            })
        })
        .map(|d| d.confidence)
        .fold(0., f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_object_elsewhere_does_not_confirm_motion() {
        let cat = Detection {
            class: 15,
            confidence: 0.9,
            x: 100.,
            y: 100.,
            w: 80.,
            h: 60.,
        };
        let elsewhere = crate::motion_gate::Region {
            x: 180.,
            y: 180.,
            w: 30.,
            h: 30.,
        };
        assert_eq!(verified_score(&[cat.clone()], &[elsewhere]), 0.);
        let overlapping = crate::motion_gate::Region {
            x: 30.,
            y: 30.,
            w: 20.,
            h: 20.,
        };
        assert_eq!(verified_score(&[cat], &[overlapping]), 0.9);
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
