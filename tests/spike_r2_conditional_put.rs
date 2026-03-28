//! Spike test: verify that Cloudflare R2 honours `If-None-Match: *` on PUT requests.
//!
//! # Purpose
//!
//! The whirlwind lock protocol depends on a single atomic conditional PUT:
//!
//!   PUT /locks/<project>.lock
//!   If-None-Match: *
//!
//! The first PUT must succeed (200/204); a second PUT to the same key must fail with
//! 412 Precondition Failed.  If R2 silently ignores the header the lock is non-atomic
//! and two simultaneous pushes can corrupt each other.
//!
//! # Running
//!
//! This test requires a real Cloudflare R2 bucket and valid credentials.  It cannot
//! run in CI without secrets.  Skip it when credentials are absent — the test detects
//! missing environment variables and returns early (pass) with a SKIP notice.
//!
//! ```sh
//! R2_ACCOUNT_ID=<account_id> \
//! R2_ACCESS_KEY_ID=<key_id> \
//! R2_SECRET_ACCESS_KEY=<secret> \
//! R2_BUCKET=<bucket_name> \
//! cargo test --test spike_r2_conditional_put -- --nocapture
//! ```
//!
//! The test prints a clear PASS or FAIL result before the assertions fire.

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client,
    config::{Builder, Region},
    primitives::ByteStream,
};

const TEST_KEY: &str = "spike-test/lock-test.lock";
const TEST_BODY: &str =
    r#"{"locked_by":"spike-test","locked_at":"2026-03-28T00:00:00Z","machine":"ci"}"#;

/// Build an S3 client pointed at Cloudflare R2.
///
/// R2's S3-compatible endpoint is:
///   https://<account_id>.r2.cloudflarestorage.com
///
/// The region must be the literal string "auto" — R2 ignores the value but the
/// SDK requires one to be set.  We also enable path-style addressing because R2
/// does not support virtual-hosted-style bucket URLs.
async fn build_r2_client(account_id: &str, access_key_id: &str, secret_access_key: &str) -> Client {
    let endpoint = format!("https://{}.r2.cloudflarestorage.com", account_id);

    let creds = Credentials::from_keys(access_key_id, secret_access_key, None);

    // Build from minimal defaults so we do not accidentally inherit ambient
    // AWS_* environment variables that might point at real AWS endpoints.
    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new("auto"))
        .endpoint_url(&endpoint)
        .credentials_provider(creds)
        .load()
        .await;

    let s3_config = Builder::from(&sdk_config).force_path_style(true).build();

    Client::from_conf(s3_config)
}

/// Attempt a conditional PUT (`If-None-Match: *`).
///
/// Returns:
/// - `Ok(true)`  — 200/204, object written (lock acquired)
/// - `Ok(false)` — 412 Precondition Failed (lock already held)
/// - `Err(_)`    — any other failure
async fn conditional_put(client: &Client, bucket: &str, key: &str) -> Result<bool, String> {
    let result = client
        .put_object()
        .bucket(bucket)
        .key(key)
        .if_none_match("*")
        .content_type("application/json")
        .body(ByteStream::from_static(TEST_BODY.as_bytes()))
        .send()
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(sdk_err) => {
            // The aws-sdk-s3 v1.x SDK surfaces 412 as an unhandled service
            // error.  The most reliable way to detect it is via the raw HTTP
            // response status code attached to the SdkError.
            let http_status = sdk_err.raw_response().map(|r| r.status().as_u16());
            match http_status {
                Some(412) => Ok(false),
                Some(code) => Err(format!("Unexpected HTTP {}: {:?}", code, sdk_err)),
                None => {
                    // Fallback: inspect the Debug representation.  This handles
                    // edge cases where the raw response is not attached (e.g.
                    // the SDK parsed and re-raised the error before attaching
                    // the response).
                    let debug = format!("{:?}", sdk_err);
                    if debug.contains("412") || debug.contains("PreconditionFailed") {
                        Ok(false)
                    } else {
                        Err(format!("SDK error (no raw response): {:?}", sdk_err))
                    }
                }
            }
        }
    }
}

/// Delete the test object so the bucket is clean after the test runs (or is
/// interrupted and re-run).
async fn cleanup(client: &Client, bucket: &str, key: &str) {
    let _ = client.delete_object().bucket(bucket).key(key).send().await;
}

#[tokio::test]
async fn test_r2_conditional_put_if_none_match() {
    // ----------------------------------------------------------------
    // 1. Read credentials from environment.  Skip gracefully if absent.
    // ----------------------------------------------------------------
    let account_id = std::env::var("R2_ACCOUNT_ID").unwrap_or_default();
    let access_key_id = std::env::var("R2_ACCESS_KEY_ID").unwrap_or_default();
    let secret_access_key = std::env::var("R2_SECRET_ACCESS_KEY").unwrap_or_default();
    let bucket = std::env::var("R2_BUCKET").unwrap_or_default();

    if account_id.is_empty()
        || access_key_id.is_empty()
        || secret_access_key.is_empty()
        || bucket.is_empty()
    {
        println!();
        println!("=================================================================");
        println!("SKIP: R2 credentials not set.");
        println!("To run this spike, provide all four environment variables:");
        println!("  R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET");
        println!();
        println!("  R2_ACCOUNT_ID=... \\");
        println!("  R2_ACCESS_KEY_ID=... \\");
        println!("  R2_SECRET_ACCESS_KEY=... \\");
        println!("  R2_BUCKET=... \\");
        println!("  cargo test --test spike_r2_conditional_put -- --nocapture");
        println!("=================================================================");
        println!();
        // Early return without panic = test passes (skipped).
        return;
    }

    println!();
    println!("=================================================================");
    println!("Spike: If-None-Match:* conditional PUT on Cloudflare R2");
    println!(
        "  endpoint : https://{}.r2.cloudflarestorage.com",
        account_id
    );
    println!("  bucket   : {}", bucket);
    println!("  key      : {}", TEST_KEY);
    println!("=================================================================");

    let client = build_r2_client(&account_id, &access_key_id, &secret_access_key).await;

    // Pre-clean in case a previous run was interrupted before the DELETE step.
    cleanup(&client, &bucket, TEST_KEY).await;

    // ----------------------------------------------------------------
    // 2. First PUT — object does not exist; must succeed.
    // ----------------------------------------------------------------
    println!("[1] PUT with If-None-Match:* (object absent — expect 200/204) ...");
    let first = conditional_put(&client, &bucket, TEST_KEY).await;
    match &first {
        Ok(true) => println!("    -> 200/204 OK  (lock acquired)  [expected]"),
        Ok(false) => println!("    -> 412         (UNEXPECTED — object should not exist yet)"),
        Err(e) => println!("    -> ERROR: {}", e),
    }

    // ----------------------------------------------------------------
    // 3. Second PUT — object now exists; must return 412.
    // ----------------------------------------------------------------
    println!("[2] PUT with If-None-Match:* (object present — expect 412) ...");
    let second = conditional_put(&client, &bucket, TEST_KEY).await;
    match &second {
        Ok(false) => {
            println!("    -> 412         (lock contention detected correctly)  [expected]")
        }
        Ok(true) => {
            println!("    -> 200/204 OK  (UNEXPECTED — R2 ignored If-None-Match:*)");
        }
        Err(e) => println!("    -> ERROR: {}", e),
    }

    // ----------------------------------------------------------------
    // 4. Clean up regardless of outcome.
    // ----------------------------------------------------------------
    println!("[3] DELETE test object ...");
    cleanup(&client, &bucket, TEST_KEY).await;
    println!("    -> done");

    // ----------------------------------------------------------------
    // 5. Print a human-readable verdict before assertions fire.
    // ----------------------------------------------------------------
    println!();
    let pass = matches!(first, Ok(true)) && matches!(second, Ok(false));
    if pass {
        println!("PASS: R2 correctly enforces If-None-Match:* on conditional PUT.");
        println!("      The whirlwind lock protocol is safe to implement as designed.");
    } else {
        println!("FAIL: R2 did NOT correctly enforce If-None-Match:* on conditional PUT.");
        println!("      The lock protocol needs to fall back to probabilistic lock-by-naming.");
        println!("      See TDD section 9 Risk 1 and open a redesign issue before proceeding.");
    }
    println!("=================================================================");
    println!();

    assert!(
        matches!(first, Ok(true)),
        "First PUT (object absent) should succeed with 200/204, got: {:?}",
        first
    );
    assert!(
        matches!(second, Ok(false)),
        "Second PUT (object present) should fail with 412, got: {:?}",
        second
    );
}
