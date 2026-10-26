use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use cctv::{
    auth::Auth,
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

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: &str,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost:3380")
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}
fn cookie_of(response: &axum::response::Response) -> String {
    let value = response.headers()["set-cookie"].to_str().unwrap();
    assert!(value.contains("HttpOnly") && value.contains("SameSite=Strict"));
    assert!(value.contains("Max-Age=300"));
    value.split(';').next().unwrap().to_owned()
}
#[tokio::test]
async fn pin_gate_blocks_everything_until_login_and_locks_out_guessing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "secret page").unwrap();
    let (app, _, stop, task) = fixture(dir.path()).await;
    let auth = Arc::new(Auth::new("123456").unwrap());
    let router = server::router_with_auth(app, dir.path().into(), Some(auth));
    for uri in ["/api/cameras", "/stream/0", "/snapshot/0", "/recordings/x.mp4"] {
        assert_eq!(call(&router, "GET", uri, None, "").await.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    for uri in ["/", "/index.html", "/pkg/cctv_ui.js", "/bootstrap.js"] {
        let response = call(&router, "GET", uri, None, "").await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(response.headers()["location"], "/login");
    }
    assert_eq!(call(&router, "GET", "/login", None, "").await.status(), StatusCode::OK);
    let forged = Some("cctv_session=deadbeef");
    assert_eq!(call(&router, "GET", "/api/cameras", forged, "").await.status(), StatusCode::UNAUTHORIZED);
    let health = call(&router, "GET", "/healthz", None, "").await;
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    assert!(health.headers().get("set-cookie").is_none());
    // Wrong guesses never create a session; the fifth locks even the right PIN out.
    for _ in 0..4 {
        let response = call(&router, "POST", "/login", None, "pin=000000").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get("set-cookie").is_none());
    }
    assert_eq!(call(&router, "POST", "/login", None, "pin=000000").await.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(call(&router, "POST", "/login", None, "pin=123456").await.status(), StatusCode::TOO_MANY_REQUESTS);
    stop.cancel();
    let _ = task.await;
}
#[tokio::test]
async fn correct_pin_grants_session_that_expires() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "secret page").unwrap();
    let (app, _, stop, task) = fixture(dir.path()).await;
    let auth = Arc::new(Auth::with_ttl("123456", std::time::Duration::from_millis(400)).unwrap());
    let router = server::router_with_auth(app, dir.path().into(), Some(auth));
    let response = call(&router, "POST", "/login", None, "pin=123456").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = cookie_of(&response);
    assert_eq!(call(&router, "GET", "/api/cameras", Some(&cookie), "").await.status(), StatusCode::OK);
    assert_eq!(call(&router, "GET", "/", Some(&cookie), "").await.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(call(&router, "GET", "/api/cameras", Some(&cookie), "").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(call(&router, "GET", "/", Some(&cookie), "").await.status(), StatusCode::SEE_OTHER);
    stop.cancel();
    let _ = task.await;
}
#[test]
fn pin_must_be_six_digits() {
    for bad in ["", "12345", "1234567", "12345a", "١٢٣٤٥٦"] {
        assert!(Auth::new(bad).is_err(), "{bad}");
    }
    assert!(Auth::new("000123").is_ok());
}

#[tokio::test]
async fn background_polling_does_not_extend_idle_deadline_and_streams_close() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _, stop, task) = fixture(dir.path()).await;
    let auth = Arc::new(Auth::with_ttl("123456", std::time::Duration::from_millis(600)).unwrap());
    let router = server::router_with_auth(app, dir.path().into(), Some(auth));
    let response = call(&router, "POST", "/login", None, "pin=123456").await;
    let cookie = cookie_of(&response);
    let response = call(&router, "GET", "/api/session", Some(&cookie), "").await;
    let first: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    let video = call(&router, "GET", "/stream/0", Some(&cookie), "").await;
    let audio = call(&router, "GET", "/audio/0", Some(&cookie), "").await;
    assert_eq!(video.status(), StatusCode::OK);
    assert_eq!(audio.status(), StatusCode::OK);
    for _ in 0..3 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let response = call(&router, "GET", "/api/session", Some(&cookie), "").await;
        assert_eq!(response.status(), StatusCode::OK);
        let now: Value = serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
        assert!(now["remainingMs"].as_u64().unwrap() < first["remainingMs"].as_u64().unwrap());
    }
    let closed = async {
        let (video, audio) = tokio::join!(to_bytes(video.into_body(), 10000), to_bytes(audio.into_body(), 10000));
        assert!(video.unwrap().is_empty());
        assert!(audio.unwrap().is_empty());
    };
    tokio::time::timeout(std::time::Duration::from_millis(500), closed).await.expect("streams must close at the original login deadline");
    assert_eq!(call(&router, "GET", "/api/session", Some(&cookie), "").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(call(&router, "GET", "/api/cameras", Some(&cookie), "").await.status(), StatusCode::UNAUTHORIZED);
    stop.cancel();
    let _ = task.await;
}

#[tokio::test]
async fn user_activity_extends_cookie_and_existing_streams_but_cannot_revive_expired_session() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let (app, _, stop, task) = fixture(dir.path()).await;
    let auth = Arc::new(Auth::with_ttl("123456", Duration::from_millis(800)).unwrap());
    let router = server::router_with_auth(app, dir.path().into(), Some(auth));
    let response = call(&router, "POST", "/login", None, "pin=123456").await;
    let cookie = cookie_of(&response);
    let video = call(&router, "GET", "/stream/0", Some(&cookie), "").await;
    let audio = call(&router, "GET", "/audio/0", Some(&cookie), "").await;
    let mut video = Box::pin(to_bytes(video.into_body(), 10000));
    let mut audio = Box::pin(to_bytes(audio.into_body(), 10000));
    tokio::time::sleep(Duration::from_millis(450)).await;
    let response = call(&router, "POST", "/api/session", Some(&cookie), "{}").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(cookie_of(&response), cookie, "activity must refresh the cookie for the same token");
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert_eq!(call(&router, "GET", "/api/session", Some(&cookie), "").await.status(), StatusCode::OK, "session must outlive the original login deadline");
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut video).await.is_err(), "video must stay open beyond original deadline");
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut audio).await.is_err(), "audio must stay open beyond original deadline");
    let (video, audio) = tokio::time::timeout(Duration::from_millis(700), async { tokio::join!(video, audio) }).await.expect("streams must close after inactivity");
    assert!(video.unwrap().is_empty());
    assert!(audio.unwrap().is_empty());
    assert_eq!(call(&router, "POST", "/api/session", Some(&cookie), "{}").await.status(), StatusCode::UNAUTHORIZED);
    stop.cancel();
    let _ = task.await;
}
