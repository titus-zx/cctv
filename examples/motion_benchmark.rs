//! cargo run --release --example motion_benchmark -- /path/to/clip.mp4 2
use anyhow::{Context, Result};
use std::{path::PathBuf, time::Instant};
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let video = PathBuf::from(std::env::args().nth(1).context("Berikan path klip MP4")?);
    let runs: usize = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "2".into())
        .parse()?;
    let temp = tempfile::tempdir()?;
    for run in 1..=runs {
        let start = Instant::now();
        let result = cctv::thumbs::analyze(&video, &temp.path().join(format!("{run}.jpg"))).await;
        match result {
            Ok(score) => println!(
                "{}",
                serde_json::json!({"run":run,"seconds":start.elapsed().as_secs_f64(),"motion":score})
            ),
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"run":run,"seconds":start.elapsed().as_secs_f64(),"motion":null,"error":format!("{error:#}")})
                );
                return Err(error);
            }
        }
    }
    Ok(())
}
