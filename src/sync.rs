use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use walkdir::WalkDir;

use crate::error::AppError;
use crate::progress::ProgressReporter;
use crate::r2::compute_file_md5_hex;

// ---------------------------------------------------------------------------
// Summary types
// ---------------------------------------------------------------------------

pub struct PushSummary {
    pub files_uploaded: usize,
    pub files_skipped: usize,
    pub total_bytes: u64,
}

pub struct PullSummary {
    pub files_downloaded: usize,
    pub files_skipped: usize,
    pub total_bytes: u64,
}

const UPLOAD_CONCURRENCY: usize = 4;

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
    /// Files whose local MD5 matches the R2 object's stored digest are skipped.
    /// The comparison uses the `x-amz-meta-content-md5` custom metadata field
    /// when present (reliable for both single-part and multipart uploads),
    /// falling back to the raw ETag for single-part objects.
    ///
    /// Files are uploaded concurrently up to `UPLOAD_CONCURRENCY` at a time.
    /// Large files stream from disk one chunk at a time — the whole file is
    /// never loaded into memory.
    ///
    /// Does NOT delete any R2 objects — see TDD section 7 "Deletion Semantics".
    pub async fn push(&self, project: &str, local_dir: &Path) -> Result<PushSummary, AppError> {
        // Phase 1: list remote objects and walk local files to decide what needs
        // uploading.  This phase is serial; the heavy work (parallel uploads)
        // happens in Phase 2.
        let prefix = crate::r2::R2Client::project_prefix(project);
        let remote_index: HashMap<String, crate::r2::R2Object> = self
            .r2
            .list_objects(&prefix)
            .await?
            .into_iter()
            .map(|obj| (format!("{}{}", prefix, obj.key), obj))
            .collect();

        // (r2_key, local_abs_path, byte_count, display_name)
        let mut to_upload: Vec<(String, std::path::PathBuf, u64, String)> = Vec::new();
        let mut files_skipped: usize = 0;

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

            let relative_path_str = relative_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");

            let r2_key = format!("{}{}", prefix, relative_path_str);

            let r2_meta: Option<crate::r2::R2ObjectMeta> = match remote_index.get(&r2_key) {
                None => None,
                Some(obj) if obj.etag.contains('-') => {
                    self.r2.head_object(&r2_key).await.ok().flatten()
                }
                Some(obj) => Some(crate::r2::R2ObjectMeta {
                    etag: obj.etag.clone(),
                    size: obj.size,
                    content_md5: None,
                }),
            };

            if let Some(ref meta) = r2_meta {
                // Stream the file to compute its MD5 without loading it fully.
                let local_md5 =
                    compute_local_etag(abs_path)
                        .await
                        .map_err(|e| AppError::IoError {
                            path: relative_path_str.clone(),
                            source: e,
                        })?;
                if etags_match(&local_md5, meta) {
                    files_skipped += 1;
                    continue;
                }
            }

            let byte_count = abs_path
                .metadata()
                .map_err(|e| AppError::IoError {
                    path: relative_path_str.clone(),
                    source: e,
                })?
                .len();

            to_upload.push((
                r2_key,
                abs_path.to_path_buf(),
                byte_count,
                relative_path_str,
            ));
        }

        // Phase 2: upload collected files concurrently.
        let reporter = Arc::new(ProgressReporter::new());
        let r2 = Arc::clone(&self.r2);

        let upload_results: Vec<Result<u64, AppError>> = stream::iter(to_upload)
            .map(|(r2_key, path, byte_count, display_name)| {
                let r2 = Arc::clone(&r2);
                let reporter = Arc::clone(&reporter);
                async move {
                    let bar = reporter.add_file_bar(&display_name, byte_count);
                    r2.put_object_file(&r2_key, &path, |pos| bar.update(pos))
                        .await
                        .map_err(|e| match e {
                            AppError::UploadFailed { source, .. } => AppError::UploadFailed {
                                path: display_name.clone(),
                                source,
                            },
                            other => other,
                        })?;
                    bar.finish(&display_name, byte_count);
                    Ok::<u64, AppError>(byte_count)
                }
            })
            .buffer_unordered(UPLOAD_CONCURRENCY)
            .collect()
            .await;

        let mut files_uploaded: usize = 0;
        let mut total_bytes: u64 = 0;
        for result in upload_results {
            total_bytes += result?;
            files_uploaded += 1;
        }

        println!(
            "Push complete: {} uploaded, {} unchanged, {} transferred.",
            files_uploaded,
            files_skipped,
            format_bytes(total_bytes)
        );

        Ok(PushSummary {
            files_uploaded,
            files_skipped,
            total_bytes,
        })
    }

    /// Download all R2 objects under `projects/<project>/` to `local_dir`.
    ///
    /// Files whose local MD5 matches the R2 ETag (or `x-amz-meta-content-md5`
    /// when available) are skipped. For multipart-uploaded objects where the
    /// ETag is a composite digest and no custom MD5 metadata is present, the
    /// file is always downloaded (conservative — no false skips).
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

        let reporter = ProgressReporter::new();
        let mut files_downloaded: usize = 0;
        let mut files_skipped: usize = 0;
        let mut total_bytes: u64 = 0;

        for obj in &objects {
            // object.key is relative (prefix already stripped in r2.rs).
            // Validate that the key does not escape local_dir via path traversal.
            if !is_safe_r2_key(&obj.key) {
                eprintln!(
                    "Warning: skipping '{}': key contains path traversal components",
                    obj.key
                );
                continue;
            }
            let local_path = local_dir.join(&obj.key);

            // Skip unchanged files: if local copy exists and MD5 matches.
            if local_path.exists()
                && let Ok(local_md5) = compute_local_etag(&local_path).await
            {
                let meta = crate::r2::R2ObjectMeta {
                    etag: obj.etag.clone(),
                    size: obj.size,
                    content_md5: obj.content_md5.clone(),
                };
                if etags_match(&local_md5, &meta) {
                    files_skipped += 1;
                    continue;
                }
            }

            // Ensure parent directories exist.
            if let Some(parent) = local_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::IoError {
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }

            let bar = reporter.add_file_bar(&obj.key, obj.size);

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

            bar.update(byte_count);
            bar.finish(&obj.key, byte_count);

            files_downloaded += 1;
            total_bytes += byte_count;
        }

        println!(
            "Pull complete: {} downloaded, {} unchanged, {} transferred.",
            files_downloaded,
            files_skipped,
            format_bytes(total_bytes)
        );

        Ok(PullSummary {
            files_downloaded,
            files_skipped,
            total_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return `true` if `key` is safe to join onto a local directory.
///
/// Rejects keys that are absolute or contain any `..` component, which would
/// allow a malicious or compromised bucket to write files outside `local_dir`.
fn is_safe_r2_key(key: &str) -> bool {
    let path = Path::new(key);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|c| c != std::path::Component::ParentDir)
}

/// Compute the MD5 hex digest of a local file without loading it fully.
async fn compute_local_etag(path: &Path) -> Result<String, std::io::Error> {
    compute_file_md5_hex(path).await
}

/// Compare a local MD5 hex digest against R2 object metadata.
///
/// Preference order:
/// 1. `content_md5` custom metadata (reliable for single-part and multipart).
/// 2. Raw ETag — but only for single-part uploads (no `-` in the ETag).
///
/// Returns `true` when the file is identical and can be skipped.
fn etags_match(local_md5: &str, r2_meta: &crate::r2::R2ObjectMeta) -> bool {
    if let Some(ref remote_md5) = r2_meta.content_md5 {
        return local_md5 == remote_md5.as_str();
    }
    // Multipart ETags contain a dash (e.g. "abc123-2"); cannot compare directly.
    if r2_meta.etag.contains('-') {
        return false;
    }
    local_md5 == r2_meta.etag.as_str()
}

pub fn format_bytes(bytes: u64) -> String {
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
    use crate::r2::R2ObjectMeta;

    #[test]
    fn is_safe_r2_key_accepts_normal_keys() {
        assert!(is_safe_r2_key("foo.rpp"));
        assert!(is_safe_r2_key("subdir/foo.rpp"));
        assert!(is_safe_r2_key("a/b/c.wav"));
    }

    #[test]
    fn is_safe_r2_key_rejects_parent_dir_traversal() {
        assert!(!is_safe_r2_key("../../.ssh/authorized_keys"));
        assert!(!is_safe_r2_key("subdir/../../../etc/passwd"));
        assert!(!is_safe_r2_key(".."));
    }

    #[test]
    fn is_safe_r2_key_rejects_absolute_paths() {
        assert!(!is_safe_r2_key("/etc/passwd"));
        assert!(!is_safe_r2_key("/tmp/evil"));
    }

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
            files_skipped: 2,
            total_bytes: 1024,
        };
        assert_eq!(s.files_uploaded, 3);
        assert_eq!(s.files_skipped, 2);
        assert_eq!(s.total_bytes, 1024);
    }

    #[test]
    fn pull_summary_has_correct_fields() {
        let s = PullSummary {
            files_downloaded: 5,
            files_skipped: 1,
            total_bytes: 2048,
        };
        assert_eq!(s.files_downloaded, 5);
        assert_eq!(s.files_skipped, 1);
        assert_eq!(s.total_bytes, 2048);
    }

    #[tokio::test]
    async fn compute_local_etag_known_value() {
        // MD5("hello\n") — write a known file and check the digest.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello").unwrap();
        // MD5("hello") == 5d41402abc4b2a76b9719d911017c592
        assert_eq!(
            compute_local_etag(&path).await.unwrap(),
            "5d41402abc4b2a76b9719d911017c592"
        );
    }

    #[test]
    fn etags_match_uses_content_md5_first() {
        let meta = R2ObjectMeta {
            etag: "different-etag".to_string(),
            size: 5,
            content_md5: Some("5d41402abc4b2a76b9719d911017c592".to_string()),
        };
        // content_md5 matches local_md5 — should skip even though etag differs.
        assert!(etags_match("5d41402abc4b2a76b9719d911017c592", &meta));
    }

    #[test]
    fn etags_match_falls_back_to_etag_for_single_part() {
        let meta = R2ObjectMeta {
            etag: "5d41402abc4b2a76b9719d911017c592".to_string(),
            size: 5,
            content_md5: None,
        };
        assert!(etags_match("5d41402abc4b2a76b9719d911017c592", &meta));
    }

    #[test]
    fn etags_match_returns_false_for_multipart_without_content_md5() {
        // Multipart ETags contain a dash — comparison is not possible without content_md5.
        let meta = R2ObjectMeta {
            etag: "abc123-2".to_string(),
            size: 10_000_000,
            content_md5: None,
        };
        // Even if local_md5 happens to equal the base of the etag, we must not skip.
        assert!(!etags_match("abc123", &meta));
    }

    #[test]
    fn etags_match_returns_false_on_mismatch() {
        let meta = R2ObjectMeta {
            etag: "abc123".to_string(),
            size: 5,
            content_md5: None,
        };
        assert!(!etags_match("def456", &meta));
    }
}
