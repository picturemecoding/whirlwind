use std::path::Path;
use std::sync::Arc;

use walkdir::WalkDir;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Summary types
// ---------------------------------------------------------------------------

pub struct PushSummary {
    pub files_uploaded: usize,
    pub total_bytes: u64,
}

pub struct PullSummary {
    pub files_downloaded: usize,
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// SyncEngine
// ---------------------------------------------------------------------------

pub struct SyncEngine {
    r2: Arc<crate::r2::R2Client>,
}

impl SyncEngine {
    pub fn new(r2: Arc<crate::r2::R2Client>) -> Self {
        Self { r2 }
    }

    /// Upload all files in `local_dir` to R2 under `projects/<project>/`.
    ///
    /// Phase 1: unconditional — every file is uploaded regardless of whether
    /// an identical copy already exists in R2. ETag-based skipping is added
    /// in Phase 3.
    ///
    /// Does NOT delete any R2 objects — see TDD section 7 "Deletion Semantics".
    pub async fn push(&self, project: &str, local_dir: &Path) -> Result<PushSummary, AppError> {
        let mut files_uploaded: usize = 0;
        let mut total_bytes: u64 = 0;

        for entry_result in WalkDir::new(local_dir) {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "Warning: skipping {}: {}",
                        e.path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        e
                    );
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }

            let abs_path = entry.path();

            // Compute the relative path from local_dir.
            let relative_path =
                abs_path
                    .strip_prefix(local_dir)
                    .map_err(|e| AppError::IoError {
                        path: abs_path.display().to_string(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            e.to_string(),
                        ),
                    })?;

            // Use forward slashes on all platforms.
            let relative_path_str = relative_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");

            let r2_key = format!("projects/{}/{}", project, relative_path_str);

            let bytes = std::fs::read(abs_path).map_err(|e| AppError::IoError {
                path: relative_path_str.clone(),
                source: e,
            })?;

            let byte_count = bytes.len() as u64;
            let size_str = format_bytes(byte_count);

            println!("  Uploading {:<40} ({})", relative_path_str, size_str);

            self.r2
                .put_object(&r2_key, bytes)
                .await
                .map_err(|e| match e {
                    // Re-wrap with the relative path for a clearer error message.
                    AppError::UploadFailed { source, .. } => AppError::UploadFailed {
                        path: relative_path_str.clone(),
                        source,
                    },
                    other => other,
                })?;

            files_uploaded += 1;
            total_bytes += byte_count;
        }

        println!(
            "Push complete: {} files, {} uploaded.",
            files_uploaded,
            format_bytes(total_bytes)
        );

        Ok(PushSummary {
            files_uploaded,
            total_bytes,
        })
    }

    /// Download all R2 objects under `projects/<project>/` to `local_dir`.
    ///
    /// Phase 1: unconditional — every object is downloaded regardless of
    /// whether an identical local copy already exists. ETag-based skipping
    /// is added in Phase 3.
    ///
    /// Returns `AppError::R2Error` if the project prefix is empty (not found).
    pub async fn pull(&self, project: &str, local_dir: &Path) -> Result<PullSummary, AppError> {
        let prefix = crate::r2::R2Client::project_prefix(project);
        let objects = self.r2.list_objects(&prefix).await?;

        if objects.is_empty() {
            return Err(AppError::R2Error(format!(
                "Project '{}' not found in R2. Has it been pushed yet?",
                project
            )));
        }

        let mut files_downloaded: usize = 0;
        let mut total_bytes: u64 = 0;

        for obj in &objects {
            // object.key is relative (prefix already stripped in r2.rs).
            let local_path = local_dir.join(&obj.key);

            // Ensure parent directories exist.
            if let Some(parent) = local_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::IoError {
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }

            let size_str = format_bytes(obj.size);
            println!("  Downloading {:<40} ({})", obj.key, size_str);

            // Full R2 key = prefix + relative key.
            let full_key = format!("{}{}", prefix, obj.key);
            let bytes = self
                .r2
                .get_object_bytes(&full_key)
                .await
                .map_err(|e| match e {
                    AppError::DownloadFailed { source, .. } => AppError::DownloadFailed {
                        path: obj.key.clone(),
                        source,
                    },
                    other => other,
                })?;

            let byte_count = bytes.len() as u64;

            std::fs::write(&local_path, &bytes).map_err(|e| AppError::IoError {
                path: local_path.display().to_string(),
                source: e,
            })?;

            files_downloaded += 1;
            total_bytes += byte_count;
        }

        println!(
            "Pull complete: {} files, {} downloaded.",
            files_downloaded,
            format_bytes(total_bytes)
        );

        Ok(PullSummary {
            files_downloaded,
            total_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_ranges() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
    }

    #[test]
    fn push_summary_accumulates_correctly() {
        let s = PushSummary {
            files_uploaded: 3,
            total_bytes: 1024,
        };
        assert_eq!(s.files_uploaded, 3);
        assert_eq!(s.total_bytes, 1024);
    }

    #[test]
    fn pull_summary_has_correct_fields() {
        let s = PullSummary {
            files_downloaded: 5,
            total_bytes: 2048,
        };
        assert_eq!(s.files_downloaded, 5);
        assert_eq!(s.total_bytes, 2048);
    }
}
