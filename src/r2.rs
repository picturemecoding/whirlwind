use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    config::{Builder, Region},
    primitives::ByteStream,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use md5::{Digest, Md5};

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
    /// Maps SDK errors to `AppError::DownloadFailed`.
    pub async fn get_object_bytes(&self, key: &str) -> Result<Bytes, AppError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::DownloadFailed {
                path: key.to_string(),
                source: Box::new(e),
            })?;

        let body = resp
            .body
            .collect()
            .await
            .map_err(|e| AppError::DownloadFailed {
                path: key.to_string(),
                source: Box::new(e),
            })?;

        Ok(body.into_bytes())
    }

    /// Unconditional PUT — overwrite or create the object at `key`.
    ///
    /// Computes the MD5 of `body` and attaches it as custom metadata under
    /// the key `"content-md5"` (stored by R2/S3 as `x-amz-meta-content-md5`).
    ///
    /// Maps SDK errors to `AppError::UploadFailed`.
    pub async fn put_object(&self, key: &str, body: Vec<u8>) -> Result<(), AppError> {
        let md5_hex = compute_md5_hex(&body);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .metadata("content-md5", &md5_hex)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| AppError::UploadFailed {
                path: key.to_string(),
                source: Box::new(e),
            })?;

        Ok(())
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
}
