//! Bounded background analysis. Hard links keep clips alive without delaying uploads.
use crate::{
    storage::{parse_name, QueueStatus, Store},
    thumbs::thumb_path,
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;
const WAITING: usize = 5;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const DIR: &str = ".analysis";
#[derive(Default)]
struct Active {
    names: HashSet<String>,
    bytes: u64,
}
struct Job {
    name: String,
    path: PathBuf,
    size: u64,
    active: Arc<Mutex<Active>>,
    keep: bool,
}
impl Drop for Job {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
        let mut active = self.active.lock().unwrap();
        active.names.remove(&self.name);
        active.bytes = active.bytes.saturating_sub(self.size);
    }
}
#[derive(Clone)]
pub struct AnalysisQueue {
    sender: mpsc::Sender<Job>,
    active: Arc<Mutex<Active>>,
    store: Store,
    status: Arc<AsyncMutex<QueueStatus>>,
}
impl AnalysisQueue {
    pub fn new(
        store: Store,
        status: Arc<AsyncMutex<QueueStatus>>,
        cancel: CancellationToken,
    ) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Job>(WAITING);
        let active = Arc::new(Mutex::new(Active::default()));
        let worker_store = store.clone();
        let worker_status = status.clone();
        tokio::spawn(async move {
            loop {
                let mut job = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    job = receiver.recv() => match job {Some(job)=>job,None=>break},
                };
                let completed = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => false,
                    _ = worker_store.ensure_thumb(&job.path) => true,
                };
                if completed {
                    if worker_store.index.motion(&job.name).is_none() {
                        worker_status.lock().await.analysis_failed += 1;
                    }
                    // Persist the result before dropping the last local hard link.
                    worker_store.index.persist().await;
                    job.keep = false;
                }
                drop(job);
                let mut status = worker_status.lock().await;
                status.analysis_pending = status.analysis_pending.saturating_sub(1);
                drop(status);
                if !completed {
                    break;
                }
            }
            receiver.close();
            while let Ok(job) = receiver.try_recv() {
                drop(job);
            }
            worker_status.lock().await.analysis_pending = 0;
        });
        Self {
            sender,
            active,
            store,
            status,
        }
    }
    pub async fn submit(&self, file: &Path) {
        let Some(name) = file.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
            return;
        };
        if parse_name(&name, 0).is_none() {
            return;
        }
        let pinned = file.parent() == Some(self.store.dir.join(DIR).as_path());
        if self.store.index.motion(&name).is_some()
            && thumb_path(&self.store.dir, &name).is_some_and(|p| p.exists())
        {
            if pinned {
                let _ = tokio::fs::remove_file(file).await;
            }
            return;
        }
        let Ok(meta) = tokio::fs::symlink_metadata(file).await else {
            return;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            return;
        }
        if self.active.lock().unwrap().names.contains(&name) {
            return;
        }
        let permit = match self.sender.try_reserve() {
            Ok(permit) => permit,
            Err(_) => {
                self.status.lock().await.analysis_skipped += 1;
                if pinned {
                    let _ = tokio::fs::remove_file(file).await;
                }
                return;
            }
        };
        let accepted = {
            let mut active = self.active.lock().unwrap();
            if active.names.contains(&name) {
                return;
            }
            if active.bytes.saturating_add(meta.len()) > MAX_BYTES {
                false
            } else {
                active.bytes += meta.len();
                active.names.insert(name.clone());
                true
            }
        };
        if !accepted {
            self.status.lock().await.analysis_skipped += 1;
            if pinned {
                let _ = tokio::fs::remove_file(file).await;
            }
            return;
        }
        let dir = self.store.dir.join(DIR);
        let mut job = Job {
            name: name.clone(),
            path: dir.join(&name),
            size: meta.len(),
            active: self.active.clone(),
            keep: true,
        };
        let result = async {
            tokio::fs::create_dir_all(&dir).await?;
            if !pinned {
                tokio::fs::hard_link(file, &job.path).await?;
            }
            Ok::<_, std::io::Error>(())
        }
        .await;
        if let Err(error) = result {
            job.keep = false;
            eprintln!("Antrean analisis {name} gagal: {error}");
            self.status.lock().await.analysis_failed += 1;
            return;
        }
        self.store.index.add(&name, meta.len());
        self.status.lock().await.analysis_pending += 1;
        permit.send(job);
    }
    pub async fn recover(&self) {
        let Ok(mut entries) = tokio::fs::read_dir(self.store.dir.join(DIR)).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            self.submit(&entry.path()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn idle(store: Store) -> (AnalysisQueue, mpsc::Receiver<Job>) {
        let (sender, receiver) = mpsc::channel(WAITING);
        (
            AnalysisQueue {
                sender,
                active: Arc::new(Mutex::new(Active::default())),
                store,
                status: Arc::new(AsyncMutex::new(QueueStatus::default())),
            },
            receiver,
        )
    }
    fn store(path: &Path) -> Store {
        Store::new(
            &serde_json::from_value(
                serde_json::json!({"enabled":true,"destination":"local","dir":path}),
            )
            .unwrap(),
        )
    }
    #[tokio::test]
    async fn backlog_is_bounded_and_cancelled_pins_can_be_recovered_after_upload() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let (queue, receiver) = idle(store.clone());
        for i in 0..9 {
            let path = dir.path().join(format!("cam1_20260101_1000{i:02}.mp4"));
            tokio::fs::write(&path, [0u8; 128]).await.unwrap();
            queue.submit(&path).await;
            assert!(
                path.exists(),
                "analysis must never remove the original recording"
            );
        }
        let status = queue.status.lock().await.clone();
        assert_eq!(status.analysis_pending, WAITING);
        assert_eq!(status.analysis_skipped, 4);
        assert_eq!(queue.active.lock().unwrap().bytes, 128 * WAITING as u64);
        drop(receiver);
        drop(queue); // Cancel/crash: keep accepted links for recovery.
        for i in 0..WAITING {
            tokio::fs::remove_file(dir.path().join(format!("cam1_20260101_1000{i:02}.mp4")))
                .await
                .unwrap();
        }
        let (recovered, _receiver) = idle(store);
        recovered.recover().await;
        assert_eq!(recovered.status.lock().await.analysis_pending, WAITING);
        assert_eq!(recovered.active.lock().unwrap().bytes, 128 * WAITING as u64);
    }
    #[tokio::test]
    async fn oversized_clip_stays_unknown_without_consuming_staging_space() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let (queue, _receiver) = idle(store.clone());
        let path = dir.path().join("cam1_20260101_100000.mp4");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap(); // Sparse: does not allocate 256 MiB on disk.
        store.index.add("cam1_20260101_100000.mp4", MAX_BYTES + 1);
        queue.submit(&path).await;
        assert_eq!(queue.status.lock().await.analysis_skipped, 1);
        assert_eq!(queue.active.lock().unwrap().bytes, 0);
        assert!(!dir.path().join(DIR).exists());
        assert_eq!(store.index.motion("cam1_20260101_100000.mp4"), None);
    }
}
