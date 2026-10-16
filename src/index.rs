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
#[derive(Default, Serialize, Deserialize)]
struct Data {
    clips: BTreeMap<String, u64>,
}
#[derive(Default)]
struct Inner {
    days: BTreeMap<String, BTreeMap<String, u64>>,
    ready: bool,
    dirty: bool,
}
pub struct Index {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
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
            .and_then(|b| serde_json::from_slice::<Data>(&b).ok())
        {
            for (name, size) in data.clips {
                if let Some(day) = day_of(&name) {
                    inner.days.entry(day).or_default().insert(name, size);
                }
            }
            inner.ready = true;
        }
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }
    /// True once the index came from disk or a full sync, so it can be trusted for listing.
    pub fn ready(&self) -> bool {
        self.inner.lock().unwrap().ready
    }
    pub fn add(&self, name: &str, size: u64) {
        if let Some(day) = day_of(name) {
            let mut inner = self.inner.lock().unwrap();
            inner.days.entry(day).or_default().insert(name.into(), size);
            inner.dirty = true;
        }
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
        self.inner.lock().unwrap().days.get(&day)?.get(name).copied()
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
                    .filter_map(|(name, size)| parse_name(name, *size))
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Merge a complete listing: add what is new and drop what vanished, except clips younger
    /// than ten minutes (they may not have reached the listing yet).
    pub fn sync(&self, listing: Vec<(String, u64)>) {
        let recent = Utc::now() - Age::minutes(10);
        let mut inner = self.inner.lock().unwrap();
        let mut fresh: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for (name, size) in listing {
            if let Some(day) = day_of(&name) {
                fresh.entry(day).or_default().insert(name, size);
            }
        }
        for (day, bucket) in &inner.days {
            for (name, size) in bucket {
                if parse_name(name, 0).is_some_and(|c| c.start > recent) {
                    fresh
                        .entry(day.clone())
                        .or_default()
                        .entry(name.clone())
                        .or_insert(*size);
                }
            }
        }
        if fresh != inner.days {
            inner.days = fresh;
            inner.dirty = true;
        }
        inner.ready = true;
    }
    pub async fn persist(&self) {
        let Some(path) = &self.path else { return };
        let bytes = {
            let mut inner = self.inner.lock().unwrap();
            if !inner.dirty {
                return;
            }
            inner.dirty = false;
            let clips = inner
                .days
                .values()
                .flat_map(|b| b.iter().map(|(n, s)| (n.clone(), *s)))
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
pub fn index_path(dir: &Path) -> PathBuf {
    dir.join(".index.json")
}
