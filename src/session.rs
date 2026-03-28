use std::process::Command;
use std::sync::Arc;

use crate::{
    config::Config, error::AppError, lock::LockManager, metadata::MetadataManager, r2::R2Client,
    sync::SyncEngine,
};

/// Run a full session: acquire lock, pull, launch Reaper, wait, push, release lock.
///
/// On push failure the lock is intentionally retained via `std::mem::forget` so
/// the user can retry with `whirlwind push <project>` or explicitly release
/// with `whirlwind unlock <project>`.
pub async fn run_session(
    project: &str,
    config: Arc<Config>,
    r2: Arc<R2Client>,
) -> Result<(), AppError> {
    // Step 1: Verify Reaper binary exists.
    if !config.reaper.binary_path.exists() {
        return Err(AppError::ReaperNotFound {
            path: config.reaper.binary_path.display().to_string(),
        });
    }

    let lm = LockManager::new(Arc::clone(&r2), Arc::clone(&config));
    let sync_engine = SyncEngine::new(Arc::clone(&r2));
    let metadata_manager = MetadataManager::new(Arc::clone(&r2));
    let local_dir = config.local.working_dir.join(project);

    // Step 2: Acquire lock (LockGuard auto-releases on drop via RAII).
    println!("Acquiring lock for {}...", project);
    let lock_guard = lm.acquire(project).await?;

    // Step 3: Pull latest files.
    println!("Pulling latest files...");
    std::fs::create_dir_all(&local_dir).map_err(|e| AppError::IoError {
        path: local_dir.display().to_string(),
        source: e,
    })?;
    sync_engine.pull(project, &local_dir).await?;
    // If pull fails, LockGuard drops here → lock released automatically.

    // Step 4: Launch Reaper.
    let rpp_path = local_dir.join(format!("{}.rpp", project));
    println!("Launching Reaper...");

    let mut child = Command::new(&config.reaper.binary_path)
        .arg(&rpp_path)
        .spawn()
        .map_err(|e| AppError::ReaperSpawnFailed(e.to_string()))?;
    // If spawn fails, LockGuard drops → lock released.

    println!(
        "Reaper launched (PID {}). Waiting for Reaper to exit...",
        child.id()
    );

    // Step 5: Wait for Reaper to exit.
    let status = child
        .wait()
        .map_err(|e| AppError::ReaperSpawnFailed(e.to_string()))?;
    println!("Reaper exited ({}). Pushing changes...", status);

    // Step 6: Push — always push regardless of Reaper exit code.
    let push_result = sync_engine.push(project, &local_dir).await;

    match push_result {
        Ok(summary) => {
            // Update metadata best-effort; warn but do not fail the session.
            if let Err(e) = metadata_manager
                .record_push(
                    project,
                    &config.identity.user,
                    summary.files_uploaded as u32,
                    summary.total_bytes,
                )
                .await
            {
                eprintln!("Warning: failed to update project metadata: {}", e);
            }

            // Step 7: Drop lock_guard → releases lock.
            drop(lock_guard);
            println!("Session complete. Lock released.");
            Ok(())
        }
        Err(e) => {
            // CRITICAL: retain lock on push failure so the user can retry.
            // std::mem::forget prevents LockGuard::drop from releasing the lock.
            std::mem::forget(lock_guard);
            eprintln!(
                "Reaper exited. Push failed: {}\n\n\
                Your lock on {} is still held. Your local changes are safe.\n\
                To retry:   whirlwind push {}\n\
                To give up: whirlwind unlock {}",
                e, project, project, project
            );
            Err(e)
        }
    }
}
