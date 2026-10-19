use js_sys::{Array, Float32Array, Function, Object, Reflect, Uint8Array};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Element, Event, EventTarget, RequestInit, Response};

#[derive(Default)]
struct Ui {
    cameras: Vec<Value>,
    session_deadline: Option<f64>,
    cards: HashMap<usize, Element>,
    loading: bool,
    playback: bool,
    clips: Vec<Value>,
    current: Option<usize>,
    page: usize,
    pages: usize,
    sound_id: Option<usize>,
    sound_generation: u64,
    audio_context: Option<JsValue>,
    audio_abort: Option<web_sys::AbortController>,
    audio_next: f64,
    talk_held: bool,
    talk_context: Option<JsValue>,
    talk_stream: Option<JsValue>,
    talk_node: Option<JsValue>,
    talk_source: Option<JsValue>,
    talk_position: f64,
    talk_pending: Vec<u8>,
    talk_callback: Option<Closure<dyn FnMut(JsValue)>>,
    ptz_callback: Option<Closure<dyn FnMut()>>,
    ptz_timer: Option<i32>,
    ptz_id: Option<usize>,
}
thread_local! {static UI:RefCell<Ui>=RefCell::new(Ui{page:1,pages:1,..Ui::default()});}
fn window() -> web_sys::Window {
    web_sys::window().unwrap()
}
fn document() -> web_sys::Document {
    window().document().unwrap()
}
fn error(message: &str) -> JsValue {
    JsValue::from_str(message)
}
fn get(object: &JsValue, key: &str) -> JsValue {
    Reflect::get(object, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}
fn set(object: &JsValue, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(object, &JsValue::from_str(key), &value.into());
}
fn call(object: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f = get(object, name).dyn_into::<Function>()?;
    let array = Array::new();
    for value in args {
        array.push(value);
    }
    f.apply(object, &array)
}
async fn promise(value: JsValue) -> Result<JsValue, JsValue> {
    JsFuture::from(js_sys::Promise::resolve(&value)).await
}
fn q(selector: &str) -> Result<Element, JsValue> {
    document()
        .query_selector(selector)?
        .ok_or_else(|| error("Elemen halaman tidak tersedia."))
}
fn child(parent: &Element, selector: &str) -> Result<Element, JsValue> {
    parent
        .query_selector(selector)?
        .ok_or_else(|| error("Elemen kamera tidak tersedia."))
}
fn text(element: &Element, value: &str) {
    element.set_text_content(Some(value));
}
fn text_at(selector: &str, value: &str) {
    if let Ok(e) = q(selector) {
        text(&e, value)
    }
}
fn hidden(element: &Element, hide: bool) {
    if hide {
        let _ = element.set_attribute("hidden", "");
    } else {
        let _ = element.remove_attribute("hidden");
    }
}
fn hidden_at(selector: &str, hide: bool) {
    if let Ok(e) = q(selector) {
        hidden(&e, hide)
    }
}
fn value(selector: &str) -> String {
    q(selector)
        .ok()
        .map(|e| get(e.as_ref(), "value").as_string().unwrap_or_default())
        .unwrap_or_default()
}
fn checked(selector: &str) -> bool {
    q(selector)
        .ok()
        .and_then(|e| get(e.as_ref(), "checked").as_bool())
        .unwrap_or(false)
}
fn value_at(selector: &str, v: &str) {
    if let Ok(e) = q(selector) {
        set(e.as_ref(), "value", v)
    }
}
fn disabled_at(selector: &str, disabled: bool) {
    if let Ok(e) = q(selector) {
        set(e.as_ref(), "disabled", disabled)
    }
}
fn bind<F: FnMut(Event) + 'static>(
    target: &EventTarget,
    name: &str,
    handler: F,
) -> Result<(), JsValue> {
    let closure = Closure::wrap(Box::new(handler) as Box<dyn FnMut(Event)>);
    target.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}
fn interval<F: FnMut() + 'static>(ms: i32, f: F) -> Result<i32, JsValue> {
    let closure = Closure::wrap(Box::new(f) as Box<dyn FnMut()>);
    let id = window().set_interval_with_callback_and_timeout_and_arguments_0(
        closure.as_ref().unchecked_ref(),
        ms,
    )?;
    closure.forget();
    Ok(id)
}
fn later<F: FnOnce() + 'static>(ms: i32, f: F) {
    let callback = Closure::once_into_js(f);
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), ms);
}
fn message(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| get(error, "message").as_string())
        .unwrap_or_else(|| "Permintaan gagal.".into())
}
fn toast(message: &str) {
    text_at("#toast", message);
    hidden_at("#toast", false);
    later(4000, || hidden_at("#toast", true));
}
fn expire_session() {
    release_ptz();
    stop_talk();
    stop_sound();
    if let Some(context) = UI.with(|ui| ui.borrow_mut().audio_context.take()) {
        let _ = call(&context, "close", &[]);
    }
    if let Ok(video) = q("#pb-video") {
        let _ = call(video.as_ref(), "pause", &[]);
        let _ = video.remove_attribute("src");
        let _ = call(video.as_ref(), "load", &[]);
    }
    let _ = window().location().replace("/login");
}
fn check_session() {
    let expired = UI.with(|ui| ui.borrow().session_deadline.is_some_and(|d| js_sys::Date::now() >= d));
    if expired { expire_session(); }
}
async fn initialize_session() {
    // Anchor before the request so network latency can only shorten the viewing period.
    let started = js_sys::Date::now();
    match api("/api/session", None).await {
        Ok(session) => {
            let remaining = session["remainingMs"].as_f64().unwrap_or(0.);
            UI.with(|ui| ui.borrow_mut().session_deadline = Some(started + remaining));
            later(remaining.min(300_000.) as i32, expire_session);
            check_session();
        }
        Err(_) => expire_session(),
    }
}
async fn response(url: &str, body: Option<&Value>) -> Result<Response, JsValue> {
    let init = RequestInit::new();
    let headers = web_sys::Headers::new()?;
    if let Some(body) = body {
        init.set_method("POST");
        headers.set("Content-Type", "application/json")?;
        init.set_body(&JsValue::from_str(&body.to_string()));
    }
    headers.set("Cache-Control", "no-cache")?;
    init.set_headers(headers.as_ref());
    let response = JsFuture::from(window().fetch_with_str_and_init(url, &init))
        .await?
        .dyn_into::<Response>()?;
    if response.status() == 401 {
        // Session expired: force the PIN page again.
        expire_session();
    }
    Ok(response)
}
async fn api(url: &str, body: Option<Value>) -> Result<Value, JsValue> {
    let response = response(url, body.as_ref()).await?;
    let text = JsFuture::from(response.text()?)
        .await?
        .as_string()
        .unwrap_or_default();
    let value: Value =
        serde_json::from_str(&text).map_err(|_| error("Respons server tidak valid."))?;
    if !response.ok() {
        return Err(error(
            value["error"].as_str().unwrap_or("Permintaan gagal."),
        ));
    }
    Ok(value)
}
fn field<'a>(v: &'a Value, name: &str) -> &'a str {
    v[name].as_str().unwrap_or("")
}
fn id(v: &Value) -> usize {
    v["id"].as_u64().unwrap_or(0) as usize
}
fn locale(date: &JsValue, kind: &str) -> String {
    let options = Object::new();
    if kind == "toLocaleTimeString" {
        set(options.as_ref(), "hour", "2-digit");
        set(options.as_ref(), "minute", "2-digit");
        set(options.as_ref(), "second", "2-digit");
    }
    call(date, kind, &[JsValue::from_str("id-ID"), options.into()])
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}
fn camera_by_id(camera_id: usize) -> Option<Value> {
    UI.with(|ui| {
        ui.borrow()
            .cameras
            .iter()
            .find(|c| id(c) == camera_id)
            .cloned()
    })
}
fn update_mode() {
    let mode = value("[name=mode]");
    hidden_at("#rtsp-field", mode != "rtsp");
    hidden_at("#cloud-note", mode != "cloud");
    if let Ok(ip) = q("[name=ip]") {
        set(ip.as_ref(), "required", mode != "cloud");
        set(ip.as_ref(), "readOnly", mode == "cloud");
    }
    if let Ok(port) = q("[name=v380Port]") {
        set(port.as_ref(), "readOnly", mode == "cloud")
    }
}
fn open_settings(camera_id: usize) {
    if let Some(camera) = camera_by_id(camera_id) {
        if let Ok(form) = q("#settings-form") {
            let _ = call(form.as_ref(), "reset", &[]);
        }
        for field_name in [
            "id", "name", "ip", "deviceId", "username", "mode", "quality", "v380Port",
        ] {
            let v = &camera[field_name];
            let value = v
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string());
            value_at(&format!("[name={field_name}]"), &value);
        }
        if let Ok(input) = q("[name=password]") {
            set(
                input.as_ref(),
                "placeholder",
                if camera["hasPassword"] == true {
                    "Kosongkan untuk mempertahankan password"
                } else {
                    "Masukkan password kamera"
                },
            );
        }
        text_at("#form-error", "");
        update_mode();
        if let Ok(dialog) = q("#settings") {
            let _ = call(dialog.as_ref(), "showModal", &[]);
        }
    }
}
async fn reconnect(camera_id: usize) {
    if let Err(e) = api(
        &format!("/api/cameras/{camera_id}/reconnect"),
        Some(json!({})),
    )
    .await
    {
        toast(&message(&e));
    }
    refresh().await;
}
fn create_card(camera: &Value) -> Result<Element, JsValue> {
    let template = q("#camera-template")?.dyn_into::<web_sys::HtmlTemplateElement>()?;
    let node = template
        .content()
        .first_element_child()
        .ok_or_else(|| error("Template kamera kosong."))?
        .clone_node_with_deep(true)?
        .dyn_into::<Element>()?;
    node.set_attribute("data-id", &id(camera).to_string())?;
    q("#grid")?.append_child(&node)?;
    let image = child(&node, ".video")?;
    let copy = image.clone();
    bind(image.as_ref(), "error", move |_| {
        let _ = copy.remove_attribute("src");
    })?;
    UI.with(|ui| {
        ui.borrow_mut().cards.insert(id(camera), node.clone());
    });
    Ok(node)
}
fn render(camera: &Value) -> Result<(), JsValue> {
    let card = UI
        .with(|ui| ui.borrow().cards.get(&id(camera)).cloned())
        .map(Ok)
        .unwrap_or_else(|| create_card(camera))?;
    let state = field(camera, "state");
    let live = state == "live";
    card.set_attribute("data-state", state)?;
    text(&child(&card, "h2")?, field(camera, "name"));
    let mode = field(camera, "mode");
    text(
        &child(&card, ".camera-address")?,
        &if mode == "cloud" {
            "Internet · Cloud V380".into()
        } else {
            format!("{} · {}", field(camera, "ip"), mode.to_uppercase())
        },
    );
    text(
        &child(&card, ".device-id")?,
        &format!("DEVICE ID {}", field(camera, "deviceId")),
    );
    let resolution = match (camera["width"].as_u64(), camera["height"].as_u64()) {
        (Some(w), Some(h)) if w > 0 && h > 0 => format!("{w} × {h}"),
        _ => "—".into(),
    };
    text(&child(&card, ".resolution")?, &resolution);
    text(
        &child(&card, ".status-text")?,
        if live {
            if camera["previewError"].is_null() {
                "Terhubung"
            } else {
                "Terhubung · tampilan bermasalah"
            }
        } else if state == "error" {
            "Koneksi bermasalah"
        } else {
            "Menghubungkan…"
        },
    );
    let image = child(&card, ".video")?;
    let playback = UI.with(|ui| ui.borrow().playback);
    if !playback && !image.has_attribute("src") {
        image.set_attribute("src", &format!("/stream/{}", id(camera)))?;
    }
    hidden(&image, !live || playback);
    for selector in [
        ".video-overlay",
        ".retry",
        ".live-badge",
        ".video-quality",
        ".ptz",
        ".talk",
        ".sound",
    ] {
        let hide = match selector {
            ".video-overlay" => live,
            ".retry" => state != "error",
            ".talk" | ".sound" => !live || camera["audio"] != true,
            _ => !live,
        };
        hidden(&child(&card, selector)?, hide);
    }
    text(
        &child(&card, ".video-overlay h3")?,
        if state == "error" {
            "Kamera belum menampilkan video"
        } else {
            "Menghubungkan kamera"
        },
    );
    text(&child(&card, ".video-overlay p")?, field(camera, "message"));
    text(
        &child(&card, ".video-quality")?,
        &field(camera, "quality").to_uppercase(),
    );
    set(child(&card, ".fullscreen")?.as_ref(), "disabled", !live);
    child(&card, ".snapshot")?
        .set_attribute("aria-disabled", if live { "false" } else { "true" })?;
    Ok(())
}
async fn refresh() {
    if UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if ui.loading {
            true
        } else {
            ui.loading = true;
            false
        }
    }) {
        return;
    }
    match api("/api/cameras", None).await {
        Ok(value) => {
            let cameras = value.as_array().cloned().unwrap_or_default();
            UI.with(|ui| ui.borrow_mut().cameras = cameras.clone());
            for camera in &cameras {
                if let Err(e) = render(camera) {
                    web_sys::console::error_1(&e)
                }
            }
            text_at("#total", &cameras.len().to_string());
            text_at("#camera-count", &cameras.len().to_string());
            text_at(
                "#live",
                &cameras
                    .iter()
                    .filter(|c| field(c, "state") == "live")
                    .count()
                    .to_string(),
            );
            let cloud = cameras
                .iter()
                .filter(|c| field(c, "mode") == "cloud")
                .count();
            text_at(
                "#connection-mode",
                if cloud == 0 {
                    "Jaringan rumah"
                } else if cloud == cameras.len() {
                    "Cloud V380"
                } else {
                    "LAN + Cloud"
                },
            );
            text_at(
                "#connection-note",
                if cloud > 0 {
                    "Kamera cloud terhubung melalui relay V380"
                } else {
                    "Video melalui jaringan rumah"
                },
            );
            text_at(
                "#updated",
                &format!(
                    "Diperbarui {}",
                    locale(js_sys::Date::new_0().as_ref(), "toLocaleTimeString")
                ),
            );
            hidden_at("#global-error", true);
            fill_cameras();
        }
        Err(_) => {
            text_at(
                "#global-error",
                "Server viewer tidak terjangkau. Menunggu koneksi kembali…",
            );
            hidden_at("#global-error", false);
            UI.with(|ui| {
                for card in ui.borrow().cards.values() {
                    let _ = card.set_attribute("data-state", "error");
                    if let Ok(image) = child(card, ".video") {
                        hidden(&image, true);
                        let _ = image.remove_attribute("src");
                    }
                    if let Ok(overlay) = child(card, ".video-overlay") {
                        hidden(&overlay, false);
                    }
                    if let Ok(message) = child(card, ".video-overlay p") {
                        text(&message, "Server terputus. Menunggu koneksi kembali…");
                    }
                }
            });
        }
    }
    UI.with(|ui| ui.borrow_mut().loading = false);
}
fn fill_cameras() {
    if let Ok(select) = q("#pb-camera") {
        let current = get(select.as_ref(), "value")
            .as_string()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "all".into());
        let cameras = UI.with(|ui| ui.borrow().cameras.clone());
        select.set_text_content(None);
        let add = |v: &str, label: &str| {
            if let Ok(option) = document().create_element("option") {
                set(option.as_ref(), "value", v);
                text(&option, label);
                let _ = select.append_child(&option);
            }
        };
        add("all", "Semua kamera");
        for camera in cameras {
            add(&id(&camera).to_string(), field(&camera, "name"));
        }
        set(select.as_ref(), "value", current);
    }
}
fn route() {
    let playback = window().location().hash().unwrap_or_default() == "#playback";
    UI.with(|ui| ui.borrow_mut().playback = playback);
    hidden_at("#live-view", playback);
    hidden_at("#playback-view", !playback);
    text_at("#crumb", if playback { "Rekaman" } else { "Live view" });
    if let Ok(nodes) = document().query_selector_all("[data-view]") {
        for i in 0..nodes.length() {
            if let Some(node) = nodes.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                let active = node.get_attribute("data-view").as_deref()
                    == Some(if playback { "playback" } else { "live" });
                let _ = node.class_list().toggle_with_force("active", active);
            }
        }
    }
    if playback {
        stop_sound();
        stop_talk();
        UI.with(|ui| {
            for card in ui.borrow().cards.values() {
                if let Ok(image) = child(card, ".video") {
                    let _ = image.remove_attribute("src");
                }
            }
        });
        spawn_local(load(false));
    } else {
        if let Ok(video) = q("#pb-video") {
            let _ = call(video.as_ref(), "pause", &[]);
        }
        spawn_local(refresh());
    }
}
async fn load(keep_page: bool) {
    fill_cameras();
    if !keep_page {
        UI.with(|ui| ui.borrow_mut().page = 1);
    }
    text_at("#pb-count", "Memuat…");
    if let Ok(list) = q("#pb-list") {
        list.set_text_content(None);
    }
    let params = url_query(&[
        ("camera", value("#pb-camera")),
        ("date", value("#pb-date")),
        ("from", value("#pb-from")),
        ("to", value("#pb-to")),
        (
            "motion",
            if checked("#pb-motion") { "1" } else { "0" }.into(),
        ),
        ("page", UI.with(|ui| ui.borrow().page.to_string())),
        ("size", "50".into()),
    ]);
    match api(&format!("/api/recordings?{params}"), None).await {
        Ok(data) => {
            let clips = data["items"].as_array().cloned().unwrap_or_default();
            UI.with(|ui| {
                let mut ui = ui.borrow_mut();
                ui.clips = clips.clone();
                ui.current = None;
                ui.page = data["page"].as_u64().unwrap_or(1) as usize;
                ui.pages = data["pages"].as_u64().unwrap_or(1) as usize;
            });
            text_at(
                "#pb-count",
                &if data["enabled"] == true {
                    format!("{} rekaman", data["total"].as_u64().unwrap_or(0))
                } else {
                    "Perekaman belum diaktifkan di config.json".into()
                },
            );
            let (page, pages) = UI.with(|ui| {
                let ui = ui.borrow();
                (ui.page, ui.pages)
            });
            text_at("#pb-page", &format!("Halaman {page} / {pages}"));
            disabled_at("#pb-prev", page <= 1);
            disabled_at("#pb-next", page >= pages);
            if let Ok(list) = q("#pb-list") {
                for (i, clip) in clips.iter().enumerate() {
                    if let Ok(li) = document().create_element("li") {
                        if let Ok(button) = document().create_element("button") {
                            let _ = button.set_attribute("data-index", &i.to_string());
                            if clip["moving"] == true {
                                button.set_class_name("moving");
                            }
                            if clip["motion"].is_number()
                                && let Ok(img) = document().create_element("img")
                            {
                                img.set_class_name("pb-thumb");
                                let _ = img.set_attribute("loading", "lazy");
                                let _ = img.set_attribute("alt", "");
                                let _ = img.set_attribute(
                                    "src",
                                    &format!("/thumbs/{}", field(clip, "name")),
                                );
                                let _ = button.append_child(&img);
                            } else if let Ok(blank) = document().create_element("div") {
                                blank.set_class_name("pb-thumb pb-thumb-empty");
                                let _ = button.append_child(&blank);
                            }
                            let date = js_sys::Date::new(&JsValue::from_str(field(clip, "start")));
                            let size = clip["size"].as_f64().unwrap_or(0.0);
                            let size = if size >= 1048576.0 {
                                format!("{:.1} MB", size / 1048576.0)
                            } else {
                                format!("{:.0} KB", (size / 1024.0).max(1.0))
                            };
                            if let Ok(meta) = document().create_element("div") {
                                meta.set_class_name("pb-meta");
                                for (tag, label) in [
                                    ("b", locale(date.as_ref(), "toLocaleTimeString")),
                                    ("span", field(clip, "camera").to_owned()),
                                ] {
                                    if let Ok(e) = document().create_element(tag) {
                                        text(&e, &label);
                                        let _ = meta.append_child(&e);
                                    }
                                }
                                if clip["moving"] == true
                                    && let Ok(tag) = document().create_element("em")
                                {
                                    text(&tag, "● gerakan");
                                    let _ = meta.append_child(&tag);
                                }
                                let _ = button.append_child(&meta);
                            }
                            if let Ok(e) = document().create_element("small") {
                                text(&e, &size);
                                let _ = button.append_child(&e);
                            }
                            let _ = li.append_child(&button);
                        }
                        let _ = list.append_child(&li);
                    }
                }
                if clips.is_empty()
                    && let Ok(li) = document().create_element("li")
                {
                    li.set_class_name("pb-empty");
                    text(&li, "Tidak ada rekaman pada tanggal dan jam ini.");
                    let _ = list.append_child(&li);
                }
            }
        }
        Err(e) => text_at("#pb-count", &message(&e)),
    }
}
fn url_query(values: &[(&str, String)]) -> String {
    values
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                js_sys::encode_uri_component(k),
                js_sys::encode_uri_component(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}
fn play(index: usize) {
    let clip = UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        let clip = ui.clips.get(index).cloned();
        if clip.is_some() {
            ui.current = Some(index);
        }
        clip
    });
    if let Some(clip) = clip {
        if let Ok(video) = q("#pb-video") {
            set(
                video.as_ref(),
                "src",
                format!(
                    "/recordings/{}",
                    js_sys::encode_uri_component(field(&clip, "name"))
                ),
            );
            if let Ok(p) = call(video.as_ref(), "play", &[]) {
                spawn_local(async move {
                    let _ = promise(p).await;
                });
            }
        }
        text_at(
            "#pb-now",
            &format!(
                "{} · {}",
                field(&clip, "camera"),
                locale(
                    js_sys::Date::new(&JsValue::from_str(field(&clip, "start"))).as_ref(),
                    "toLocaleString"
                )
            ),
        );
        if let Ok(list) = q("#pb-list") {
            let children = list.children();
            for i in 0..children.length() {
                if let Some(li) = children.item(i) {
                    let _ = li
                        .class_list()
                        .toggle_with_force("active", i as usize == index);
                }
            }
        }
    }
}
fn target(event: &Event) -> Option<Element> {
    event.target()?.dyn_into::<Element>().ok()
}
fn camera_target(event: &Event) -> Option<(usize, Element, Element)> {
    let target = target(event)?;
    let button = target.closest("button,a").ok()??;
    let card = button.closest("article.camera").ok()??;
    let id = card.get_attribute("data-id")?.parse().ok()?;
    Some((id, button, card))
}
async fn download_snapshot(camera_id: usize) -> Result<(), JsValue> {
    let response = response(&format!("/snapshot/{camera_id}"), None).await?;
    if !response.ok() {
        return Err(error("Snapshot belum tersedia."));
    }
    let blob = JsFuture::from(response.blob()?)
        .await?
        .dyn_into::<web_sys::Blob>()?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let anchor = document().create_element("a")?;
    set(anchor.as_ref(), "href", url.clone());
    set(anchor.as_ref(), "download", format!("v380-{camera_id}.jpg"));
    document().body().unwrap().append_child(&anchor)?;
    call(anchor.as_ref(), "click", &[])?;
    anchor.remove();
    later(1000, move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    Ok(())
}

fn new_audio(sample_rate: Option<f64>) -> Result<JsValue, JsValue> {
    let constructor = get(window().as_ref(), "AudioContext").dyn_into::<Function>()?;
    let args = Array::new();
    if let Some(sample_rate) = sample_rate {
        let options = Object::new();
        set(options.as_ref(), "sampleRate", sample_rate);
        args.push(&options);
    }
    Reflect::construct(&constructor, &args)
}
pub fn alaw_decode(byte: u8) -> f32 {
    let a = byte ^ 0x55;
    let segment = (a & 0x70) >> 4;
    let mut t = ((a & 15) as i32) << 4;
    t = if segment == 0 {
        t + 8
    } else {
        (t + 0x108) << segment.saturating_sub(1)
    };
    if a & 0x80 != 0 {
        t as f32 / 32768.0
    } else {
        -(t as f32) / 32768.0
    }
}
pub fn alaw_encode(sample: f32) -> u8 {
    let mut pcm = (sample * 32767.0).round().clamp(-32768.0, 32767.0) as i32;
    let mask = if pcm < 0 {
        pcm = -pcm - 1;
        0x55
    } else {
        0xd5
    };
    pcm = pcm.min(0x7fff);
    let mut segment = 0u8;
    let mut v = pcm >> 8;
    while v != 0 {
        segment += 1;
        v >>= 1;
    }
    if segment >= 8 {
        return 0x7f ^ mask;
    }
    ((segment << 4)
        | if segment < 2 {
            ((pcm >> 4) & 15) as u8
        } else {
            ((pcm >> (segment + 3)) & 15) as u8
        })
        ^ mask
}
fn stop_sound() {
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.sound_generation += 1;
        ui.sound_id = None;
        if let Some(abort) = ui.audio_abort.take() {
            abort.abort();
        }
    });
    if let Ok(buttons) = document().query_selector_all(".sound") {
        for i in 0..buttons.length() {
            if let Some(button) = buttons.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                text(&button, "🔇");
                let _ = button.set_attribute("aria-pressed", "false");
            }
        }
    }
}
async fn play_sound(camera_id: usize, button: Element) -> Result<(), JsValue> {
    stop_sound();
    let existing = UI.with(|ui| ui.borrow().audio_context.clone());
    let context = match existing {
        Some(context) => context,
        None => {
            let context = new_audio(Some(8000.0))?;
            UI.with(|ui| ui.borrow_mut().audio_context = Some(context.clone()));
            context
        }
    };
    promise(call(&context, "resume", &[])?).await?;
    let abort = web_sys::AbortController::new()?;
    let generation = UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.sound_id = Some(camera_id);
        ui.audio_abort = Some(abort.clone());
        ui.audio_next = 0.0;
        ui.sound_generation
    });
    text(&button, "🔊");
    button.set_attribute("aria-pressed", "true")?;
    let result = async {
        let init = RequestInit::new();
        init.set_signal(Some(&abort.signal()));
        let response =
            JsFuture::from(window().fetch_with_str_and_init(&format!("/audio/{camera_id}"), &init))
                .await?;
        if get(&response, "ok") != JsValue::TRUE {
            return Err(error("Audio belum tersedia."));
        }
        let reader = call(&get(&response, "body"), "getReader", &[])?;
        loop {
            let chunk = promise(call(&reader, "read", &[])?).await?;
            if get(&chunk, "done") == JsValue::TRUE {
                break;
            }
            if UI.with(|ui| ui.borrow().sound_generation != generation) {
                break;
            }
            let input = Uint8Array::new(&get(&chunk, "value")).to_vec();
            let samples: Vec<f32> = input.into_iter().map(alaw_decode).collect();
            if samples.is_empty() {
                continue;
            }
            let buffer = call(
                &context,
                "createBuffer",
                &[1.into(), (samples.len() as u32).into(), 8000.into()],
            )?;
            let floats = Float32Array::from(samples.as_slice());
            call(&buffer, "copyToChannel", &[floats.into(), 0.into()])?;
            let source = call(&context, "createBufferSource", &[])?;
            set(&source, "buffer", buffer.clone());
            call(&source, "connect", &[get(&context, "destination")])?;
            let now = get(&context, "currentTime").as_f64().unwrap_or(0.0);
            let next = UI.with(|ui| {
                let mut ui = ui.borrow_mut();
                if ui.audio_next < now + 0.02 {
                    ui.audio_next = now + 0.15;
                }
                if ui.audio_next > now + 1.0 {
                    return None;
                }
                let start = ui.audio_next;
                ui.audio_next += get(&buffer, "duration").as_f64().unwrap_or(0.0);
                Some(start)
            });
            if let Some(next) = next {
                call(&source, "start", &[next.into()])?;
            }
        }
        Ok::<_, JsValue>(())
    }
    .await;
    if UI.with(|ui| ui.borrow().sound_generation == generation) {
        stop_sound();
    }
    if let Err(e) = result
        && get(&e, "name").as_string().as_deref() != Some("AbortError")
    {
        return Err(e);
    }
    Ok(())
}
fn stop_talk() {
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.talk_held = false;
        if let Some(node) = ui.talk_node.take() {
            set(&node, "onaudioprocess", JsValue::NULL);
            let _ = call(&node, "disconnect", &[]);
        }
        if let Some(source) = ui.talk_source.take() {
            let _ = call(&source, "disconnect", &[]);
        }
        if let Some(stream) = ui.talk_stream.take()
            && let Ok(tracks) = call(&stream, "getTracks", &[])
        {
            for track in Array::from(&tracks).iter() {
                let _ = call(&track, "stop", &[]);
            }
        }
        ui.talk_pending.clear();
        ui.talk_callback = None;
    });
    if let Ok(buttons) = document().query_selector_all(".talk") {
        for i in 0..buttons.length() {
            if let Some(button) = buttons.item(i).and_then(|n| n.dyn_into::<Element>().ok()) {
                let _ = button.class_list().remove_1("active");
            }
        }
    }
}
async fn post_audio(camera_id: usize, data: Vec<u8>) {
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&Uint8Array::from(data.as_slice()).into());
    if let Ok(headers) = web_sys::Headers::new() {
        let _ = headers.set("Content-Type", "application/octet-stream");
        init.set_headers(headers.as_ref());
    }
    let _ = JsFuture::from(
        window().fetch_with_str_and_init(&format!("/api/cameras/{camera_id}/talk"), &init),
    )
    .await;
}
async fn start_talk(camera_id: usize, button: Element) -> Result<(), JsValue> {
    let media = get(window().navigator().as_ref(), "mediaDevices");
    let constraints = Object::new();
    let audio = Object::new();
    set(audio.as_ref(), "echoCancellation", true);
    set(audio.as_ref(), "noiseSuppression", true);
    set(audio.as_ref(), "channelCount", 1);
    set(constraints.as_ref(), "audio", audio);
    let stream = promise(call(&media, "getUserMedia", &[constraints.into()])?).await?;
    if !UI.with(|ui| ui.borrow().talk_held) {
        if let Ok(tracks) = call(&stream, "getTracks", &[]) {
            for track in Array::from(&tracks).iter() {
                let _ = call(&track, "stop", &[]);
            }
        }
        return Ok(());
    }
    let context = match UI.with(|ui| ui.borrow().talk_context.clone()) {
        Some(c) => c,
        None => {
            let context = new_audio(None)?;
            UI.with(|ui| ui.borrow_mut().talk_context = Some(context.clone()));
            context
        }
    };
    promise(call(&context, "resume", &[])?).await?;
    let ratio = get(&context, "sampleRate").as_f64().unwrap_or(48000.0) / 8000.0;
    let source = call(
        &context,
        "createMediaStreamSource",
        std::slice::from_ref(&stream),
    )?;
    let node = call(
        &context,
        "createScriptProcessor",
        &[2048.into(), 1.into(), 1.into()],
    )?;
    let callback = Closure::wrap(Box::new(move |event: JsValue| {
        let samples = call(&get(&event, "inputBuffer"), "getChannelData", &[0.into()])
            .ok()
            .map(|v| Float32Array::new(&v).to_vec())
            .unwrap_or_default();
        let chunks = UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            if !ui.talk_held {
                return vec![];
            }
            while ui.talk_position < (samples.len() as f64) {
                let start = ui.talk_position.floor() as usize;
                let end = ((ui.talk_position + ratio).floor() as usize)
                    .max(start + 1)
                    .min(samples.len());
                let sample = samples[start..end].iter().sum::<f32>() / (end - start) as f32;
                ui.talk_pending.push(alaw_encode(sample));
                ui.talk_position += ratio;
            }
            ui.talk_position -= samples.len() as f64;
            let mut chunks = vec![];
            while ui.talk_pending.len() >= 480 {
                chunks.push(ui.talk_pending.drain(..480).collect::<Vec<_>>());
            }
            chunks
        });
        for chunk in chunks {
            spawn_local(post_audio(camera_id, chunk));
        }
    }) as Box<dyn FnMut(JsValue)>);
    set(&node, "onaudioprocess", callback.as_ref().clone());
    call(&source, "connect", std::slice::from_ref(&node))?;
    call(&node, "connect", &[get(&context, "destination")])?;
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.talk_stream = Some(stream);
        ui.talk_node = Some(node);
        ui.talk_source = Some(source);
        ui.talk_position = 0.0;
        ui.talk_callback = Some(callback);
    });
    button.class_list().add_1("active")?;
    Ok(())
}
fn send_ptz(camera_id: usize, direction: String) {
    spawn_local(async move {
        if let Err(e) = api(
            &format!("/api/cameras/{camera_id}/ptz"),
            Some(json!({"direction":direction})),
        )
        .await
        {
            toast(&message(&e));
        }
    });
}
fn release_ptz() {
    let id = UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        if let Some(timer) = ui.ptz_timer.take() {
            window().clear_interval_with_handle(timer);
        }
        ui.ptz_callback = None;
        ui.ptz_id.take()
    });
    if let Some(id) = id {
        send_ptz(id, "stop".into());
    }
}
fn begin_ptz(camera_id: usize, direction: String) {
    release_ptz();
    send_ptz(camera_id, direction.clone());
    let callback =
        Closure::wrap(Box::new(move || send_ptz(camera_id, direction.clone())) as Box<dyn FnMut()>);
    if let Ok(timer) = window().set_interval_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        700,
    ) {
        UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            ui.ptz_id = Some(camera_id);
            ui.ptz_timer = Some(timer);
            ui.ptz_callback = Some(callback);
        });
    }
}
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    spawn_local(initialize_session());
    bind(document().as_ref(), "visibilitychange", |_| check_session())?;
    bind(window().as_ref(), "focus", |_| check_session())?;
    interval(1000, check_session)?;
    let date = js_sys::Date::new_0();
    value_at(
        "#pb-date",
        &format!(
            "{:04}-{:02}-{:02}",
            date.get_full_year(),
            date.get_month() + 1,
            date.get_date()
        ),
    );
    bind(q("#grid")?.as_ref(), "click", move |event| {
        if let Some((camera_id, button, _)) = camera_target(&event) {
            let classes = button.class_list();
            if classes.contains("settings") {
                open_settings(camera_id);
            } else if classes.contains("retry") {
                spawn_local(reconnect(camera_id));
            } else if classes.contains("fullscreen") {
                if let Some(card) = button.closest("article.camera").ok().flatten()
                    && let Ok(stage) = child(&card, ".video-stage")
                    && let Ok(p) = call(stage.as_ref(), "requestFullscreen", &[])
                {
                    spawn_local(async move {
                        let _ = promise(p).await;
                    });
                }
            } else if classes.contains("snapshot") {
                event.prevent_default();
                spawn_local(async move {
                    if let Err(e) = download_snapshot(camera_id).await {
                        toast(&message(&e));
                    }
                });
            } else if classes.contains("sound") {
                let same = UI.with(|ui| ui.borrow().sound_id == Some(camera_id));
                stop_sound();
                if !same {
                    spawn_local(async move {
                        if let Err(e) = play_sound(camera_id, button).await {
                            toast(&message(&e));
                        }
                    });
                }
            }
        }
    })?;
    bind(q("#grid")?.as_ref(), "pointerdown", move |event| {
        if let Some((camera_id, button, _)) = camera_target(&event) {
            if let Ok(pointer) = event.clone().dyn_into::<web_sys::PointerEvent>() {
                let _ = call(
                    button.as_ref(),
                    "setPointerCapture",
                    &[pointer.pointer_id().into()],
                );
            }
            if button.class_list().contains("talk") {
                event.prevent_default();
                stop_talk();
                UI.with(|ui| ui.borrow_mut().talk_held = true);
                spawn_local(async move {
                    if start_talk(camera_id, button).await.is_err() {
                        stop_talk();
                        toast("Mikrofon tidak dapat diakses. Izinkan mikrofon di browser.");
                    }
                });
            } else if let Some(direction) = button.get_attribute("data-dir") {
                event.prevent_default();
                begin_ptz(camera_id, direction);
            }
        }
    })?;
    for event in ["pointerup", "pointercancel"] {
        bind(document().as_ref(), event, move |_| {
            release_ptz();
            stop_talk();
        })?;
    }
    bind(q("#grid")?.as_ref(), "keydown", move |event| {
        if let Some((camera_id, button, _)) = camera_target(&event)
            && let Some(direction) = button.get_attribute("data-dir")
            && let Ok(key) = event.clone().dyn_into::<web_sys::KeyboardEvent>()
            && (key.key() == "Enter" || key.key() == " ")
            && !key.repeat()
        {
            event.prevent_default();
            begin_ptz(camera_id, direction);
        }
    })?;
    bind(q("#grid")?.as_ref(), "keyup", move |_| release_ptz())?;
    bind(q("#settings-form")?.as_ref(), "submit", move |event| {
        event.prevent_default();
        disabled_at("#settings-form [type=submit]", true);
        spawn_local(async move {
            let camera_id = value("[name=id]");
            let mut data = json!({});
            for field in [
                "name", "ip", "deviceId", "username", "password", "mode", "quality", "v380Port",
                "rtspUrl",
            ] {
                data[field] = json!(value(&format!("[name={field}]")));
            }
            match api(&format!("/api/cameras/{camera_id}"), Some(data)).await {
                Ok(_) => {
                    if let Ok(dialog) = q("#settings") {
                        let _ = call(dialog.as_ref(), "close", &[]);
                    }
                    toast("Konfigurasi disimpan. Menghubungkan kamera…");
                    refresh().await;
                }
                Err(e) => text_at("#form-error", &message(&e)),
            }
            disabled_at("#settings-form [type=submit]", false);
        });
    })?;
    bind(q("[name=mode]")?.as_ref(), "change", move |_| update_mode())?;
    for selector in ["#close-settings", "#cancel-settings"] {
        bind(q(selector)?.as_ref(), "click", move |_| {
            if let Ok(dialog) = q("#settings") {
                let _ = call(dialog.as_ref(), "close", &[]);
            }
        })?;
    }
    bind(q("#refresh")?.as_ref(), "click", move |_| {
        spawn_local(async {
            disabled_at("#refresh", true);
            let ids = UI.with(|ui| ui.borrow().cameras.iter().map(id).collect::<Vec<_>>());
            for id in ids {
                let _ = api(&format!("/api/cameras/{id}/reconnect"), Some(json!({}))).await;
            }
            refresh().await;
            disabled_at("#refresh", false);
        })
    })?;
    for selector in ["#pb-camera", "#pb-date", "#pb-from", "#pb-to", "#pb-motion"] {
        bind(q(selector)?.as_ref(), "change", move |_| {
            spawn_local(load(false))
        })?;
    }
    bind(q("#pb-reload")?.as_ref(), "click", move |_| {
        spawn_local(load(true))
    })?;
    bind(q("#pb-prev")?.as_ref(), "click", move |_| {
        UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            ui.page = ui.page.saturating_sub(1).max(1);
        });
        spawn_local(load(true));
    })?;
    bind(q("#pb-next")?.as_ref(), "click", move |_| {
        UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            ui.page = (ui.page + 1).min(ui.pages);
        });
        spawn_local(load(true));
    })?;
    bind(q("#pb-list")?.as_ref(), "click", move |event| {
        if let Some(button) =
            target(&event).and_then(|e| e.closest("button[data-index]").ok().flatten())
            && let Some(index) = button
                .get_attribute("data-index")
                .and_then(|n| n.parse().ok())
        {
            play(index);
        }
    })?;
    bind(q("#pb-video")?.as_ref(), "ended", move |_| {
        if let Some(next) = UI.with(|ui| ui.borrow().current).filter(|n| *n > 0) {
            play(next - 1);
        }
    })?;
    bind(q("#pb-video")?.as_ref(), "error", move |_| {
        text_at(
            "#pb-now",
            "Rekaman tidak dapat diputar (belum selesai disalin atau terhapus).",
        )
    })?;
    bind(window().as_ref(), "hashchange", move |_| route())?;
    bind(window().as_ref(), "blur", move |_| {
        release_ptz();
        stop_talk();
    })?;
    interval(1000, || {
        text_at(
            "#clock",
            &locale(js_sys::Date::new_0().as_ref(), "toLocaleTimeString"),
        )
    })?;
    interval(2500, || spawn_local(refresh()))?;
    route();
    spawn_local(refresh());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn g711_silence_and_polarity() {
        assert!(alaw_decode(0xd5).abs() < 0.001);
        assert!(alaw_decode(0x55) < 0.0);
        for sample in [-0.9f32, -0.25, 0.0, 0.25, 0.9] {
            assert!((alaw_decode(alaw_encode(sample)) - sample).abs() < 0.04);
        }
    }
}
