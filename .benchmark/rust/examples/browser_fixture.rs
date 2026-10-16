//! Local synthetic camera and MP4 fixture for browser tests; no real CCTV credentials.
use anyhow::Result;
use cctv::{
    camera,
    config::{Config, RetentionSettings},
    server::{self, App},
    storage::{RetentionStatus, Store},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
#[tokio::main]
async fn main() -> Result<()> {
    let jpeg = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=size=160x120:rate=5",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-c:v",
            "mjpeg",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .output()?
        .stdout;
    let camera_listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = camera_listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = camera_listener.accept().await else {
                break;
            };
            let jpeg = jpeg.clone();
            tokio::spawn(async move {
                let mut packet = [0; 256];
                if stream.read_exact(&mut packet).await.is_err() {
                    return;
                }
                let kind = u32::from_le_bytes(packet[..4].try_into().unwrap());
                if kind == 1167 {
                    let mut reply = [0; 256];
                    reply[..4].copy_from_slice(&1168u32.to_le_bytes());
                    reply[4..8].copy_from_slice(&1001u32.to_le_bytes());
                    reply[12] = 30;
                    let _ = stream.write_all(&reply).await;
                } else if kind == 301 {
                    let mut reply = [0; 32];
                    reply[..4].copy_from_slice(&401u32.to_le_bytes());
                    reply[8..10].copy_from_slice(&20u16.to_le_bytes());
                    reply[10..14].copy_from_slice(&160u32.to_le_bytes());
                    reply[14..18].copy_from_slice(&120u32.to_le_bytes());
                    if stream.write_all(&reply).await.is_err()
                        || stream.read_exact(&mut packet).await.is_err()
                    {
                        return;
                    }
                    let mut payload = vec![0; 16];
                    payload.extend(jpeg);
                    let mut header = [0; 12];
                    header[0] = 127;
                    header[1] = 9;
                    header[3..5].copy_from_slice(&1u16.to_le_bytes());
                    header[7..9].copy_from_slice(&(payload.len() as u16).to_le_bytes());
                    loop {
                        if stream.write_all(&header).await.is_err()
                            || stream.write_all(&payload).await.is_err()
                        {
                            break;
                        }
                        let mut audio_header = [0u8; 12];
                        audio_header[0] = 127;
                        audio_header[1] = 26;
                        audio_header[3..5].copy_from_slice(&1u16.to_le_bytes());
                        audio_header[7..9].copy_from_slice(&512u16.to_le_bytes());
                        let mut audio = vec![0; 32];
                        audio.extend([0xd5; 480]);
                        if stream.write_all(&audio_header).await.is_err()
                            || stream.write_all(&audio).await.is_err()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            });
        }
    });
    let dir = tempfile::tempdir()?;
    let clip = format!(
        "cam123_{}.mp4",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=size=160x120:rate=5",
            "-t",
            "1",
            "-threads",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(dir.path().join(clip))
        .output()?;
    anyhow::ensure!(output.status.success(), "Fixture MP4 failed");
    let cfg: Config = serde_json::from_value(
        serde_json::json!({"cameras":[{"name":"Rust test camera","ip":"127.0.0.1","deviceId":"123","password":"test","v380Port":port}],"recording":{"enabled":true,"destination":"local","dir":dir.path()}}),
    )?;
    let cancel = CancellationToken::new();
    let (handle, _) = camera::spawn(cfg.cameras[0].clone(), None, None, cancel);
    let store = Store::new(cfg.recording.as_ref().unwrap());
    let app = Arc::new(App {
        config: Mutex::new(cfg),
        config_path: dir.path().join("config.json"),
        cameras: vec![handle],
        store: Some(store),
        retention: RetentionSettings {
            days: 0.0,
            check_seconds: 3600,
        },
        retention_status: Arc::new(Mutex::new(RetentionStatus::default())),
    });
    let listener = TcpListener::bind("127.0.0.1:3390").await?;
    println!("Browser fixture http://127.0.0.1:3390");
    axum::serve(listener, server::router(app, "public".into())).await?;
    Ok(())
}
