use crate::storage::{Clip, parse_name};
use chrono::{Duration as Age, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Clip names and sizes for remote storage, persisted next to the recordings so the playback
/// page never has to list the remote (slow on Google Drive/OneDrive) while opening.
/// Grouped by the `YYYYMMDD` in the file name, which is the local recording date.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Meta {
    size: u64,
    /// Confidence of confirmed object movement.
    motion: Option<f32>,
    #[serde(default)]
    motion_version: u8,
}
/// Older index files stored only the size.
#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Size(u64),
    Full(Meta),
}
#[derive(Default, Serialize, Deserialize)]
struct Data<T> {
    clips: BTreeMap<String, T>,
}
#[derive(Default)]
struct Inner {
    days: BTreeMap<String, BTreeMap<String, Meta>>,
    ready: bool,
    dirty: bool,
}
pub struct Index {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
    persist_lock: tokio::sync::Mutex<()>,
}
fn day_of(name: &str) -> Option<String> {
    parse_name(name, 0)?;
    name.split('_').nth(1).map(str::to_owned)
}
impl Index {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut inner = Inner::default();
        if let Some(data) = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Data<Entry>>(&b).ok())
        {
            for (name, entry) in data.clips {
                let meta = match entry {
                    Entry::Size(size) => Meta { size, motion: None, motion_version: crate::motion::VERSION },
                    Entry::Full(mut meta) => {
                        if meta.motion_version != crate::motion::VERSION {
                            meta.motion = None;
                            meta.motion_version = crate::motion::VERSION;
                            inner.dirty = true;
                        }
                        meta
                    },
                };
                if let Some(day) = day_of(&name) {
                    inner.days.entry(day).or_default().insert(name, meta);
                }
            }
            inner.ready = true;
        }
        Self {
            path,
            inner: Mutex::new(inner),
            persist_lock: tokio::sync::Mutex::new(()),
        }
    }
    /// True once the index came from disk or a full sync, so it can be trusted for listing.
    pub fn ready(&self) -> bool {
        self.inner.lock().unwrap().ready
    }
    pub fn add(&self, name: &str, size: u64) {
        if let Some(day) = day_of(name) {
            let mut inner = self.inner.lock().unwrap();
            inner
                .days
                .entry(day)
                .or_default()
                .entry(name.into())
                .and_modify(|m| m.size = size)
                .or_insert(Meta { size, motion: None, motion_version: crate::motion::VERSION });
            inner.dirty = true;
        }
    }
    pub fn set_motion(&self, name: &str, motion: f32) {
        if let Some(day) = day_of(name) {
            let mut inner = self.inner.lock().unwrap();
            if let Some(meta) = inner.days.get_mut(&day).and_then(|b| b.get_mut(name)) {
                meta.motion = Some(motion);
                meta.motion_version = crate::motion::VERSION;
                inner.dirty = true;
            }
        }
    }
    pub fn motion(&self, name: &str) -> Option<f32> {
        let day = day_of(name)?;
        self.inner.lock().unwrap().days.get(&day)?.get(name)?.motion
    }
    pub fn remove(&self, name: &str) {
        if let Some(day) = day_of(name) {
            let mut inner = self.inner.lock().unwrap();
            if let Some(bucket) = inner.days.get_mut(&day)
                && bucket.remove(name).is_some()
            {
                inner.dirty = true;
            }
        }
    }
    pub fn size(&self, name: &str) -> Option<u64> {
        let day = day_of(name)?;
        self.inner.lock().unwrap().days.get(&day)?.get(name).map(|m| m.size)
    }
    /// `day` is `YYYYMMDD`.
    pub fn day(&self, day: &str) -> Vec<Clip> {
        let inner = self.inner.lock().unwrap();
        inner
            .days
            .get(day)
            .map(|bucket| {
                bucket
                    .iter()
                    .filter_map(|(name, meta)| {
                        let mut clip = parse_name(name, meta.size)?;
                        clip.set_motion(meta.motion);
                        Some(clip)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Merge a complete listing: add what is new and drop what vanished, except clips younger
    /// than ten minutes (they may not have reached the listing yet).
    pub fn sync(&self, listing: Vec<(String, u64)>) {
        let recent = Utc::now() - Age::minutes(10);
        let mut inner = self.inner.lock().unwrap();
        let mut fresh: BTreeMap<String, BTreeMap<String, Meta>> = BTreeMap::new();
        for (name, size) in listing {
            if let Some(day) = day_of(&name) {
                // Keep thumbnail/motion data across syncs.
                let motion = inner.days.get(&day).and_then(|b| b.get(&name)).and_then(|m| m.motion);
                fresh.entry(day).or_default().insert(name, Meta { size, motion, motion_version: crate::motion::VERSION });
            }
        }
        for (day, bucket) in &inner.days {
            for (name, meta) in bucket {
                if parse_name(name, 0).is_some_and(|c| c.start > recent) {
                    fresh
                        .entry(day.clone())
                        .or_default()
                        .entry(name.clone())
                        .or_insert(*meta);
                }
            }
        }
        if !same(&fresh, &inner.days) {
            inner.days = fresh;
            inner.dirty = true;
        }
        inner.ready = true;
    }
    pub async fn persist(&self) {
        let _guard = self.persist_lock.lock().await;
        let Some(path) = &self.path else { return };
        let bytes = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            let clips: BTreeMap<String, Meta> = inner
                .days
                .values()
                .flat_map(|b| b.iter().map(|(n, m)| (n.clone(), *m)))
                .collect();
            serde_json::to_vec(&Data { clips }).unwrap_or_default()
        };
        let temp = path.with_extension("json.tmp");
        if tokio::fs::write(&temp, bytes).await.is_err()
            || tokio::fs::rename(&temp, path).await.is_err()
        {
            self.inner.lock().unwrap().dirty = true;
        }
    }
}
fn same(a: &BTreeMap<String, BTreeMap<String, Meta>>, b: &BTreeMap<String, BTreeMap<String, Meta>>) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((da, ba), (db, bb))| {
            da == db
                && ba.len() == bb.len()
                && ba
                    .iter()
                    .zip(bb)
                    .all(|((na, ma), (nb, mb))| na == nb && ma.size == mb.size && ma.motion == mb.motion)
        })
}
pub fn index_path(dir: &Path) -> PathBuf {
    dir.join(".index.json")
}
