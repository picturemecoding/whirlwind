use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Top-level metadata document stored at `metadata.json` in the R2 bucket.
///
/// The `version` field is present for forward-compatibility with the on-disk
/// JSON schema described in the TDD (section 4). It is ignored by this
/// implementation but preserved on round-trips so that future versions can
/// detect schema evolution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    /// Schema version — always written as `1`.  Defaults to `0` when absent
    /// (e.g. an object written before this field existed).
    #[serde(default)]
    pub version: u32,

    /// Map of project name → last-push statistics.
    pub projects: HashMap<String, ProjectEntry>,
}

/// Per-project statistics recorded after each successful push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub last_pushed_by: String,
    pub last_pushed_at: DateTime<Utc>,
    pub object_count: u32,
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// MetadataManager
// ---------------------------------------------------------------------------

/// Reads and writes `metadata.json` in R2.
pub struct MetadataManager {
    r2: Arc<crate::r2::R2Client>,
}

impl MetadataManager {
    pub fn new(r2: Arc<crate::r2::R2Client>) -> Self {
        Self { r2 }
    }

    /// Load `metadata.json` from R2.
    ///
    /// Returns an empty [`Metadata`] on first run (object does not exist yet).
    /// Returns `Err(AppError::R2Error)` for real R2 failures.
    pub async fn load(&self) -> Result<Metadata, crate::error::AppError> {
        match self
            .r2
            .get_object_bytes(crate::r2::R2Client::METADATA_KEY)
            .await
        {
            Ok(bytes) => {
                let metadata: Metadata = serde_json::from_slice(&bytes).map_err(|e| {
                    crate::error::AppError::R2Error(format!(
                        "failed to deserialize metadata.json: {}",
                        e
                    ))
                })?;
                Ok(metadata)
            }
            // First run — metadata.json does not exist yet.
            Err(crate::error::AppError::DownloadFailed { .. }) => Ok(Metadata::default()),
            Err(e) => Err(crate::error::AppError::R2Error(e.to_string())),
        }
    }

    /// Save `metadata.json` to R2.
    ///
    /// Maps all errors to `AppError::R2Error`.
    pub async fn save(&self, metadata: &Metadata) -> Result<(), crate::error::AppError> {
        let bytes = serde_json::to_vec(metadata).map_err(|e| {
            crate::error::AppError::R2Error(format!("failed to serialize metadata.json: {}", e))
        })?;

        self.r2
            .put_object(crate::r2::R2Client::METADATA_KEY, bytes)
            .await
            .map_err(|e| crate::error::AppError::R2Error(e.to_string()))?;

        Ok(())
    }

    /// Update the entry for `project` after a successful push, then persist.
    ///
    /// Performs a read-modify-write.  Races on `metadata.json` are acceptable
    /// per the TDD (section 4) because the lock protocol prevents concurrent
    /// pushes to the same project, and metadata is informational only.
    pub async fn record_push(
        &self,
        project: &str,
        pushed_by: &str,
        object_count: u32,
        total_bytes: u64,
    ) -> Result<(), crate::error::AppError> {
        let mut metadata = self.load().await?;

        metadata.projects.insert(
            project.to_string(),
            ProjectEntry {
                last_pushed_by: pushed_by.to_string(),
                last_pushed_at: Utc::now(),
                object_count,
                total_bytes,
            },
        );

        // Ensure version is stamped on every write.
        metadata.version = 1;

        self.save(&metadata).await?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_default_has_empty_projects() {
        let m = Metadata::default();
        assert!(m.projects.is_empty());
    }

    #[test]
    fn metadata_round_trips_json() {
        let mut m = Metadata::default();
        m.projects.insert(
            "ep-1".to_string(),
            ProjectEntry {
                last_pushed_by: "alice".to_string(),
                last_pushed_at: DateTime::from_timestamp(0, 0).unwrap(),
                object_count: 3,
                total_bytes: 1024,
            },
        );
        let json = serde_json::to_string(&m).unwrap();
        let m2: Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(m2.projects["ep-1"].last_pushed_by, "alice");
        assert_eq!(m2.projects["ep-1"].object_count, 3);
    }

    #[test]
    fn metadata_deserializes_without_version_field() {
        // Ensure backward compat: JSON without "version" should deserialize
        // with version defaulting to 0.
        let json = r#"{"projects":{}}"#;
        let m: Metadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.version, 0);
        assert!(m.projects.is_empty());
    }

    #[test]
    fn metadata_deserializes_full_tdd_example() {
        let json = r#"{
            "version": 1,
            "projects": {
                "episode-47": {
                    "last_pushed_by": "alice",
                    "last_pushed_at": "2026-03-28T10:00:00Z",
                    "object_count": 3,
                    "total_bytes": 847392810
                }
            }
        }"#;
        let m: Metadata = serde_json::from_str(json).unwrap();
        assert_eq!(m.version, 1);
        let ep = &m.projects["episode-47"];
        assert_eq!(ep.last_pushed_by, "alice");
        assert_eq!(ep.object_count, 3);
        assert_eq!(ep.total_bytes, 847_392_810);
    }

    #[test]
    fn project_entry_round_trips_json() {
        let entry = ProjectEntry {
            last_pushed_by: "bob".to_string(),
            last_pushed_at: DateTime::from_timestamp(1_000_000, 0).unwrap(),
            object_count: 5,
            total_bytes: 99_999,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let entry2: ProjectEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry2.last_pushed_by, "bob");
        assert_eq!(entry2.object_count, 5);
        assert_eq!(entry2.total_bytes, 99_999);
        assert_eq!(entry2.last_pushed_at, entry.last_pushed_at);
    }
}
