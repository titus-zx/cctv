use crate::{
    index::{Index, index_path},
    config::{Recording, RetentionSettings},
    recording::finalized_mp4,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{process::Command, sync::Mutex};

#[derive(Clone)]
pub struct Store {
    pub dir: PathBuf,
    pub remote: Option<String>,
    pub bin: String,
    pub index: Arc<Index>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub name: String,
    pub device_id: String,
    pub start: DateTime<Utc>,
    pub size: u64,
    /// Confidence of recognized object movement (0 when no movement was confirmed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion: Option<f32>,
    pub moving: bool,
    pub motion_version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
}
pub fn parse_name(name: &str, size: u64) -> Option<Clip> {
    let name_body = name.strip_prefix("cam")?.strip_suffix(".mp4")?;
    let parts: Vec<_> = name_body.split('_').collect();
    if parts.len() != 3
        || parts[0].is_empty()
        || !parts[0].bytes().all(|b| b.is_ascii_digit())
        || parts[1].len() != 8
        || parts[2].len() != 6
        || !parts[1..]
            .iter()
            .all(|s| s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let timestamp =
        NaiveDateTime::parse_from_str(&format!("{}_{}", parts[1], parts[2]), "%Y%m%d_%H%M%S")
            .ok()?;
    let time = Local.from_local_datetime(&timestamp).single()?;
    Some(Clip {
        name: name.into(),
        device_id: parts[0].into(),
        start: time.with_timezone(&Utc),
        size,
        motion: None,
        moving: false,
        motion_version: crate::motion::VERSION,
        camera_id: None,
        camera: None,
    })
}
impl Clip {
    pub fn set_motion(&mut self, motion: Option<f32>) {
        self.motion = motion;
        self.moving = motion.is_some_and(|m| m >= MOTION_THRESHOLD);
    }
}
/// Positive scores are confidence values from the object detector.
pub const MOTION_THRESHOLD: f32 = 0.5;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteRange {
    Full,
    Part { start: u64, end: u64 },
    Unsatisfiable,
}
pub fn byte_range(header: Option<&str>, size: u64) -> ByteRange {
    let Some(range) = header.and_then(|s| s.strip_prefix("bytes=")) else {
        return ByteRange::Full;
    };
    let Some((a, b)) = range.split_once('-') else {
        return ByteRange::Full;
    };
    if a.is_empty() && b.is_empty()
        || !a.bytes().all(|b| b.is_ascii_digit())
        || !b.bytes().all(|b| b.is_ascii_digit())
    {
        return ByteRange::Full;
    }
    let (start, end) = if a.is_empty() {
        let Ok(suffix) = b.parse::<u64>() else {
            return ByteRange::Unsatisfiable;
        };
        if suffix == 0 {
            return ByteRange::Unsatisfiable;
        }
        (size.saturating_sub(suffix), size.saturating_sub(1))
    } else {
        let Ok(start) = a.parse::<u64>() else {
            return ByteRange::Unsatisfiable;
        };
        let end = if b.is_empty() {
            size.saturating_sub(1)
        } else {
            let Ok(end) = b.parse::<u64>() else {
                return ByteRange::Unsatisfiable;
            };
            end.min(size.saturating_sub(1))
        };
        (start, end)
    };
    if size == 0 || start >= size || start > end {
        ByteRange::Unsatisfiable
    } else {
        ByteRange::Part { start, end }
    }
}
impl Store {
    pub fn new(recording: &Recording) -> Self {
        Self {
            index: Arc::new(Index::new(Some(index_path(&recording.dir)))),
            dir: recording.dir.clone(),
            remote: if recording.destination == "local" {
                None
            } else {
                recording.rclone.get(&recording.destination).cloned()
            },
            bin: recording
                .rclone
                .get("bin")
                .cloned()
                .or_else(|| std::env::var("RCLONE_PATH").ok())
                .unwrap_or_else(|| "rclone".into()),
        }
    }
    pub fn target(&self, name: &str) -> Result<String> {
        if parse_name(name, 0).is_none() {
            bail!("Nama rekaman tidak valid.")
        }
        Ok(format!(
            "{}/{}",
            self.remote
                .as_ref()
                .context("Remote tidak diisi")?
                .trim_end_matches('/'),
            name
        ))
    }
    pub async fn rclone(&self, args: &[String]) -> Result<String> {
        let output = Command::new(&self.bin)
            .args(args)
            .kill_on_drop(true)
            .output()
            .await
            .context("rclone tidak dapat dijalankan")?;
        if !output.status.success() {
            bail!("rclone gagal ({}).", output.status.code().unwrap_or(-1))
        }
        Ok(String::from_utf8(output.stdout)?)
    }
    pub async fn remote_list(&self, day: Option<&str>) -> Result<Vec<Clip>> {
        let pattern = day
            .map(|day| format!("*_{day}_*.mp4"))
            .unwrap_or_else(|| "cam*_????????_??????.mp4".into());
        let result = self
            .rclone(&[
                "lsjson".into(),
                "--files-only".into(),
                "--no-mimetype".into(),
                "--no-modtime".into(),
                "--include".into(),
                pattern,
                self.remote.clone().context("Remote tidak diisi")?,
            ])
            .await?;
        let values: Vec<Value> = serde_json::from_str(&result)?;
        Ok(values
            .into_iter()
            .filter(|v| v["IsDir"] != true && v["Path"] == v["Name"])
            .filter_map(|v| parse_name(v["Name"].as_str()?, v["Size"].as_u64().unwrap_or(0)))
            .collect())
    }
    pub async fn list(&self, day: &str) -> Result<Vec<Clip>> {
        let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").context("Tanggal tidak valid.")?;
        if date.format("%Y-%m-%d").to_string() != day {
            bail!("Tanggal tidak valid.")
        }
        if self.remote.is_some() {
            let day = date.format("%Y%m%d").to_string();
            // Served from the metadata index; only the very first run (no index yet) lists live.
            if self.index.ready() {
                return Ok(self.index.day(&day));
            }
            return self.remote_list(Some(&day)).await;
        }
        let mut clips = vec![];
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(clips),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            if let Some(clip) = parse_name(
                &entry.file_name().to_string_lossy(),
                entry.metadata().await?.len(),
            ) && clip.start.with_timezone(&Local).date_naive() == date
            {
                let mut clip = clip;
                clip.set_motion(self.index.motion(&clip.name));
                clips.push(clip);
            }
        }
        Ok(clips)
    }
    /// A clip still on local disk is complete (segments are linked in only once finalized).
    pub async fn local_size(&self, name: &str) -> Option<u64> {
        parse_name(name, 0)?;
        tokio::fs::symlink_metadata(self.dir.join(name))
            .await
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
    }
    pub async fn size(&self, name: &str) -> Result<Option<u64>> {
        if parse_name(name, 0).is_none() {
            return Ok(None);
        }
        if self.remote.is_some() {
            if let Some(size) = self.local_size(name).await.or_else(|| self.index.size(name)) {
                return Ok(Some(size));
            }
        }
        if self.remote.is_none() {
            return match tokio::fs::symlink_metadata(self.dir.join(name)).await {
                Ok(m) if m.is_file() => Ok(Some(m.len())),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            };
        }
        let text = match self
            .rclone(&[
                "lsjson".into(),
                "--files-only".into(),
                "--no-mimetype".into(),
                "--no-modtime".into(),
                self.target(name)?,
            ])
            .await
        {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let files: Vec<Value> = serde_json::from_str(&text)?;
        Ok(files.first().and_then(|f| f["Size"].as_u64()))
    }
    /// Rebuild the index from the remote plus clips still waiting locally.
    pub async fn sync_index(&self) -> Result<()> {
        let mut listing: Vec<(String, u64)> = if self.remote.is_some() {
            self.remote_list(None)
                .await?
                .into_iter()
                .map(|c| (c.name, c.size))
                .collect()
        } else {
            vec![]
        };
        if let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Ok(meta) = entry.metadata().await
                    && meta.is_file()
                    && parse_name(&name, 0).is_some()
                {
                    listing.push((name, meta.len()));
                }
            }
        }
        self.index.sync(listing);
        self.index.persist().await;
        Ok(())
    }
    pub async fn upload(&self, file: &Path) -> Result<()> {
        if self.remote.is_none() {
            return Ok(());
        }
        let name = file
            .file_name()
            .context("File rekaman tidak valid")?
            .to_string_lossy();
        self.rclone(&[
            "copyto".into(),
            "--no-traverse".into(),
            "--".into(),
            file.to_string_lossy().into(),
            self.target(&name)?,
        ])
        .await?;
        Ok(())
    }
    pub async fn delete_remote(&self, name: &str) -> Result<()> {
        self.rclone(&["deletefile".into(), "--".into(), self.target(name)?])
            .await?;
        self.index.remove(name);
        Ok(())
    }
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStatus {
    pub uploaded: u64,
    pub pending: usize,
    pub analysis_pending: usize,
    pub analysis_skipped: u64,
    pub analysis_failed: u64,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct UploadQueue {
    pub queued: Arc<Mutex<HashSet<PathBuf>>>,
    pub status: Arc<Mutex<QueueStatus>>,
    sender: tokio::sync::mpsc::UnboundedSender<PathBuf>,
    index: Arc<Index>,
    analysis: crate::analysis::AnalysisQueue,
}
impl UploadQueue {
    pub fn new(
        store: Store,
        keep_local: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let index = store.index.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let status = Arc::new(Mutex::new(QueueStatus::default()));
        let analysis = crate::analysis::AnalysisQueue::new(store.clone(), status.clone(), cancel.clone());
        let set = queued.clone();
        let state = status.clone();
        tokio::spawn(async move {
            loop {
                let file = tokio::select! {_ = cancel.cancelled()=>break,file=receiver.recv()=>match file{Some(file)=>file,None=>break}};
                let mut attempts = 0;
                // Analysis owns a bounded hard link and proceeds independently of upload.
                loop {
                    if !file.exists() {
                        set.lock().await.remove(&file);
                        let count = set.lock().await.len();
                        state.lock().await.pending = count;
                        break;
                    }
                    let upload = tokio::select! {_ = cancel.cancelled()=>return,result=store.upload(&file)=>result};
                    match upload {
                        Ok(()) => {
                            let mut s = state.lock().await;
                            s.uploaded += 1;
                            s.error = None;
                            drop(s);
                            if !keep_local {
                                let _ = tokio::fs::remove_file(&file).await;
                            }
                            set.lock().await.remove(&file);
                            let count = set.lock().await.len();
                            state.lock().await.pending = count;
                            break;
                        }
                        Err(_) => {
                            state.lock().await.error = Some(
                                "Upload gagal; file lokal dipertahankan dan akan dicoba ulang."
                                    .into(),
                            );
                            attempts += 1;
                            let delay = std::time::Duration::from_secs(
                                (2u64.pow(attempts.min(5)) * 2).min(60),
                            );
                            tokio::select! {_ = cancel.cancelled()=>return,_=tokio::time::sleep(delay)=>{}}
                        }
                    }
                }
            }
        });
        Self {
            queued,
            status,
            sender,
            index,
            analysis,
        }
    }
    pub async fn add(&self, file: PathBuf) {
        if let (Some(name), Ok(meta)) = (
            file.file_name().map(|n| n.to_string_lossy().into_owned()),
            tokio::fs::metadata(&file).await,
        ) {
            self.index.add(&name, meta.len());
        }
        let mut queued = self.queued.lock().await;
        if queued.insert(file.clone()) {
            self.status.lock().await.pending = queued.len();
            self.analysis.submit(&file).await;
            let _ = self.sender.send(file);
        }
    }
    pub async fn recover(&self, dir: &Path) -> Result<()> {
        self.analysis.recover().await;
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_file()
                && parse_name(&entry.file_name().to_string_lossy(), 0).is_some()
                && finalized_mp4(&entry.path())
            {
                self.add(entry.path()).await
            }
        }
        Ok(())
    }
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionStatus {
    pub deleted_local: u64,
    pub deleted_remote: u64,
    pub last_run_at: Option<i64>,
    pub error: Option<String>,
}
pub async fn clean_retention(
    store: &Store,
    queue: &UploadQueue,
    ids: &HashSet<String>,
    days: f64,
    now: DateTime<Utc>,
    status: &mut RetentionStatus,
) -> Result<()> {
    let cutoff = now - chrono::Duration::milliseconds((days * 86400000.0) as i64);
    status.last_run_at = Some(now.timestamp_millis());
    let pending: HashSet<_> = queue
        .queued
        .lock()
        .await
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let expired = |name: &str| {
        parse_name(name, 0).is_some_and(|c| ids.contains(&c.device_id) && c.start < cutoff)
            && !pending.contains(name)
    };
    let uploaded = if store.remote.is_some() {
        let clips = store.remote_list(None).await?;
        let uploaded = clips.iter().map(|c| c.name.clone()).collect::<HashSet<_>>();
        for clip in clips {
            if expired(&clip.name) {
                store.delete_remote(&clip.name).await?;
                status.deleted_remote += 1;
            }
        }
        Some(uploaded)
    } else {
        None
    };
    let mut entries = match tokio::fs::read_dir(&store.dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().await?.is_file()
            && expired(&name)
            && uploaded.as_ref().is_none_or(|names| names.contains(&name))
            && finalized_mp4(&entry.path())
        {
            tokio::fs::remove_file(entry.path()).await?;
            store.index.remove(&name);
            status.deleted_local += 1;
        }
    }
    clean_thumbs(store, &expired, uploaded.as_ref()).await;
    status.error = None;
    Ok(())
}
/// Thumbnails follow their clip: removed when the clip expires, and when the clip is gone from
/// both local disk and the remote (orphans), plus abandoned temp files.
async fn clean_thumbs(
    store: &Store,
    expired: impl Fn(&str) -> bool,
    uploaded: Option<&HashSet<String>>,
) {
    let dir = crate::thumbs::thumb_dir(&store.dir);
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file = entry.file_name().to_string_lossy().into_owned();
        let old = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > crate::thumbs::GRACE);
        let remove = match crate::thumbs::clip_name(&file) {
            Some(name) => {
                expired(&name)
                    || (old
                        && !store.dir.join(&name).exists()
                        && uploaded.is_none_or(|u| !u.contains(&name)))
            }
            None => old,
        };
        if remove && entry.file_type().await.is_ok_and(|t| t.is_file()) {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}
pub fn retention_json(settings: RetentionSettings, status: &RetentionStatus) -> Value {
    let mut value = serde_json::to_value(status).unwrap();
    value["enabled"] = json!(settings.days > 0.0);
    value["days"] = json!(settings.days);
    value["checkSeconds"] = json!(settings.check_seconds);
    value
}
