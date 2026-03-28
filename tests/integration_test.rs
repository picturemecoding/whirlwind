//! Integration tests for whirlwind R2 operations.
//!
//! These tests require a real Cloudflare R2 bucket. They are SKIPPED (not failed)
//! when the required environment variables are not set.
//!
//! Required env vars:
//!   WHIRLWIND_TEST_R2_ACCOUNT_ID
//!   WHIRLWIND_TEST_R2_ACCESS_KEY_ID
//!   WHIRLWIND_TEST_R2_SECRET_ACCESS_KEY
//!   WHIRLWIND_TEST_R2_BUCKET
//!
//! Run with:
//!   WHIRLWIND_TEST_R2_ACCOUNT_ID=... \
//!   WHIRLWIND_TEST_R2_ACCESS_KEY_ID=... \
//!   WHIRLWIND_TEST_R2_SECRET_ACCESS_KEY=... \
//!   WHIRLWIND_TEST_R2_BUCKET=... \
//!   cargo test --test integration_test -- --nocapture

use std::sync::Arc;
use whirlwind::{
    config::{Config, IdentityConfig, LocalConfig, R2Config, ReaperConfig},
    error::AppError,
    lock::LockManager,
    r2::R2Client,
};

fn test_config() -> Option<Config> {
    let account_id = std::env::var("WHIRLWIND_TEST_R2_ACCOUNT_ID").ok()?;
    let access_key_id = std::env::var("WHIRLWIND_TEST_R2_ACCESS_KEY_ID").ok()?;
    let secret_access_key = std::env::var("WHIRLWIND_TEST_R2_SECRET_ACCESS_KEY").ok()?;
    let bucket = std::env::var("WHIRLWIND_TEST_R2_BUCKET").ok()?;
    Some(Config {
        r2: R2Config {
            account_id,
            access_key_id,
            secret_access_key,
            bucket,
        },
        local: LocalConfig {
            working_dir: std::path::PathBuf::from("/tmp/whirlwind-test"),
        },
        reaper: ReaperConfig {
            binary_path: std::path::PathBuf::from("/usr/bin/reaper"),
        },
        identity: IdentityConfig {
            user: "test-user".to_string(),
            machine: "test-machine".to_string(),
        },
    })
}

macro_rules! skip_without_r2 {
    ($config:expr) => {
        match $config {
            Some(c) => c,
            None => {
                println!("SKIP — R2 env vars not set");
                return;
            }
        }
    };
}

#[tokio::test]
async fn integration_lock_acquire_release_roundtrip() {
    let config = skip_without_r2!(test_config());
    let config = Arc::new(config);
    let r2 = Arc::new(R2Client::new(&config).await.expect("R2 client init"));
    let lm = LockManager::new(Arc::clone(&r2), Arc::clone(&config));
    let project = "integration-test-lock";

    // Clean up any leftover lock from prior run
    let _ = lm.release(project).await;

    // Acquire
    let guard = lm.acquire(project).await.expect("acquire lock");
    println!("Lock acquired for '{}'", project);

    // Try to acquire again — same identity means SelfLock, not LockContention.
    let result = lm.acquire(project).await;
    assert!(
        matches!(result, Err(AppError::SelfLock { .. })),
        "Expected SelfLock, got {:?}",
        result
    );

    // Drop guard — releases lock via LockGuard::drop
    drop(guard);
    println!("Lock released");

    // Acquire again after release
    let _guard2 = lm.acquire(project).await.expect("re-acquire after release");
    // Cleanup
    let _ = lm.release(project).await;
}

#[tokio::test]
async fn integration_push_pull_roundtrip() {
    let config = skip_without_r2!(test_config());
    let config = Arc::new(config);
    let r2 = Arc::new(R2Client::new(&config).await.expect("R2 client init"));
    let sync_engine = whirlwind::sync::SyncEngine::new(Arc::clone(&r2));

    let project = "integration-test-push-pull";

    // Create temp source dir with test files
    let src_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        src_dir.path().join("test.rpp"),
        b"fake reaper project content",
    )
    .unwrap();
    std::fs::create_dir(src_dir.path().join("audio")).unwrap();
    std::fs::write(
        src_dir.path().join("audio/track1.wav"),
        b"fake audio data 1234567890",
    )
    .unwrap();

    // Push
    let push_summary = sync_engine
        .push(project, src_dir.path())
        .await
        .expect("push");
    assert_eq!(push_summary.files_uploaded, 2);
    println!("Pushed {} files", push_summary.files_uploaded);

    // Pull to different dir
    let dst_dir = tempfile::tempdir().expect("tempdir");
    let pull_summary = sync_engine
        .pull(project, dst_dir.path())
        .await
        .expect("pull");
    assert_eq!(pull_summary.files_downloaded, 2);

    // Verify contents
    let rpp = std::fs::read(dst_dir.path().join("test.rpp")).unwrap();
    assert_eq!(rpp, b"fake reaper project content");
    let audio = std::fs::read(dst_dir.path().join("audio/track1.wav")).unwrap();
    assert_eq!(audio, b"fake audio data 1234567890");
    println!("Pull verified — file contents match");
}
