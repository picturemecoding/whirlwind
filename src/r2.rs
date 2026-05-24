use std::future::Future;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    config::{Builder, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::config::TransferConfig;
use crate::error::AppError;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

pub struct R2Object {
    /// Key relative to the prefix (prefix stripped).
    pub key: String,
    /// ETag with quotes stripped.
    pub etag: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
    /// MD5 hex digest stored as custom metadata (`x-amz-meta-content-md5`).
    /// Populated via `head_object`; `list_objects` always returns `None` because
    /// list responses do not include user metadata.
    pub content_md5: Option<String>,
}

pub struct R2ObjectMeta {
    /// ETag with quotes stripped.
    pub etag: String,
    pub size: u64,
    /// MD5 hex digest stored as custom metadata (`x-amz-meta-content-md5`).
    pub content_md5: Option<String>,
}

pub enum AcquireResult {
    Acquired,
    AlreadyExists,
}

// R2/S3 minimum part size for non-final multipart parts.
// Used to validate the config value at startup; see Config::validate().
pub const MIN_MULTIPART_CHUNK_MB: u64 = 5;

// ---------------------------------------------------------------------------
// Key-path helpers
// ---------------------------------------------------------------------------

/// R2Client wraps `aws_sdk_s3::Client` with R2-specific initialisation.
pub struct R2Client {
    client: aws_sdk_s3::Client,
    pub bucket: String,
    transfer: TransferConfig,
}

impl R2Client {
    /// Key conventions — used throughout the codebase.
    pub const METADATA_KEY: &'static str = "metadata.json";

    pub fn project_prefix(project: &str) -> String {
        format!("projects/{}/", project)
    }

    pub fn lock_key(project: &str) -> String {
        format!("locks/{}.lock", project)
    }

    pub fn template_key(template_name: &str, uppercase: bool) -> String {
        if uppercase {
            format!("templates/{}.RPP", template_name)
        } else {
            format!("templates/{}.rpp", template_name)
        }
    }

    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------

    /// Build an `R2Client` from the application config.
    ///
    /// Endpoint URL is `https://<account_id>.r2.cloudflarestorage.com`.
    /// Region is the literal string `"auto"` (required by R2).
    /// Credentials come from `config.toml` — NOT from environment variables.
    pub async fn new(config: &crate::config::Config) -> Result<Self, AppError> {
        let endpoint = format!("https://{}.r2.cloudflarestorage.com", config.r2.account_id);

        let creds =
            Credentials::from_keys(&config.r2.access_key_id, &config.r2.secret_access_key, None);

        // Use `aws_config::defaults` so we do not accidentally inherit ambient
        // AWS_* environment variables that point at real AWS endpoints.
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new("auto"))
            .endpoint_url(&endpoint)
            .credentials_provider(creds)
            .load()
            .await;

        let s3_config = Builder::from(&sdk_config).force_path_style(true).build();

        let client = aws_sdk_s3::Client::from_conf(s3_config);

        Ok(Self {
            client,
            bucket: config.r2.bucket.clone(),
            transfer: config.transfer.clone(),
        })
    }

    // -----------------------------------------------------------------------
    // Public async methods
    // -----------------------------------------------------------------------

    /// List all objects under `prefix`, handling pagination transparently.
    ///
    /// The prefix is stripped from each returned `R2Object.key`.
    /// Returns an empty `Vec` when the prefix contains no objects.
    /// Note: `content_md5` is always `None` here — list responses do not
    /// include user metadata. Use `head_object` when the MD5 is required.
    pub async fn list_objects(&self, prefix: &str) -> Result<Vec<R2Object>, AppError> {
        let mut objects: Vec<R2Object> = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.map_err(|e| {
                AppError::R2Error(format!(
                    "list_objects failed for prefix '{}': {}",
                    prefix, e
                ))
            })?;

            for obj in resp.contents() {
                let full_key = obj.key().unwrap_or_default();
                // Strip the prefix to get the relative key.
                let relative_key = full_key
                    .strip_prefix(prefix)
                    .unwrap_or(full_key)
                    .to_string();

                let raw_etag = obj.e_tag().unwrap_or_default();
                let etag = strip_etag_quotes(raw_etag);

                let size = obj.size().unwrap_or(0) as u64;

                let last_modified = obj
                    .last_modified()
                    .and_then(|dt| {
                        let secs = dt.secs();
                        let nanos = dt.subsec_nanos();
                        DateTime::from_timestamp(secs, nanos)
                    })
                    .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());

                objects.push(R2Object {
                    key: relative_key,
                    etag,
                    size,
                    last_modified,
                    content_md5: None,
                });
            }

            if resp.is_truncated().unwrap_or(false) {
                continuation_token = resp.next_continuation_token().map(str::to_string);
            } else {
                break;
            }
        }

        Ok(objects)
    }

    /// Download an object and return its body as `Bytes`.
    ///
    /// Maps HTTP 404 / NoSuchKey to `AppError::NotFound`; all other SDK errors
    /// to `AppError::DownloadFailed`. Retries transient failures up to
    /// `config.transfer.retry_count` times with exponential backoff.
    pub async fn get_object_bytes(&self, key: &str) -> Result<Bytes, AppError> {
        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);

        retry_with_backoff(
            &op_name,
            max_retries,
            initial_delay,
            timeout,
            is_transient_error,
            || async {
                let result = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await;

                let resp = match result {
                    Ok(r) => r,
                    Err(sdk_err) => {
                        let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
                        if matches!(http_status, Some(404)) {
                            return Err(AppError::NotFound {
                                key: key.to_string(),
                            });
                        }
                        let debug = format!("{:?}", sdk_err);
                        if debug.contains("NoSuchKey") {
                            return Err(AppError::NotFound {
                                key: key.to_string(),
                            });
                        }
                        let msg = format_sdk_error_msg(http_status, &sdk_err);
                        return Err(AppError::DownloadFailed {
                            path: key.to_string(),
                            source: Box::new(std::io::Error::other(msg)),
                        });
                    }
                };

                let body = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| AppError::DownloadFailed {
                        path: key.to_string(),
                        source: Box::new(e),
                    })?;

                Ok(body.into_bytes())
            },
        )
        .await
    }

    /// Download an object and write it directly to `path` without loading the
    /// entire body into memory.
    ///
    /// Writes to a `.tmp` sibling first; renames to `path` only on success so
    /// a failed download never leaves a partial file at the target path.
    ///
    /// Calls `on_progress(bytes_written_so_far)` after each chunk.
    /// Retries transient failures up to `config.transfer.retry_count` times.
    pub async fn get_object_file(
        &self,
        key: &str,
        path: &Path,
        on_progress: impl Fn(u64),
    ) -> Result<u64, AppError> {
        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);

        let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
        temp_name.push(".tmp");
        let temp_path = path.with_file_name(temp_name);

        let result = retry_with_backoff(
            &op_name,
            max_retries,
            initial_delay,
            timeout,
            is_transient_error,
            || async {
                let get_result = self
                    .client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .send()
                    .await;

                let resp = match get_result {
                    Ok(r) => r,
                    Err(sdk_err) => {
                        let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
                        if matches!(http_status, Some(404)) {
                            return Err(AppError::NotFound {
                                key: key.to_string(),
                            });
                        }
                        let debug = format!("{:?}", sdk_err);
                        if debug.contains("NoSuchKey") {
                            return Err(AppError::NotFound {
                                key: key.to_string(),
                            });
                        }
                        let msg = format_sdk_error_msg(http_status, &sdk_err);
                        return Err(AppError::DownloadFailed {
                            path: key.to_string(),
                            source: Box::new(std::io::Error::other(msg)),
                        });
                    }
                };

                let mut file =
                    tokio::fs::File::create(&temp_path)
                        .await
                        .map_err(|e| AppError::IoError {
                            path: temp_path.display().to_string(),
                            source: e,
                        })?;

                let mut stream = resp.body.into_async_read();
                let mut buf = vec![0u8; 65536];
                let mut bytes_written: u64 = 0;

                loop {
                    let n = stream
                        .read(&mut buf)
                        .await
                        .map_err(|e| AppError::DownloadFailed {
                            path: key.to_string(),
                            source: Box::new(e),
                        })?;
                    if n == 0 {
                        break;
                    }
                    file.write_all(&buf[..n])
                        .await
                        .map_err(|e| AppError::IoError {
                            path: temp_path.display().to_string(),
                            source: e,
                        })?;
                    bytes_written += n as u64;
                    on_progress(bytes_written);
                }

                file.flush().await.map_err(|e| AppError::IoError {
                    path: temp_path.display().to_string(),
                    source: e,
                })?;

                Ok(bytes_written)
            },
        )
        .await;

        match result {
            Ok(n) => {
                tokio::fs::rename(&temp_path, path)
                    .await
                    .map_err(|e| AppError::IoError {
                        path: path.display().to_string(),
                        source: e,
                    })?;
                Ok(n)
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                Err(e)
            }
        }
    }

    /// Unconditional PUT — overwrite or create the object at `key`.
    ///
    /// Calls `on_progress(bytes_sent_so_far)` after each transferred chunk so
    /// callers can advance a progress bar incrementally.
    ///
    /// Files smaller than `MULTIPART_THRESHOLD` are sent as a single PUT.
    /// Larger files are split into `MULTIPART_CHUNK_SIZE` parts and uploaded
    /// via the S3 multipart API, which retries individual parts rather than
    /// restarting the whole transfer on failure.
    ///
    /// Permanent client errors (4xx except 408/429) are not retried.
    pub async fn put_object(
        &self,
        key: &str,
        body: Vec<u8>,
        on_progress: impl Fn(u64),
    ) -> Result<(), AppError> {
        let threshold = self.transfer.multipart_threshold_mb as usize * 1024 * 1024;
        let chunk_size = self.transfer.multipart_chunk_mb as usize * 1024 * 1024;
        if body.len() >= threshold {
            return self
                .put_object_multipart(key, body, chunk_size, on_progress)
                .await;
        }

        let md5_hex = compute_md5_hex(&body);
        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);
        let total_bytes = body.len() as u64;

        retry_with_backoff(
            &op_name,
            max_retries,
            initial_delay,
            timeout,
            is_transient_error,
            || async {
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(key)
                    .metadata("content-md5", &md5_hex)
                    .body(ByteStream::from(body.clone()))
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| {
                        let http_status = e.raw_response().map(|r| r.status().as_u16());
                        let msg = format_sdk_error_msg(http_status, &e);
                        AppError::UploadFailed {
                            path: key.to_string(),
                            source: Box::new(std::io::Error::other(msg)),
                        }
                    })
            },
        )
        .await?;

        on_progress(total_bytes);
        Ok(())
    }

    /// Upload `body` to `key` using the S3 multipart upload API.
    ///
    /// Parts are uploaded sequentially; each part is retried independently on
    /// transient failure.  If any part or the final complete call fails after
    /// all retries, the incomplete multipart upload is aborted so R2 does not
    /// accumulate orphaned parts.
    async fn put_object_multipart(
        &self,
        key: &str,
        body: Vec<u8>,
        chunk_size: usize,
        on_progress: impl Fn(u64),
    ) -> Result<(), AppError> {
        let md5_hex = compute_md5_hex(&body);
        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);
        let total_parts = body.chunks(chunk_size).count();

        // Step 1 — start the multipart upload and obtain an upload_id.
        let create_op = format!("{} create-multipart", op_name);
        let create_resp = retry_with_backoff(
            &create_op,
            max_retries,
            initial_delay,
            timeout,
            is_transient_error,
            || async {
                self.client
                    .create_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .metadata("content-md5", &md5_hex)
                    .send()
                    .await
                    .map_err(|e| {
                        let status = e.raw_response().map(|r| r.status().as_u16());
                        AppError::UploadFailed {
                            path: key.to_string(),
                            source: Box::new(std::io::Error::other(format_sdk_error_msg(
                                status, &e,
                            ))),
                        }
                    })
            },
        )
        .await?;

        let upload_id = create_resp
            .upload_id()
            .ok_or_else(|| AppError::UploadFailed {
                path: key.to_string(),
                source: Box::new(std::io::Error::other("R2 did not return an upload_id")),
            })?
            .to_string();

        // Step 2 — upload each part; abort and propagate on any failure.
        let mut completed_parts: Vec<CompletedPart> = Vec::with_capacity(total_parts);
        let mut bytes_sent: u64 = 0;

        for (i, chunk) in body.chunks(chunk_size).enumerate() {
            let part_number = (i + 1) as i32;
            let chunk_owned = chunk.to_vec();
            let part_op = format!("{} part {}/{}", op_name, part_number, total_parts);

            let etag_result = retry_with_backoff(
                &part_op,
                max_retries,
                initial_delay,
                timeout,
                is_transient_error,
                || async {
                    self.client
                        .upload_part()
                        .bucket(&self.bucket)
                        .key(key)
                        .upload_id(&upload_id)
                        .part_number(part_number)
                        .body(ByteStream::from(chunk_owned.clone()))
                        .send()
                        .await
                        .map_err(|e| {
                            let status = e.raw_response().map(|r| r.status().as_u16());
                            AppError::UploadFailed {
                                path: key.to_string(),
                                source: Box::new(std::io::Error::other(format_sdk_error_msg(
                                    status, &e,
                                ))),
                            }
                        })
                        .and_then(|r| {
                            r.e_tag()
                                .ok_or_else(|| AppError::UploadFailed {
                                    path: key.to_string(),
                                    source: Box::new(std::io::Error::other(
                                        "upload_part response missing ETag",
                                    )),
                                })
                                .map(|e| e.to_string())
                        })
                },
            )
            .await;

            match etag_result {
                Ok(etag) => {
                    bytes_sent += chunk.len() as u64;
                    on_progress(bytes_sent);
                    completed_parts.push(
                        CompletedPart::builder()
                            .e_tag(etag)
                            .part_number(part_number)
                            .build(),
                    );
                }
                Err(e) => {
                    let _ = self.abort_multipart_upload(key, &upload_id).await;
                    return Err(e);
                }
            }
        }

        // Step 3 — assemble the parts into the final object.
        let complete_result = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed_parts))
                    .build(),
            )
            .send()
            .await
            .map(|_| ())
            .map_err(|e| {
                let status = e.raw_response().map(|r| r.status().as_u16());
                AppError::UploadFailed {
                    path: key.to_string(),
                    source: Box::new(std::io::Error::other(format_sdk_error_msg(status, &e))),
                }
            });

        if complete_result.is_err() {
            let _ = self.abort_multipart_upload(key, &upload_id).await;
        }

        complete_result
    }

    /// Upload a local file to `key`, streaming it from disk rather than loading
    /// it into memory first.
    ///
    /// Files smaller than `transfer.multipart_threshold_mb` are read in full and
    /// sent as a single PUT (fast for small files).  Larger files are streamed
    /// one chunk at a time through the S3 multipart API — only one chunk is ever
    /// held in memory, regardless of how large the file is.
    pub async fn put_object_file(
        &self,
        key: &str,
        path: &Path,
        on_progress: impl Fn(u64),
    ) -> Result<(), AppError> {
        let file_size = std::fs::metadata(path)
            .map_err(|e| AppError::IoError {
                path: path.display().to_string(),
                source: e,
            })?
            .len();

        let threshold = self.transfer.multipart_threshold_mb * 1024 * 1024;

        if file_size < threshold {
            let bytes = std::fs::read(path).map_err(|e| AppError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;
            return self.put_object(key, bytes, on_progress).await;
        }

        let chunk_size = self.transfer.multipart_chunk_mb as usize * 1024 * 1024;
        self.put_object_multipart_file(key, path, file_size, chunk_size, on_progress)
            .await
    }

    /// Multipart upload of a file from disk, streaming one chunk at a time.
    ///
    /// Makes two sequential passes over the file: one to compute the full-file
    /// MD5 (stored as `x-amz-meta-content-md5` for skip detection), then one
    /// to stream chunks for upload.  Each chunk is retried independently on
    /// transient failure; the upload is aborted on unrecoverable error.
    async fn put_object_multipart_file(
        &self,
        key: &str,
        path: &Path,
        file_size: u64,
        chunk_size: usize,
        on_progress: impl Fn(u64),
    ) -> Result<(), AppError> {
        let md5_hex = compute_file_md5_hex(path)
            .await
            .map_err(|e| AppError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;

        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);
        let total_parts = (file_size as usize).div_ceil(chunk_size);

        // Step 1 — initiate.
        let create_op = format!("{} create-multipart", op_name);
        let create_resp = retry_with_backoff(
            &create_op,
            max_retries,
            initial_delay,
            timeout,
            is_transient_error,
            || async {
                self.client
                    .create_multipart_upload()
                    .bucket(&self.bucket)
                    .key(key)
                    .metadata("content-md5", &md5_hex)
                    .send()
                    .await
                    .map_err(|e| {
                        let status = e.raw_response().map(|r| r.status().as_u16());
                        AppError::UploadFailed {
                            path: key.to_string(),
                            source: Box::new(std::io::Error::other(format_sdk_error_msg(
                                status, &e,
                            ))),
                        }
                    })
            },
        )
        .await?;

        let upload_id = create_resp
            .upload_id()
            .ok_or_else(|| AppError::UploadFailed {
                path: key.to_string(),
                source: Box::new(std::io::Error::other("R2 did not return an upload_id")),
            })?
            .to_string();

        // Step 2 — stream and upload each chunk.
        let mut completed_parts: Vec<CompletedPart> = Vec::with_capacity(total_parts);
        let mut bytes_sent: u64 = 0;

        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| AppError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;

        for part_num in 1i32..=(total_parts as i32) {
            let this_chunk_size = chunk_size.min((file_size - bytes_sent) as usize);
            let mut chunk = vec![0u8; this_chunk_size];

            if let Err(e) = file.read_exact(&mut chunk).await {
                let _ = self.abort_multipart_upload(key, &upload_id).await;
                return Err(AppError::IoError {
                    path: path.display().to_string(),
                    source: e,
                });
            }

            let part_op = format!("{} part {}/{}", op_name, part_num, total_parts);

            let etag_result = retry_with_backoff(
                &part_op,
                max_retries,
                initial_delay,
                timeout,
                is_transient_error,
                || async {
                    self.client
                        .upload_part()
                        .bucket(&self.bucket)
                        .key(key)
                        .upload_id(&upload_id)
                        .part_number(part_num)
                        .body(ByteStream::from(chunk.clone()))
                        .send()
                        .await
                        .map_err(|e| {
                            let status = e.raw_response().map(|r| r.status().as_u16());
                            AppError::UploadFailed {
                                path: key.to_string(),
                                source: Box::new(std::io::Error::other(format_sdk_error_msg(
                                    status, &e,
                                ))),
                            }
                        })
                        .and_then(|r| {
                            r.e_tag()
                                .ok_or_else(|| AppError::UploadFailed {
                                    path: key.to_string(),
                                    source: Box::new(std::io::Error::other(
                                        "upload_part response missing ETag",
                                    )),
                                })
                                .map(|e| e.to_string())
                        })
                },
            )
            .await;

            match etag_result {
                Ok(etag) => {
                    bytes_sent += this_chunk_size as u64;
                    on_progress(bytes_sent);
                    completed_parts.push(
                        CompletedPart::builder()
                            .e_tag(etag)
                            .part_number(part_num)
                            .build(),
                    );
                }
                Err(e) => {
                    let _ = self.abort_multipart_upload(key, &upload_id).await;
                    return Err(e);
                }
            }
        }

        // Step 3 — complete.
        let complete_result = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(completed_parts))
                    .build(),
            )
            .send()
            .await
            .map(|_| ())
            .map_err(|e| {
                let status = e.raw_response().map(|r| r.status().as_u16());
                AppError::UploadFailed {
                    path: key.to_string(),
                    source: Box::new(std::io::Error::other(format_sdk_error_msg(status, &e))),
                }
            });

        if complete_result.is_err() {
            let _ = self.abort_multipart_upload(key, &upload_id).await;
        }

        complete_result
    }

    /// Abort an in-progress multipart upload, freeing its stored parts.
    ///
    /// Called automatically on upload failure.  Errors here are logged but not
    /// propagated — the original upload error takes priority.
    async fn abort_multipart_upload(&self, key: &str, upload_id: &str) -> Result<(), AppError> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| {
                AppError::R2Error(format!(
                    "abort_multipart_upload failed for '{}': {}",
                    key, e
                ))
            })
    }

    /// Conditional PUT with `If-None-Match: *`.
    ///
    /// Computes the MD5 of `body` and attaches it as custom metadata under
    /// the key `"content-md5"` (stored by R2/S3 as `x-amz-meta-content-md5`).
    ///
    /// - Returns `AcquireResult::Acquired` when the object did not exist and was created.
    /// - Returns `AcquireResult::AlreadyExists` when the server returns HTTP 412.
    /// - Returns `Err(AppError::UploadFailed)` for all other failures.
    pub async fn put_object_if_not_exists(
        &self,
        key: &str,
        body: Vec<u8>,
    ) -> Result<AcquireResult, AppError> {
        let md5_hex = compute_md5_hex(&body);

        let result = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .if_none_match("*")
            .metadata("content-md5", &md5_hex)
            .body(ByteStream::from(body))
            .send()
            .await;

        match result {
            Ok(_) => Ok(AcquireResult::Acquired),
            Err(sdk_err) => {
                // The aws-sdk-s3 v1.x SDK surfaces 412 as an unhandled service
                // error.  The most reliable detection is via the raw HTTP
                // response status code attached to the SdkError.
                let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
                match http_status {
                    Some(412) => Ok(AcquireResult::AlreadyExists),
                    _ => {
                        // Fallback: inspect the Debug representation.
                        let debug = format!("{:?}", sdk_err);
                        if debug.contains("PreconditionFailed") {
                            Ok(AcquireResult::AlreadyExists)
                        } else {
                            Err(AppError::UploadFailed {
                                path: key.to_string(),
                                source: Box::new(sdk_err),
                            })
                        }
                    }
                }
            }
        }
    }

    /// DELETE the object at `key`.
    ///
    /// Treats 404 / NoSuchKey as success — the operation is idempotent.
    pub async fn delete_object(&self, key: &str) -> Result<(), AppError> {
        let result = self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(sdk_err) => {
                // delete_object on S3-compatible stores returns 204 for both
                // existing and non-existing keys, so a 404 here is unexpected
                // but treat it as success for idempotency.
                let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
                if matches!(http_status, Some(404)) {
                    return Ok(());
                }
                let debug = format!("{:?}", sdk_err);
                if debug.contains("NoSuchKey") || debug.contains("404") {
                    return Ok(());
                }
                Err(AppError::R2Error(format!(
                    "delete_object failed for key '{}': {}",
                    key, sdk_err
                )))
            }
        }
    }

    /// Generate a presigned GET URL for the object at `key`.
    ///
    /// The URL is valid for `expires_in` from the time this method is called.
    /// Returns the URL as a `String` that is ready to be shared or printed.
    pub async fn presign_get_object(
        &self,
        key: &str,
        expires_in: Duration,
    ) -> Result<String, AppError> {
        let presigning_config = PresigningConfig::expires_in(expires_in)
            .map_err(|e| AppError::R2Error(format!("invalid presigning duration: {}", e)))?;

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presigning_config)
            .await
            .map_err(|e| {
                AppError::R2Error(format!(
                    "presign_get_object failed for key '{}': {}",
                    key, e
                ))
            })?;

        Ok(presigned.uri().to_string())
    }

    /// HEAD the object at `key`.
    ///
    /// - Returns `None` when the object does not exist (404 / NoSuchKey / NotFound).
    /// - Returns `Some(R2ObjectMeta)` on success, with `content_md5` populated
    ///   from the `x-amz-meta-content-md5` custom metadata header when present.
    pub async fn head_object(&self, key: &str) -> Result<Option<R2ObjectMeta>, AppError> {
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(resp) => {
                let raw_etag = resp.e_tag().unwrap_or_default();
                let etag = strip_etag_quotes(raw_etag);
                let size = resp.content_length().unwrap_or(0) as u64;
                let content_md5 = resp.metadata().and_then(|m| m.get("content-md5")).cloned();
                Ok(Some(R2ObjectMeta {
                    etag,
                    size,
                    content_md5,
                }))
            }
            Err(sdk_err) => {
                let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
                if matches!(http_status, Some(404)) {
                    return Ok(None);
                }
                let debug = format!("{:?}", sdk_err);
                if debug.contains("NoSuchKey")
                    || debug.contains("NotFound")
                    || debug.contains("404")
                {
                    return Ok(None);
                }
                Err(AppError::R2Error(format!(
                    "head_object failed for key '{}': {}",
                    key, sdk_err
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Retry a fallible async operation with exponential backoff.
///
/// - Attempts the operation up to `max_retries + 1` times total.
/// - On each retry (not the first attempt), prints the error that triggered
///   the retry and waits an exponentially increasing delay, starting at
///   `initial_delay`, doubling each attempt, capped at 30 seconds.
/// - If `should_retry` returns `false` for an error, that error is returned
///   immediately without further attempts (e.g. permanent auth failures).
/// - Returns the first `Ok` result, or the last `Err` after all retries
///   are exhausted.
///
/// This function is intentionally NOT used by `put_object_if_not_exists`
/// because a 412 response is a deliberate lock-contention signal, not a
/// transient failure.
async fn retry_with_backoff<F, Fut, T, S>(
    op_name: &str,
    max_retries: u32,
    initial_delay: Duration,
    timeout: Duration,
    should_retry: S,
    mut operation: F,
) -> Result<T, AppError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
    S: Fn(&AppError) -> bool,
{
    const MAX_DELAY: Duration = Duration::from_secs(30);

    let mut last_err: Option<AppError> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay = initial_delay
                .saturating_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX))
                .min(MAX_DELAY);
            let reason = last_err.as_ref().map(|e| e.to_string()).unwrap_or_default();
            eprintln!(
                "Retrying {} (attempt {}/{}) — {}",
                op_name, attempt, max_retries, reason
            );
            tokio::time::sleep(delay).await;
        }

        match tokio::time::timeout(timeout, operation()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(err)) => {
                if !should_retry(&err) {
                    return Err(err);
                }
                last_err = Some(err);
            }
            Err(_elapsed) => {
                last_err = Some(AppError::Other(format!(
                    "{} timed out after {}s (increase transfer.timeout_secs in config to allow more time on slow networks)",
                    op_name,
                    timeout.as_secs()
                )));
            }
        }
    }

    Err(last_err.expect("loop always runs at least once"))
}

/// Format an SDK error into a human-readable string that includes the HTTP
/// status code when one is available.  Stored as a plain string so callers
/// can pattern-match on "HTTP NNN:" for retry decisions.
fn format_sdk_error_msg(http_status: Option<u16>, e: &dyn std::fmt::Display) -> String {
    match http_status {
        Some(status) => format!("HTTP {status}: {e}"),
        None => e.to_string(),
    }
}

/// Return `true` when an error is likely transient and worth retrying.
///
/// Permanent client errors (HTTP 4xx, excluding 408 Request Timeout and
/// 429 Too Many Requests) are not retried — they indicate a configuration
/// or permission problem that won't resolve on its own.
fn is_transient_error(err: &AppError) -> bool {
    match err {
        AppError::NotFound { .. } => false,
        AppError::UploadFailed { source, .. } | AppError::DownloadFailed { source, .. } => {
            let msg = source.to_string();
            // Our formatted messages start with "HTTP NNN: ..." when a status is known.
            if let Some(rest) = msg.strip_prefix("HTTP ")
                && let Some((status_str, _)) = rest.split_once(':')
                && let Ok(status) = status_str.trim().parse::<u16>()
            {
                // 4xx are permanent except 408 (Request Timeout) and 429 (Rate Limited)
                return !(400..500).contains(&status) || status == 408 || status == 429;
            }
            true
        }
        _ => true,
    }
}

/// Compute the MD5 hex digest of a file on disk without loading it into memory.
///
/// Runs in a `spawn_blocking` thread so the async executor is not stalled by
/// the sequential disk read.
pub(crate) async fn compute_file_md5_hex(path: &Path) -> Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        let mut reader = std::io::BufReader::new(file);
        let mut hasher = Md5::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok::<String, std::io::Error>(
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        )
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Compute the MD5 hex digest of `data`.
pub(crate) fn compute_md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Strip surrounding double-quotes from an ETag value.
///
/// R2 (and S3) return ETags with quotes: `"abc123"`.  The sync engine
/// compares ETags to local MD5 hex strings, which have no quotes.
fn strip_etag_quotes(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_etag_quotes_removes_surrounding_quotes() {
        assert_eq!(strip_etag_quotes("\"abc123\""), "abc123");
    }

    #[test]
    fn strip_etag_quotes_leaves_unquoted_etag_unchanged() {
        assert_eq!(strip_etag_quotes("abc123"), "abc123");
    }

    #[test]
    fn strip_etag_quotes_handles_empty_string() {
        assert_eq!(strip_etag_quotes(""), "");
    }

    #[test]
    fn project_prefix_format() {
        assert_eq!(
            R2Client::project_prefix("episode-47"),
            "projects/episode-47/"
        );
    }

    #[test]
    fn lock_key_format() {
        assert_eq!(R2Client::lock_key("episode-47"), "locks/episode-47.lock");
    }

    #[test]
    fn metadata_key_is_correct() {
        assert_eq!(R2Client::METADATA_KEY, "metadata.json");
    }

    #[test]
    fn compute_md5_hex_known_value() {
        // MD5("") == d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(compute_md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn compute_md5_hex_nonempty() {
        // MD5("hello") == 5d41402abc4b2a76b9719d911017c592
        assert_eq!(
            compute_md5_hex(b"hello"),
            "5d41402abc4b2a76b9719d911017c592"
        );
    }

    // -----------------------------------------------------------------------
    // retry_with_backoff tests
    // -----------------------------------------------------------------------

    /// A successful operation on the first attempt is returned immediately
    /// without any retries.
    #[tokio::test]
    async fn retry_succeeds_on_first_attempt() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(
            "test-op",
            3,
            Duration::from_millis(1),
            Duration::from_secs(60),
            |_| true,
            || {
                let cc = cc.clone();
                async move {
                    cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<u32, AppError>(42)
                }
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "expected exactly one attempt"
        );
    }

    /// An operation that always fails is retried `max_retries` times and then
    /// returns the final error.
    #[tokio::test]
    async fn retry_exhausts_all_attempts_on_persistent_failure() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(
            "test-op",
            2,
            Duration::from_millis(1),
            Duration::from_secs(60),
            |_| true,
            || {
                let cc = cc.clone();
                async move {
                    cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<u32, AppError>(AppError::Other("transient".to_string()))
                }
            },
        )
        .await;

        assert!(result.is_err());
        // max_retries=2 means 3 total attempts: initial + 2 retries
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "expected 3 total attempts (initial + 2 retries)"
        );
    }

    /// An operation that fails on the first two attempts but succeeds on the
    /// third is returned as Ok.
    #[tokio::test]
    async fn retry_succeeds_on_third_attempt() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(
            "test-op",
            3,
            Duration::from_millis(1),
            Duration::from_secs(60),
            |_| true,
            || {
                let cc = cc.clone();
                async move {
                    let n = cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        Err(AppError::Other("transient".to_string()))
                    } else {
                        Ok::<u32, AppError>(99)
                    }
                }
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 99);
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "expected 3 total attempts"
        );
    }

    /// With max_retries=0, only one attempt is made (no retries).
    #[tokio::test]
    async fn retry_zero_max_retries_makes_single_attempt() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(
            "test-op",
            0,
            Duration::from_millis(1),
            Duration::from_secs(60),
            |_| true,
            || {
                let cc = cc.clone();
                async move {
                    cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<u32, AppError>(AppError::Other("fail".to_string()))
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "expected exactly one attempt when max_retries=0"
        );
    }

    /// A permanent error (should_retry returns false) causes an immediate bail
    /// with only a single attempt — no retries are wasted.
    #[tokio::test]
    async fn retry_bails_immediately_on_permanent_error() {
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(
            "test-op",
            3,
            Duration::from_millis(1),
            Duration::from_secs(60),
            |_| false, // nothing is retryable
            || {
                let cc = cc.clone();
                async move {
                    cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<u32, AppError>(AppError::Other("permanent".to_string()))
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "expected exactly one attempt when should_retry is always false"
        );
    }

    // -----------------------------------------------------------------------
    // is_transient_error tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_transient_error_returns_true_for_network_error() {
        let err = AppError::UploadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("connection reset by peer")),
        };
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_false_for_http_403() {
        let err = AppError::UploadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("HTTP 403: Access Denied".to_string())),
        };
        assert!(!is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_false_for_http_401() {
        let err = AppError::UploadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("HTTP 401: Unauthorized")),
        };
        assert!(!is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_true_for_http_429() {
        let err = AppError::UploadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("HTTP 429: Too Many Requests")),
        };
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_true_for_http_408() {
        let err = AppError::UploadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("HTTP 408: Request Timeout")),
        };
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_true_for_http_503() {
        let err = AppError::DownloadFailed {
            path: "foo.wav".to_string(),
            source: Box::new(std::io::Error::other("HTTP 503: Service Unavailable")),
        };
        assert!(is_transient_error(&err));
    }

    #[test]
    fn is_transient_error_returns_false_for_not_found() {
        let err = AppError::NotFound {
            key: "projects/ep-47/foo.wav".to_string(),
        };
        assert!(!is_transient_error(&err));
    }

    #[test]
    fn format_sdk_error_msg_includes_status_when_present() {
        struct FakeErr;
        impl std::fmt::Display for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "AccessDenied")
            }
        }
        let msg = format_sdk_error_msg(Some(403), &FakeErr);
        assert_eq!(msg, "HTTP 403: AccessDenied");
    }

    // -----------------------------------------------------------------------
    // Multipart constants
    // -----------------------------------------------------------------------

    #[test]
    fn min_multipart_chunk_mb_meets_r2_requirement() {
        assert_eq!(MIN_MULTIPART_CHUNK_MB, 5);
    }

    #[test]
    fn default_transfer_config_thresholds_are_sensible() {
        let cfg = crate::config::TransferConfig::default();
        assert!(
            cfg.multipart_chunk_mb >= MIN_MULTIPART_CHUNK_MB,
            "default chunk size must meet R2 minimum"
        );
        assert!(
            cfg.multipart_threshold_mb >= 1,
            "default threshold must be positive"
        );
        if cfg.multipart_chunk_mb < cfg.multipart_threshold_mb {
            let threshold = cfg.multipart_threshold_mb as usize * 1024 * 1024;
            let chunk = cfg.multipart_chunk_mb as usize * 1024 * 1024;
            assert!(threshold.div_ceil(chunk) >= 2);
        }
    }

    #[test]
    fn format_sdk_error_msg_omits_prefix_when_no_status() {
        struct FakeErr;
        impl std::fmt::Display for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection reset")
            }
        }
        let msg = format_sdk_error_msg(None, &FakeErr);
        assert_eq!(msg, "connection reset");
    }
}
