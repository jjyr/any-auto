//! Per-user reviewer sessions. Only table operations hold the global lock.
use crate::{
    config::Mode,
    reviewer::{Assessment, Bridge},
};
use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub fn user_session_id(payload: &Value) -> Option<&str> {
    ["conversationId", "conversation_id"]
        .into_iter()
        .filter_map(|key| payload[key].as_str())
        .find(|id| !id.trim().is_empty())
}

pub fn directory(base: &Path, user_session_id: &str) -> PathBuf {
    base.join("sessions")
        .join(format!("{:x}", Sha256::digest(user_session_id.as_bytes())))
}

struct Session {
    bridge: tokio::sync::Mutex<Bridge>,
    last_used: Mutex<Instant>,
}

pub struct SessionPool {
    mode: Mode,
    base: PathBuf,
    entries: Mutex<HashMap<String, Arc<Session>>>,
}

pub struct SessionLease(Arc<Session>);
impl SessionLease {
    pub async fn evaluate(&self, request: &Value) -> Assessment {
        self.0.bridge.lock().await.evaluate(request).await
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        // Runs on success, failure, and cancellation, before releasing our Arc.
        *self.0.last_used.lock().unwrap() = Instant::now();
    }
}
impl SessionPool {
    pub fn new(mode: Mode, base: PathBuf) -> Self {
        Self {
            mode,
            base,
            entries: Mutex::new(HashMap::new()),
        }
    }
    pub fn acquire(&self, id: Option<&str>) -> Result<SessionLease> {
        let Some(id) = id.filter(|id| !id.trim().is_empty()) else {
            return Ok(SessionLease(Arc::new(Session {
                bridge: tokio::sync::Mutex::new(Bridge::temporary(self.mode)?),
                last_used: Mutex::new(Instant::now()),
            })));
        };
        let path = directory(&self.base, id);
        let key = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut entries = self.entries.lock().unwrap();
        if !entries.contains_key(&key) && entries.len() >= 32 {
            let oldest = entries
                .iter()
                .filter(|(_, session)| Arc::strong_count(session) == 1)
                .min_by_key(|(_, session)| *session.last_used.lock().unwrap())
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                entries.remove(&key);
            } else {
                anyhow::bail!("Reviewer session limit reached; retry after active reviews finish");
            }
        }
        let entry = entries.entry(key).or_insert_with(|| {
            Arc::new(Session {
                bridge: tokio::sync::Mutex::new(Bridge::persistent(self.mode, path)),
                last_used: Mutex::new(Instant::now()),
            })
        });
        Ok(SessionLease(Arc::clone(entry)))
    }
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn prune(&self, idle: Duration) {
        self.entries.lock().unwrap().retain(|_, session| {
            // Leases include requests waiting for the per-session lock.
            Arc::strong_count(session) > 1 || session.last_used.lock().unwrap().elapsed() < idle
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn eviction_preserves_active_and_queued_leases_and_reloads_disk() {
        let root = tempfile::tempdir().unwrap();
        let pool = SessionPool::new(Mode::Sidecar, root.path().to_owned());
        let first = pool.acquire(Some("user")).unwrap();
        let queued = pool.acquire(Some("user")).unwrap();
        assert!(Arc::ptr_eq(&first.0, &queued.0));
        let lock = first.0.bridge.lock().await;
        pool.prune(Duration::ZERO);
        assert_eq!(pool.len(), 1);
        drop(lock);
        drop(first);
        pool.prune(Duration::ZERO);
        assert_eq!(pool.len(), 1);
        drop(queued);
        pool.prune(Duration::from_secs(300));
        assert_eq!(pool.len(), 1);
        let path = directory(root.path(), "user");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("reviewer_session.json"),
            r#"{"conversationId":"persisted"}"#,
        )
        .unwrap();
        pool.prune(Duration::ZERO);
        assert!(pool.is_empty());
        let restored = pool.acquire(Some("user")).unwrap();
        assert_eq!(
            restored.0.bridge.lock().await.conversation_id.as_deref(),
            Some("persisted")
        );
    }
    #[tokio::test]
    async fn cancelled_waiter_releases_lease_and_lock() {
        let root = tempfile::tempdir().unwrap();
        let pool = SessionPool::new(Mode::Cli, root.path().to_owned());
        let first = pool.acquire(Some("user")).unwrap();
        let lock = first.0.bridge.lock().await;
        let queued = pool.acquire(Some("user")).unwrap();
        let request = serde_json::json!({});
        assert!(
            tokio::time::timeout(Duration::from_millis(1), async move {
                queued.evaluate(&request).await
            })
            .await
            .is_err()
        );
        assert_eq!(Arc::strong_count(&first.0), 2);
        drop(lock);
        assert!(first.0.bridge.try_lock().is_ok());
        drop(first);
        pool.prune(Duration::ZERO);
        assert!(pool.is_empty());
    }
}
