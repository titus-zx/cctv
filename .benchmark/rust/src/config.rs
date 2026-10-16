use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    #[serde(default = "camera_name")]
    pub name: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default, deserialize_with = "string_or_number")]
    pub device_id: String,
    #[serde(default = "admin")]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "v380")]
    pub mode: String,
    #[serde(default = "hd")]
    pub quality: String,
    #[serde(default = "port")]
    pub v380_port: u16,
    #[serde(default)]
    pub rtsp_url: Option<String>,
    #[serde(default = "yes")]
    pub record: bool,
    #[serde(default = "yes")]
    pub audio: bool,
    #[serde(default = "preview_fps")]
    pub preview_fps: u8,
    #[serde(default)]
    pub recording_fps: Option<f64>,
    #[serde(default = "crf")]
    pub recording_crf: u8,
    #[serde(default = "auto")]
    pub recording_encoder: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
fn camera_name() -> String {
    "Kamera V380".into()
}
fn admin() -> String {
    "admin".into()
}
fn v380() -> String {
    "v380".into()
}
fn hd() -> String {
    "hd".into()
}
fn auto() -> String {
    "auto".into()
}
fn port() -> u16 {
    8800
}
fn yes() -> bool {
    true
}
fn preview_fps() -> u8 {
    5
}
fn crf() -> u8 {
    22
}
fn string_or_number<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<String, D::Error> {
    let v = Value::deserialize(d)?;
    match v {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(serde::de::Error::custom("Device ID tidak valid")),
    }
}
impl Camera {
    pub fn validate(&self) -> Result<()> {
        if !["v380", "cloud", "rtsp"].contains(&self.mode.as_str()) {
            bail!("Mode tidak valid.")
        }
        if !["hd", "sd"].contains(&self.quality.as_str()) {
            bail!("Kualitas tidak valid.")
        }
        if self.mode != "cloud" && self.ip.parse::<std::net::IpAddr>().is_err() {
            bail!("Alamat IP tidak valid.")
        }
        if self.mode == "cloud"
            && !self.ip.is_empty()
            && self.ip.parse::<std::net::IpAddr>().is_err()
        {
            bail!("Alamat IP tidak valid.")
        }
        if self.device_id.len() > 10
            || self
                .device_id
                .parse::<u32>()
                .ok()
                .filter(|id| *id > 0)
                .is_none()
        {
            bail!("Device ID tidak valid.")
        }
        if self.username.len() > 31 || self.password.len() > 47 {
            bail!("Username/password terlalu panjang.")
        }
        if self.v380_port == 0 {
            bail!("Port tidak valid.")
        }
        if self.recording_crf > 51 || !(1..=10).contains(&self.preview_fps) {
            bail!("Kualitas rekaman/preview tidak valid.")
        }
        if self
            .recording_fps
            .is_some_and(|fps| !fps.is_finite() || !(1.0..=60.0).contains(&fps))
        {
            bail!("Frame rate rekaman tidak valid.")
        }
        if !["auto", "cpu"].contains(&self.recording_encoder.as_str()) {
            bail!("Encoder tidak valid.")
        }
        if self.mode == "rtsp"
            && self
                .rtsp_url
                .as_ref()
                .and_then(|s| url::Url::parse(s).ok())
                .is_none_or(|u| u.scheme() != "rtsp" || u.host_str().is_none())
        {
            bail!("URL harus diawali rtsp://.")
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "segment_seconds")]
    pub segment_seconds: u64,
    #[serde(default = "recordings_dir")]
    pub dir: PathBuf,
    #[serde(default)]
    pub keep_local: bool,
    #[serde(default = "destination")]
    pub destination: String,
    #[serde(default = "yes")]
    pub audio: bool,
    #[serde(default)]
    pub rclone: BTreeMap<String, String>,
    #[serde(default)]
    pub retention_days: f64,
    #[serde(default = "retention_seconds")]
    pub retention_check_seconds: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
fn segment_seconds() -> u64 {
    60
}
fn recordings_dir() -> PathBuf {
    "recordings".into()
}
fn destination() -> String {
    "gdrive".into()
}
fn retention_seconds() -> u64 {
    3600
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "http_port")]
    pub port: u16,
    pub cameras: Vec<Camera>,
    #[serde(default)]
    pub recording: Option<Recording>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
fn http_port() -> u16 {
    3380
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let cfg: Self = serde_json::from_slice(
            &std::fs::read(path).context("Konfigurasi tidak dapat dibaca")?,
        )?;
        for camera in &cfg.cameras {
            camera.validate()?;
        }
        if let Some(r) = &cfg.recording {
            if !["local", "gdrive", "onedrive"].contains(&r.destination.as_str())
                || r.segment_seconds == 0
                || r.segment_seconds > 86400
            {
                bail!("Konfigurasi rekaman tidak valid.")
            }
            if r.enabled
                && r.destination != "local"
                && r.rclone.get(&r.destination).is_none_or(|s| s.is_empty())
            {
                bail!("Remote rclone belum diisi.")
            }
        }
        Ok(cfg)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let temporary = path.with_extension("json.tmp");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionSettings {
    pub days: f64,
    pub check_seconds: u64,
}
impl RetentionSettings {
    pub fn new(r: Option<&Recording>) -> Result<Self> {
        let value = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let days = value("RECORDING_RETENTION_DAYS")
            .map(|s| s.parse::<f64>())
            .transpose()?
            .unwrap_or(r.map_or(0.0, |r| r.retention_days));
        let seconds = value("RECORDING_RETENTION_CHECK_SECONDS")
            .map(|s| s.parse::<u64>())
            .transpose()?
            .unwrap_or(r.map_or(3600, |r| r.retention_check_seconds));
        if !days.is_finite() || days < 0.0 || seconds == 0 || seconds > 2147483 {
            bail!("Konfigurasi retensi tidak valid.")
        }
        Ok(Self {
            days,
            check_seconds: seconds,
        })
    }
}
