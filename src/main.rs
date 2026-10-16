use anyhow::Result;
use cctv::{
    auth::Auth,
    camera,
    config::{Config, RetentionSettings},
    recording,
    server::{self, App},
    storage::{self, RetentionStatus, Store, UploadQueue},
};
use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() && args[0] != "serve" {
        return cctv::cli::run(args).await;
    }
    let path = PathBuf::from(std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.json".into()));
    // Fail closed: never serve cameras without a PIN.
    let auth = Arc::new(Auth::from_env()?);
    let cfg = Config::load(&path)?;
    let retention = RetentionSettings::new(cfg.recording.as_ref())?;
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(cfg.port);
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let cancel = CancellationToken::new();
    let uploads_cancel = CancellationToken::new();
    let store = cfg.recording.as_ref().filter(|r| r.enabled).map(Store::new);
    let queue = if let (Some(recording), Some(store)) = (&cfg.recording, &store) {
        let queue = UploadQueue::new(
            store.clone(),
            recording.keep_local || recording.destination == "local",
            uploads_cancel.clone(),
        );
        queue.recover(&recording.dir).await?;
        Some(queue)
    } else {
        None
    };
    if cfg
        .cameras
        .iter()
        .any(|c| c.record && c.recording_encoder != "cpu")
    {
        let _ = recording::hardware_available().await;
    }
    let mut tasks = vec![];
    if let Some(store) = store.clone().filter(|s| s.remote.is_some()) {
        let stop = cancel.clone();
        tasks.push(tokio::spawn(async move {
            // Keep the playback index fresh in the background; the page only reads the index.
            let mut ticks = 0u32;
            loop {
                if ticks % 40 == 0 {
                    tokio::select! {_=stop.cancelled()=>break,_=store.sync_index()=>{}}
                }
                tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(15))=>{}}
                store.index.persist().await;
                ticks = ticks.wrapping_add(1);
            }
            store.index.persist().await;
        }));
    }
    let mut cameras = vec![];
    for camera in &cfg.cameras {
        let (handle, task) = camera::spawn(
            camera.clone(),
            cfg.recording.clone(),
            queue.clone(),
            cancel.clone(),
        );
        cameras.push(handle);
        tasks.push(task);
    }
    let retention_status = Arc::new(Mutex::new(RetentionStatus::default()));
    let app = Arc::new(App {
        config: Mutex::new(cfg),
        config_path: path,
        cameras,
        store: store.clone(),
        retention,
        retention_status: retention_status.clone(),
    });
    if let (Some(store), Some(queue)) = (store, queue)
        && retention.days > 0.0
    {
        let app = app.clone();
        let stop = cancel.clone();
        tasks.push(tokio::spawn(async move{
            let mut timer=tokio::time::interval(Duration::from_secs(retention.check_seconds));loop{tokio::select!{
                _=stop.cancelled()=>break,
                _=timer.tick()=>{let ids:HashSet<_>=app.config.lock().await.cameras.iter().map(|c|c.device_id.clone()).collect();let mut state=retention_status.lock().await;
                    let result=tokio::select!{_ = stop.cancelled()=>break,result=storage::clean_retention(&store,&queue,&ids,retention.days,chrono::Utc::now(),&mut state)=>result};
                    if result.is_err(){state.error=Some("Pembersihan retensi gagal; file yang belum diunggah tetap dipertahankan.".into());}
                }
            }}
        }));
    }
    let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
    println!(
        "CCTV Mas Titus Rust: http://{host}:{}",
        std::env::var("PUBLIC_PORT").unwrap_or_else(|_| port.to_string())
    );
    let shutdown = cancel.clone();
    let signal = tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {_ = tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        shutdown.cancel();
    });
    let router = server::router_with_auth(
        app,
        PathBuf::from(std::env::var("PUBLIC_DIR").unwrap_or_else(|_| "public".into())),
        Some(auth),
    );
    let stopped = cancel.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
    });
    cancel.cancelled().await;
    let drain = async {
        for task in tasks {
            let _ = task.await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(11), drain).await;
    uploads_cancel.cancel();
    server.abort();
    signal.abort();
    Ok(())
}
