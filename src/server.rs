use crate::{
    auth::{self, Auth, LoginError},
    camera::{CameraCommand, CameraHandle},
    config::{Config, RetentionSettings},
    storage::{self, ByteRange, RetentionStatus, Store},
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{Local, Timelike};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, path::PathBuf, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};
use tower_http::services::ServeDir;

pub struct App {
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub cameras: Vec<Arc<CameraHandle>>,
    pub store: Option<Store>,
    pub retention: RetentionSettings,
    pub retention_status: Arc<Mutex<RetentionStatus>>,
}
pub struct ApiError(pub StatusCode, pub String);
impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    fn missing() -> Self {
        Self(StatusCode::NOT_FOUND, "Tidak ditemukan.".into())
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self::bad(error.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
type ApiResult = std::result::Result<Response, ApiError>;
pub fn router(app: Arc<App>, public_dir: PathBuf) -> Router {
    router_with_auth(app, public_dir, None)
}
/// With `auth`, every path except the login page and its assets requires a valid session.
pub fn router_with_auth(app: Arc<App>, public_dir: PathBuf, auth: Option<Arc<Auth>>) -> Router {
    let mut router = Router::new()
        .route("/api/cameras", get(cameras))
        .route("/api/retention", get(retention))
        .route("/api/recordings", get(recordings))
        .route("/api/cameras/{id}", post(update_camera))
        .route("/api/cameras/{id}/reconnect", post(reconnect))
        .route("/api/cameras/{id}/ptz", post(ptz))
        .route("/api/cameras/{id}/talk", post(talk))
        .route("/stream/{id}", get(stream))
        .route("/audio/{id}", get(audio))
        .route("/snapshot/{id}", get(snapshot))
        .route("/recordings/{name}", get(clip))
        .fallback_service(ServeDir::new(public_dir))
        .layer(DefaultBodyLimit::max(16384));
    if let Some(auth) = auth {
        router = router
            .route("/login", get(login_page).post(login_submit))
            .layer(middleware::from_fn_with_state(auth.clone(), guard))
            .layer(Extension(auth));
    }
    router.layer(middleware::from_fn(headers)).with_state(app)
}
const LOGIN_PAGE: &str = include_str!("login.html");
fn login_html(status: StatusCode, error: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        LOGIN_PAGE.replace("{{error}}", error),
    )
        .into_response()
}
fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(name, value)| (name == auth::COOKIE).then_some(value))
}
/// Default deny: only the login page and the assets it needs are public.
async fn guard(
    State(auth): State<Arc<Auth>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if matches!(path, "/login" | "/login.js" | "/style.css" | "/favicon.svg")
        || session_token(request.headers()).is_some_and(|t| auth.valid(t))
    {
        return next.run(request).await;
    }
    let api = ["/api/", "/stream/", "/audio/", "/snapshot/", "/recordings/"]
        .iter()
        .any(|p| path.starts_with(p));
    if api {
        ApiError(StatusCode::UNAUTHORIZED, "Sesi berakhir; masukkan PIN lagi.".into())
            .into_response()
    } else {
        axum::response::Redirect::to("/login").into_response()
    }
}
async fn login_page(Extension(auth): Extension<Arc<Auth>>, headers: HeaderMap) -> Response {
    if session_token(&headers).is_some_and(|t| auth.valid(t)) {
        return axum::response::Redirect::to("/").into_response();
    }
    login_html(StatusCode::OK, "")
}
async fn login_submit(Extension(auth): Extension<Arc<Auth>>, body: Bytes) -> Response {
    let pin = url::form_urlencoded::parse(&body)
        .find(|(k, _)| k == "pin")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    match auth.login(pin.trim()) {
        Ok(token) => (
            StatusCode::SEE_OTHER,
            [
                (header::LOCATION, "/".to_owned()),
                (
                    header::SET_COOKIE,
                    format!(
                        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
                        auth::COOKIE,
                        auth::SESSION_TTL.as_secs()
                    ),
                ),
            ],
        )
            .into_response(),
        Err(LoginError::Wrong) => login_html(StatusCode::UNAUTHORIZED, "PIN salah."),
        Err(LoginError::Locked(seconds)) => login_html(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("Terlalu banyak percobaan. Coba lagi dalam {seconds} detik."),
        ),
    }
}
async fn headers(request: Request<Body>, next: Next) -> Response {
    if request.method() == axum::http::Method::POST
        && let Some(origin) = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
    {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if origin != format!("http://{host}") && origin != format!("https://{host}") {
            return ApiError(StatusCode::FORBIDDEN, "Origin tidak diizinkan.".into())
                .into_response();
        }
    }
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    h.insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert("content-security-policy",header::HeaderValue::from_static("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'"));
    response
}
fn camera(app: &App, id: usize) -> std::result::Result<Arc<CameraHandle>, ApiError> {
    app.cameras.get(id).cloned().ok_or_else(ApiError::missing)
}
fn json_body(headers: &HeaderMap, body: &Bytes) -> std::result::Result<Value, ApiError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        != Some("application/json")
    {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Gunakan application/json.".into(),
        ));
    }
    serde_json::from_slice(body).map_err(|_| ApiError::bad("JSON tidak valid."))
}
async fn cameras(State(app): State<Arc<App>>) -> Json<Value> {
    let mut cameras = vec![];
    for (id, camera) in app.cameras.iter().enumerate() {
        cameras.push(camera.public_status(id).await);
    }
    Json(json!(cameras))
}
async fn retention(State(app): State<Arc<App>>) -> Json<Value> {
    Json(storage::retention_json(
        app.retention,
        &*app.retention_status.lock().await,
    ))
}
async fn reconnect(
    State(app): State<Arc<App>>,
    Path(id): Path<usize>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    json_body(&headers, &body)?;
    camera(&app, id)?
        .commands
        .send(CameraCommand::Reconnect)
        .await
        .map_err(|_| ApiError::bad("Kamera dihentikan."))?;
    Ok(Json(json!({"ok":true})).into_response())
}
async fn ptz(
    State(app): State<Arc<App>>,
    Path(id): Path<usize>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let value = json_body(&headers, &body)?;
    camera(&app, id)?
        .ptz(
            value["direction"]
                .as_str()
                .ok_or_else(|| ApiError::bad("Arah tidak valid."))?
                .into(),
        )
        .await?;
    Ok(Json(json!({"ok":true})).into_response())
}
async fn talk(
    State(app): State<Arc<App>>,
    Path(id): Path<usize>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Gunakan application/octet-stream.".into(),
        ));
    }
    if body.is_empty() || body.len() > 480 {
        return Err(ApiError::bad("Ukuran audio tidak valid."));
    }
    camera(&app, id)?.talk(body).await?;
    Ok(Json(json!({"ok":true})).into_response())
}
async fn update_camera(
    State(app): State<Arc<App>>,
    Path(id): Path<usize>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let value = json_body(&headers, &body)?;
    let handle = camera(&app, id)?;
    let mut cfg = app.config.lock().await;
    let previous = cfg.cameras.get(id).ok_or_else(ApiError::missing)?.clone();
    let mut merged = serde_json::to_value(&previous).map_err(anyhow::Error::from)?;
    for name in [
        "ip",
        "deviceId",
        "username",
        "mode",
        "quality",
        "v380Port",
        "previewFps",
        "recordingCrf",
        "recordingEncoder",
        "recordingFps",
        "audio",
        "record",
        "rtspUrl",
    ] {
        if let Some(v) = value.get(name) {
            merged[name] = v.clone();
        }
    }
    if let Some(port) = merged["v380Port"].as_str() {
        merged["v380Port"] = json!(
            port.parse::<u16>()
                .map_err(|_| ApiError::bad("Port tidak valid."))?
        );
    }
    merged["name"] = json!(
        value["name"]
            .as_str()
            .unwrap_or(&previous.name)
            .chars()
            .take(80)
            .collect::<String>()
    );
    if let Some(password) = value.get("password")
        && password.as_str() != Some("")
    {
        merged["password"] = password.clone();
    }
    if merged["mode"] != "rtsp" {
        merged.as_object_mut().unwrap().remove("rtspUrl");
    }
    let updated: crate::config::Camera = serde_json::from_value(merged)
        .map_err(|_| ApiError::bad("Konfigurasi kamera tidak valid."))?;
    updated.validate()?;
    cfg.cameras[id] = updated.clone();
    if let Err(error) = cfg.save(&app.config_path) {
        cfg.cameras[id] = previous;
        return Err(error.into());
    }
    *handle.camera.write().await = updated;
    drop(cfg);
    handle
        .commands
        .send(CameraCommand::Reconnect)
        .await
        .map_err(|_| ApiError::bad("Kamera dihentikan."))?;
    Ok(Json(json!({"ok":true})).into_response())
}
async fn stream(State(app): State<Arc<App>>, Path(id): Path<usize>) -> ApiResult {
    let handle = camera(&app, id)?;
    let (guard, mut receiver) = handle.subscribe();
    let stream = async_stream::stream! {let _guard=guard;loop{match receiver.recv().await{
        Ok(jpeg)=>{let mut frame=Vec::with_capacity(jpeg.len()+100);frame.extend(format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",jpeg.len()).as_bytes());frame.extend_from_slice(&jpeg);frame.extend(b"\r\n");yield Ok::<_,Infallible>(Bytes::from(frame));},
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>continue,Err(_)=>break
    }}};
    Ok((
        [(
            header::CONTENT_TYPE,
            "multipart/x-mixed-replace; boundary=frame",
        )],
        Body::from_stream(stream),
    )
        .into_response())
}
async fn audio(State(app): State<Arc<App>>, Path(id): Path<usize>) -> ApiResult {
    let handle = camera(&app, id)?;
    let mut receiver = handle.audio.subscribe();
    let stream = async_stream::stream! {loop{match receiver.recv().await{Ok(bytes)=>yield Ok::<_,Infallible>(bytes),Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>continue,Err(_)=>break}}};
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Body::from_stream(stream),
    )
        .into_response())
}
async fn snapshot(State(app): State<Arc<App>>, Path(id): Path<usize>) -> ApiResult {
    let handle = camera(&app, id)?;
    let jpeg = handle
        .snapshot()
        .await
        .map_err(|e| ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
    let name = format!(
        "attachment; filename=\"v380-{}-{}.jpg\"",
        handle.camera.read().await.device_id,
        chrono::Utc::now().timestamp_millis()
    );
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CONTENT_DISPOSITION, name.as_str()),
        ],
        jpeg,
    )
        .into_response())
}
#[derive(Default, Deserialize)]
struct RecordingQuery {
    date: Option<String>,
    camera: Option<String>,
    from: Option<String>,
    to: Option<String>,
    page: Option<usize>,
    size: Option<usize>,
}
fn minutes(value: Option<&str>, fallback: u32) -> u32 {
    value
        .and_then(|s| {
            let (a, b) = s.split_once(':')?;
            let (h, m) = (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?);
            if h < 24 && m < 60 {
                Some(h * 60 + m)
            } else {
                None
            }
        })
        .unwrap_or(fallback)
}
async fn recordings(State(app): State<Arc<App>>, Query(query): Query<RecordingQuery>) -> ApiResult {
    let Some(store) = &app.store else {
        return Ok(Json(
            json!({"enabled":false,"items":[],"total":0,"page":1,"pages":1,"size":50}),
        )
        .into_response());
    };
    let day = query
        .date
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let mut clips = store.list(&day).await?;
    for clip in &mut clips {
        for (id, handle) in app.cameras.iter().enumerate() {
            let camera = handle.camera.read().await;
            if camera.device_id == clip.device_id {
                clip.camera_id = Some(id);
                clip.camera = Some(camera.name.clone());
                break;
            }
        }
    }
    let from = minutes(query.from.as_deref(), 0);
    let to = minutes(query.to.as_deref(), 1439);
    clips.retain(|clip| {
        let local = clip.start.with_timezone(&Local);
        let minute = local.hour() * 60 + local.minute();
        minute >= from
            && minute <= to
            && query
                .camera
                .as_deref()
                .is_none_or(|s| s == "all" || clip.camera_id.is_some_and(|id| id.to_string() == s))
    });
    clips.sort_by_key(|clip| std::cmp::Reverse(clip.start));
    let total = clips.len();
    let size = query.size.unwrap_or(50).clamp(1, 200);
    let pages = total.div_ceil(size).max(1);
    let page = query.page.unwrap_or(1).clamp(1, pages);
    let items: Vec<_> = clips
        .into_iter()
        .skip((page - 1) * size)
        .take(size)
        .collect();
    Ok(Json(
        json!({"enabled":true,"items":items,"total":total,"page":page,"pages":pages,"size":size}),
    )
    .into_response())
}
async fn clip(
    State(app): State<Arc<App>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let store = app.store.as_ref().ok_or_else(ApiError::missing)?;
    // A clip still on local disk is read directly, even when the destination is remote.
    let local = store.local_size(&name).await;
    let size = match local {
        Some(size) => size,
        None => store.size(&name).await?.ok_or_else(ApiError::missing)?,
    };
    let range = storage::byte_range(
        headers.get(header::RANGE).and_then(|v| v.to_str().ok()),
        size,
    );
    if range == ByteRange::Unsatisfiable {
        return Ok((
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{size}"))],
        )
            .into_response());
    }
    let (start, count, status) = match range {
        ByteRange::Part { start, end } => (start, end - start + 1, StatusCode::PARTIAL_CONTENT),
        _ => (0, size, StatusCode::OK),
    };
    let body = if local.is_some() || store.remote.is_none() {
        let mut file = tokio::fs::File::open(store.dir.join(&name))
            .await
            .map_err(|_| ApiError::missing())?;
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(anyhow::Error::from)?;
        Body::from_stream(tokio_util::io::ReaderStream::new(file.take(count)))
    } else {
        let mut command = tokio::process::Command::new(&store.bin);
        command
            .args([
                "cat",
                "--offset",
                &start.to_string(),
                "--count",
                &count.to_string(),
                "--",
                &store.target(&name)?,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(anyhow::Error::from)?;
        let mut output = child.stdout.take().unwrap();
        let stream = async_stream::try_stream! {let mut buffer=vec![0;65536];loop{let n=output.read(&mut buffer).await?;if n==0{break}yield Bytes::copy_from_slice(&buffer[..n]);}let _=child.wait().await;};
        let stream: std::pin::Pin<
            Box<dyn futures_util::Stream<Item = std::io::Result<Bytes>> + Send>,
        > = Box::pin(stream);
        Body::from_stream(stream)
    };
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, count.to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{size}", start, start + count - 1),
        );
    }
    Ok(response.body(body).unwrap())
}
