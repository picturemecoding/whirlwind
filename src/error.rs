#[derive(thiserror::Error, Debug)]
pub enum AppError {
    // Config errors
    #[error("No config found. Run `whirlwind init` first.")]
    ConfigMissing,
    #[error("Config is invalid: {0}")]
    ConfigInvalid(String),

    // R2 errors
    #[error("R2 authentication failed: check your access_key_id and secret_access_key")]
    R2AuthFailure,
    #[error("R2 error: {0}")]
    R2Error(String),

    // Lock errors
    #[error(
        "{project} is currently locked.\nLocked by: {locked_by} ({machine})\nLocked at: {locked_at}\nRun `whirlwind unlock {project}` to break the lock."
    )]
    LockContention {
        project: String,
        locked_by: String,
        machine: String,
        locked_at: String,
    },
    #[error(
        "{project} is locked by you ({user} on {machine}) from a previous session.\nRun `whirlwind push {project}` to upload your changes, or `whirlwind unlock {project}` to discard the lock."
    )]
    SelfLock {
        project: String,
        user: String,
        machine: String,
    },
    #[error("Lock not found for {project}")]
    LockNotFound { project: String },

    // Sync errors
    #[error("Download failed for {path}: {source}")]
    DownloadFailed {
        path: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Upload failed for {path}: {source}")]
    UploadFailed {
        path: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("I/O error at {path}: {source}")]
    IoError {
        path: String,
        #[source]
        source: std::io::Error,
    },

    // Process errors
    #[error("Reaper binary not found at {path}. Check your config.")]
    ReaperNotFound { path: String },
    #[error("Failed to launch Reaper: {0}")]
    ReaperSpawnFailed(String),

    // Other
    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AppError::LockContention { .. } | AppError::SelfLock { .. } => 2,
            AppError::Other(_) => 1,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_contention_message_contains_project_name() {
        let err = AppError::LockContention {
            project: "episode-47".to_string(),
            locked_by: "bob".to_string(),
            machine: "bob-macbook".to_string(),
            locked_at: "2026-03-28T10:00:00Z".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("episode-47"),
            "expected project name in message: {msg}"
        );
        assert!(msg.contains("bob"), "expected locked_by in message: {msg}");
        assert!(
            msg.contains("whirlwind unlock episode-47"),
            "expected unlock hint in message: {msg}"
        );
    }

    #[test]
    fn self_lock_message_contains_recovery_hint() {
        let err = AppError::SelfLock {
            project: "episode-47".to_string(),
            user: "alice".to_string(),
            machine: "alice-macbook".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("episode-47"),
            "expected project name in message: {msg}"
        );
        assert!(
            msg.contains("whirlwind push episode-47"),
            "expected push hint in message: {msg}"
        );
        assert!(
            msg.contains("whirlwind unlock episode-47"),
            "expected unlock hint in message: {msg}"
        );
    }

    #[test]
    fn config_missing_message_contains_init_hint() {
        let err = AppError::ConfigMissing;
        let msg = err.to_string();
        assert!(
            msg.contains("whirlwind init"),
            "expected init hint in message: {msg}"
        );
    }

    #[test]
    fn exit_code_lock_contention_is_2() {
        let err = AppError::LockContention {
            project: "episode-47".to_string(),
            locked_by: "bob".to_string(),
            machine: "bob-macbook".to_string(),
            locked_at: "2026-03-28T10:00:00Z".to_string(),
        };
        assert_eq!(err.exit_code(), 2);

        let self_lock = AppError::SelfLock {
            project: "episode-47".to_string(),
            user: "alice".to_string(),
            machine: "alice-macbook".to_string(),
        };
        assert_eq!(self_lock.exit_code(), 2);
    }

    #[test]
    fn exit_code_general_error_is_1() {
        assert_eq!(AppError::ConfigMissing.exit_code(), 1);
        assert_eq!(AppError::R2AuthFailure.exit_code(), 1);
        assert_eq!(
            AppError::Other("something went wrong".to_string()).exit_code(),
            1
        );
        assert_eq!(
            AppError::ReaperNotFound {
                path: "/usr/bin/reaper".to_string()
            }
            .exit_code(),
            1
        );
    }
}
