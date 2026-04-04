use std::future::Future;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    config::{Builder, Region},
    presigning::PresigningConfig,
    primitives::ByteStream,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use md5::{Digest, Md5};

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

        retry_with_backoff(&op_name, max_retries, initial_delay, timeout, || async {
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
                    return Err(AppError::DownloadFailed {
                        path: key.to_string(),
                        source: Box::new(sdk_err),
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
        })
        .await
    }

    /// Unconditional PUT — overwrite or create the object at `key`.
    ///
    /// Computes the MD5 of `body` and attaches it as custom metadata under
    /// the key `"content-md5"` (stored by R2/S3 as `x-amz-meta-content-md5`).
    ///
    /// Maps SDK errors to `AppError::UploadFailed`. Retries transient failures
    /// up to `config.transfer.retry_count` times with exponential backoff.
    pub async fn put_object(&self, key: &str, body: Vec<u8>) -> Result<(), AppError> {
        let md5_hex = compute_md5_hex(&body);
        let op_name = key.rsplit('/').next().unwrap_or(key).to_string();
        let max_retries = self.transfer.retry_count;
        let initial_delay = Duration::from_secs(1);
        let timeout = Duration::from_secs(self.transfer.timeout_secs);

        retry_with_backoff(&op_name, max_retries, initial_delay, timeout, || async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .metadata("content-md5", &md5_hex)
                .body(ByteStream::from(body.clone()))
                .send()
                .await
                .map_err(|e| AppError::UploadFailed {
                    path: key.to_string(),
                    source: Box::new(e),
                })?;

            Ok(())
        })
        .await
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
/// - On each retry (not the first attempt), prints a message to stderr and
///   waits an exponentially increasing delay, starting at `initial_delay`,
///   doubling each attempt, capped at 30 seconds.
/// - Returns the first `Ok` result, or the last `Err` after all retries
///   are exhausted.
///
/// This function is intentionally NOT used by `put_object_if_not_exists`
/// because a 412 response is a deliberate lock-contention signal, not a
/// transient failure.
async fn retry_with_backoff<F, Fut, T>(
    op_name: &str,
    max_retries: u32,
    initial_delay: Duration,
    timeout: Duration,
    mut operation: F,
) -> Result<T, AppError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    const MAX_DELAY: Duration = Duration::from_secs(30);

    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay = initial_delay
                .saturating_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX))
                .min(MAX_DELAY);
            eprintln!(
                "Retrying {} (attempt {}/{})...",
                op_name, attempt, max_retries
            );
            tokio::time::sleep(delay).await;
        }

        match tokio::time::timeout(timeout, operation()).await {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(err)) => last_err = Some(err),
            Err(_elapsed) => {
                last_err = Some(AppError::Other(format!(
                    "{} timed out after {}s",
                    op_name,
                    timeout.as_secs()
                )));
            }
        }
    }

    Err(last_err.expect("loop always runs at least once"))
}

/// Compute the MD5 hex digest of `data`.
pub(crate) fn compute_md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
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
}
