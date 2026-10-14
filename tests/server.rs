use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use cctv::{
    camera,
    config::{Config, RetentionSettings},
    server::{self, App},
    storage::{RetentionStatus, Store},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
async fn fixture(
    dir: &std::path::Path,
) -> (
    Arc<App>,
    axum::Router,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let cfg:Config=serde_json::from_value(json!({"cameras":[{"name":"Test","ip":"127.0.0.1","deviceId":"123","password":"secret","v380Port":1}],"recording":{"enabled":true,"destination":"local","dir":dir}})).unwrap();
    let stop = CancellationToken::new();
    let (handle, task) = camera::spawn(cfg.cameras[0].clone(), None, None, stop.clone());
    let store = Store::new(cfg.recording.as_ref().unwrap());
    let app = Arc::new(App {
        config: Mutex::new(cfg),
        config_path: dir.join("config.json"),
        cameras: vec![handle],
        store: Some(store),
        retention: RetentionSettings {
            days: 0.0,
            check_seconds: 3600,
        },
        retention_status: Arc::new(Mutex::new(RetentionStatus::default())),
    });
    let router = server::router(app.clone(), dir.into());
    (app, router, stop, task)
}
#[tokio::test]
async fn no_password_leak_and_cross_origin_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (_, router, stop, task) = fixture(dir.path()).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/cameras")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = String::from_utf8(
        to_bytes(response.into_body(), 100000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!text.contains("secret"));
    assert!(!text.contains("\"password\""));
    assert!(text.contains("hasPassword"));
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cameras/0/reconnect")
                .header("host", "localhost:3380")
                .header("origin", "https://evil.invalid")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    stop.cancel();
    task.await.unwrap();
}
#[tokio::test]
async fn settings_preserve_blank_password_and_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let (app, router, stop, task) = fixture(dir.path()).await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/cameras/0")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name":"Renamed","password":"","v380Port":"8800"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cfg = app.config.lock().await;
    assert_eq!(cfg.cameras[0].password, "secret");
    assert_eq!(cfg.cameras[0].name, "Renamed");
    drop(cfg);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("config.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    stop.cancel();
    task.await.unwrap();
}
#[tokio::test]
async fn playback_ranges_and_traversal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cam123_20261014_120000.mp4"), b"0123456789").unwrap();
    let (_, router, stop, task) = fixture(dir.path()).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/recordings/cam123_20261014_120000.mp4")
                .header("range", "bytes=2-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(to_bytes(response.into_body(), 100).await.unwrap(), "2345");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/recordings/cam123_20261014_120000.mp4")
                .header("range", "bytes=10-")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/recordings/%2e%2e%2fconfig.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    stop.cancel();
    task.await.unwrap();
}
#[tokio::test]
async fn filters_and_pagination() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "cam123_20261014_120000.mp4",
        "cam123_20261014_130000.mp4",
        "cam999_20261014_120000.mp4",
    ] {
        std::fs::write(dir.path().join(name), b"video").unwrap();
    }
    let (_, router, stop, task) = fixture(dir.path()).await;
    let response=router.oneshot(Request::builder().uri("/api/recordings?date=2026-10-14&camera=0&from=12%3A00&to=13%3A00&size=1&page=2").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 100000).await.unwrap()).unwrap();
    assert_eq!(body["total"], 2);
    assert_eq!(body["pages"], 2);
    assert_eq!(body["items"][0]["name"], "cam123_20261014_120000.mp4");
    stop.cancel();
    task.await.unwrap();
}
#[tokio::test]
async fn preview_demand_is_released_with_client() {
    let dir = tempfile::tempdir().unwrap();
    let (app, router, stop, task) = fixture(dir.path()).await;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/stream/0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        app.cameras[0]
            .demand
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    drop(response);
    assert_eq!(
        app.cameras[0]
            .demand
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    stop.cancel();
    task.await.unwrap();
}
