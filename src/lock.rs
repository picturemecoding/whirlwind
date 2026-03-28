use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::r2::{AcquireResult, R2Client};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    pub locked_by: String,
    pub locked_at: DateTime<Utc>,
    pub machine: String,
}

pub const STALE_LOCK_THRESHOLD_HOURS: i64 = 4;

pub fn is_stale(lock: &LockFile) -> bool {
    Utc::now() - lock.locked_at > Duration::hours(STALE_LOCK_THRESHOLD_HOURS)
}

// ---------------------------------------------------------------------------
// LockGuard — RAII wrapper that releases the lock on drop
// ---------------------------------------------------------------------------

pub struct LockGuard {
    pub project: String,
    r2: Arc<R2Client>,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let key = R2Client::lock_key(&self.project);
        let r2 = Arc::clone(&self.r2);
        let project = self.project.clone();

        // Calling async code from a sync Drop context while inside a tokio
        // runtime requires block_in_place so we don't block the async executor
        // thread directly.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let result = tokio::task::block_in_place(|| {
                    handle.block_on(async move { r2.delete_object(&key).await })
                });
                if let Err(e) = result {
                    eprintln!("Warning: failed to release lock for {}: {}", project, e);
                }
            }
            Err(_) => {
                eprintln!(
                    "Warning: failed to release lock for {}: tokio runtime not available",
                    project
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LockManager
// ---------------------------------------------------------------------------

pub struct LockManager {
    r2: Arc<R2Client>,
    config: Arc<crate::config::Config>,
}

impl LockManager {
    pub fn new(r2: Arc<R2Client>, config: Arc<crate::config::Config>) -> Self {
        Self { r2, config }
    }

    /// Acquire the lock for `project`.
    ///
    /// Returns a `LockGuard` on success. The guard releases the lock when
    /// dropped. Returns `AppError::LockContention` or `AppError::SelfLock`
    /// when the lock is already held.
    pub async fn acquire(&self, project: &str) -> Result<LockGuard, AppError> {
        let lock_key = R2Client::lock_key(project);

        let lock_file = LockFile {
            locked_by: self.config.identity.user.clone(),
            locked_at: Utc::now(),
            machine: self.config.identity.machine.clone(),
        };

        let body = serde_json::to_vec(&lock_file)
            .map_err(|e| AppError::R2Error(format!("failed to serialize lock file: {}", e)))?;

        match self.r2.put_object_if_not_exists(&lock_key, body).await? {
            AcquireResult::Acquired => Ok(LockGuard {
                project: project.to_string(),
                r2: Arc::clone(&self.r2),
            }),
            AcquireResult::AlreadyExists => {
                // Fetch the existing lock to build a useful error message.
                let existing = self.read(project).await?.ok_or_else(|| {
                    // Race: lock disappeared between the 412 and our GET.
                    // Treat as a generic contention error with minimal info.
                    AppError::LockContention {
                        project: project.to_string(),
                        locked_by: "(unknown)".to_string(),
                        machine: "(unknown)".to_string(),
                        locked_at: "(unknown)".to_string(),
                    }
                })?;

                let locked_at_str = existing.locked_at.format("%Y-%m-%d %H:%M UTC").to_string();

                // Self-lock: same user and machine from a previous session.
                if existing.locked_by == self.config.identity.user
                    && existing.machine == self.config.identity.machine
                {
                    return Err(AppError::SelfLock {
                        project: project.to_string(),
                        user: existing.locked_by,
                        machine: existing.machine,
                    });
                }

                Err(AppError::LockContention {
                    project: project.to_string(),
                    locked_by: existing.locked_by,
                    machine: existing.machine,
                    locked_at: locked_at_str,
                })
            }
        }
    }

    /// Release the lock for `project`.
    ///
    /// A 404 (already released) is treated as success — the operation is
    /// idempotent.
    pub async fn release(&self, project: &str) -> Result<(), AppError> {
        let lock_key = R2Client::lock_key(project);
        self.r2.delete_object(&lock_key).await
    }

    /// Read the current lock file for `project`, if one exists.
    ///
    /// Returns `Ok(None)` when no lock is held. Maps deserialization errors to
    /// `AppError::R2Error`.
    pub async fn read(&self, project: &str) -> Result<Option<LockFile>, AppError> {
        let lock_key = R2Client::lock_key(project);

        match self.r2.get_object_bytes(&lock_key).await {
            Ok(bytes) => {
                let lock_file: LockFile = serde_json::from_slice(&bytes).map_err(|e| {
                    AppError::R2Error(format!(
                        "failed to deserialize lock file for '{}': {}",
                        project, e
                    ))
                })?;
                Ok(Some(lock_file))
            }
            Err(AppError::DownloadFailed { .. }) => {
                // get_object_bytes maps 404 to DownloadFailed; treat as not found.
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_file_round_trips_json() {
        let lf = LockFile {
            locked_by: "alice".into(),
            locked_at: Utc::now(),
            machine: "macbook".into(),
        };
        let json = serde_json::to_string(&lf).unwrap();
        let lf2: LockFile = serde_json::from_str(&json).unwrap();
        assert_eq!(lf.locked_by, lf2.locked_by);
        assert_eq!(lf.machine, lf2.machine);
    }

    #[test]
    fn fresh_lock_is_not_stale() {
        let lf = LockFile {
            locked_by: "alice".into(),
            locked_at: Utc::now(),
            machine: "m".into(),
        };
        assert!(!is_stale(&lf));
    }

    #[test]
    fn old_lock_is_stale() {
        let lf = LockFile {
            locked_by: "alice".into(),
            locked_at: Utc::now() - Duration::hours(5),
            machine: "m".into(),
        };
        assert!(is_stale(&lf));
    }

    #[test]
    fn exactly_at_threshold_is_not_stale() {
        let lf = LockFile {
            locked_by: "alice".into(),
            locked_at: Utc::now() - Duration::hours(STALE_LOCK_THRESHOLD_HOURS),
            machine: "m".into(),
        };
        // Exactly at threshold: the comparison is >, so this should NOT be stale.
        assert!(!is_stale(&lf));
    }
}
