use std::collections::HashSet;
use std::process;
use std::sync::Arc;

use clap::Parser;

use whirlwind::config::{Config, config_path};
use whirlwind::error::AppError;
use whirlwind::lock::LockManager;
use whirlwind::metadata::MetadataManager;
use whirlwind::r2::R2Client;
use whirlwind::sync::SyncEngine;

mod cli;
use cli::{Cli, Commands};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            if let Err(e) = run_init().await {
                eprintln!("{}", e);
                process::exit(e.exit_code());
            }
        }

        Commands::List => {
            let (config, r2) = load_config_and_r2().await;
            let metadata_manager = MetadataManager::new(Arc::clone(&r2));
            if let Err(e) = run_list(&config, &r2, &metadata_manager).await {
                eprintln!("{}", e);
                process::exit(e.exit_code());
            }
        }

        Commands::Pull { project, force: _ } => {
            let (config, r2) = load_config_and_r2().await;
            let sync_engine = Arc::new(SyncEngine::new(Arc::clone(&r2)));
            if let Err(e) = run_pull(&config, &sync_engine, &project).await {
                eprintln!("{}", e);
                process::exit(e.exit_code());
            }
        }

        Commands::Push { project, no_lock } => {
            let (config, r2) = load_config_and_r2().await;
            let lock_manager = LockManager::new(Arc::clone(&r2), Arc::new(config.clone()));
            let sync_engine = Arc::new(SyncEngine::new(Arc::clone(&r2)));
            let metadata_manager = MetadataManager::new(Arc::clone(&r2));
            if let Err(e) = run_push(
                &config,
                &lock_manager,
                &sync_engine,
                &metadata_manager,
                &project,
                no_lock,
            )
            .await
            {
                eprintln!("{}", e);
                process::exit(e.exit_code());
            }
        }

        Commands::Status { project } => {
            println!("status command coming in Phase 3 (not yet implemented)");
            println!("Use `whirlwind list` to see all projects and lock status.");
            let _ = project;
        }

        Commands::Session { project } => {
            println!("session command coming in Phase 2 (not yet implemented)");
            println!("For now: pull manually, edit in Reaper, push manually.");
            let _ = project;
        }

        Commands::Unlock { project, force } => {
            println!("unlock command coming in Phase 3 (not yet implemented)");
            println!(
                "To manually break a lock: delete the lock file from your R2 bucket at locks/{}.lock",
                project
            );
            let _ = force;
        }
    }
}

// ---------------------------------------------------------------------------
// Shared setup helpers
// ---------------------------------------------------------------------------

/// Load config, construct R2Client, and exit on failure.
///
/// This is called by every command except `init`. Config-missing exits with
/// a hint to run `whirlwind init`.
async fn load_config_and_r2() -> (Config, Arc<R2Client>) {
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("{}", e);
        process::exit(1);
    });

    let r2 = R2Client::new(&config).await.unwrap_or_else(|e| {
        eprintln!("{}", e);
        process::exit(1);
    });

    (config, Arc::new(r2))
}

// ---------------------------------------------------------------------------
// init handler
// ---------------------------------------------------------------------------

async fn run_init() -> Result<(), AppError> {
    use dialoguer::{Confirm, Input, Password};

    let path = config_path();

    // If config already exists, prompt for overwrite.
    if path.exists() {
        let overwrite = Confirm::new()
            .with_prompt(format!(
                "Config already exists at {}. Overwrite?",
                path.display()
            ))
            .default(false)
            .interact()
            .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

        if !overwrite {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Collect R2 credentials.
    let account_id: String = Input::new()
        .with_prompt("R2 Account ID")
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    let access_key_id: String = Input::new()
        .with_prompt("R2 Access Key ID")
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    let secret_access_key: String = Password::new()
        .with_prompt("R2 Secret Access Key")
        .interact()
        .map_err(|e| AppError::ConfigInvalid(e.to_string()))?;

    let bucket: String = Input::new()
        .with_prompt("R2 Bucket Name")
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    // Collect identity.
    let default_user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let user: String = Input::new()
        .with_prompt("Your username (used in lock files and metadata)")
        .default(default_user)
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    let default_machine = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
    let machine: String = Input::new()
        .with_prompt("Machine name (hostname)")
        .default(default_machine)
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    // Collect local working directory.
    let default_working_dir = dirs::home_dir()
        .map(|h| h.join("podcast").to_string_lossy().into_owned())
        .unwrap_or_else(|| "~/podcast".to_string());
    let working_dir_str: String = Input::new()
        .with_prompt("Local working directory for projects")
        .default(default_working_dir)
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    // Collect Reaper binary path with platform-specific default.
    let default_reaper = if cfg!(target_os = "macos") {
        "/Applications/REAPER.app/Contents/MacOS/REAPER".to_string()
    } else if cfg!(target_os = "linux") {
        "/usr/bin/reaper".to_string()
    } else {
        String::new()
    };
    let reaper_binary_str: String = Input::new()
        .with_prompt("Reaper binary path")
        .default(default_reaper)
        .interact_text()
        .map_err(|e| AppError::Other(format!("prompt error: {}", e)))?;

    // Build and validate config.
    let config = Config {
        r2: whirlwind::config::R2Config {
            account_id,
            access_key_id,
            secret_access_key,
            bucket,
        },
        local: whirlwind::config::LocalConfig {
            working_dir: std::path::PathBuf::from(&working_dir_str),
        },
        reaper: whirlwind::config::ReaperConfig {
            binary_path: std::path::PathBuf::from(&reaper_binary_str),
        },
        identity: whirlwind::config::IdentityConfig { user, machine },
    };

    config.validate()?;

    // Test R2 connectivity before saving.
    let r2 = R2Client::new(&config).await?;
    r2.list_objects("")
        .await
        .map_err(|e| AppError::Other(format!("R2 connection test failed: {}", e)))?;

    // Save config only after a successful R2 test.
    config.save()?;

    println!(
        "Config written to {}. R2 connection: OK",
        config_path().display()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// list handler
// ---------------------------------------------------------------------------

async fn run_list(
    _config: &Config,
    r2: &R2Client,
    metadata_manager: &MetadataManager,
) -> Result<(), AppError> {
    use whirlwind::lock::{LockFile, is_stale};

    // Load metadata (empty on first run).
    let metadata = metadata_manager.load().await?;

    // List active locks.
    let lock_objects = r2.list_objects("locks/").await?;

    // Parse lock files: key is "<project>.lock" relative to "locks/".
    // We fetch each lock file body to get locked_by / locked_at.
    let mut lock_map: std::collections::HashMap<String, LockFile> =
        std::collections::HashMap::new();

    for obj in &lock_objects {
        // obj.key is relative to "locks/" prefix, e.g. "episode-47.lock"
        let project_name = obj
            .key
            .strip_suffix(".lock")
            .unwrap_or(&obj.key)
            .to_string();

        let full_key = format!("locks/{}", obj.key);
        match r2.get_object_bytes(&full_key).await {
            Ok(bytes) => {
                if let Ok(lock_file) = serde_json::from_slice::<LockFile>(&bytes) {
                    lock_map.insert(project_name, lock_file);
                }
            }
            Err(_) => {
                // Lock disappeared between list and get — skip.
            }
        }
    }

    // Build the combined set of project names from metadata + active locks.
    let all_projects: Vec<String> = {
        let mut set: HashSet<String> = metadata.projects.keys().cloned().collect();
        for name in lock_map.keys() {
            set.insert(name.clone());
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    };

    if all_projects.is_empty() {
        println!("No projects found. Use `whirlwind push <project>` to upload your first project.");
        return Ok(());
    }

    // -----------------------------------------------------------------------
    // Build rows then compute dynamic column widths.
    // -----------------------------------------------------------------------

    struct Row {
        project: String,
        status: String,
        locked_by: String,
        last_pushed_by: String,
        last_pushed_at: String,
    }

    let mut rows: Vec<Row> = Vec::new();

    for project in &all_projects {
        let (status, locked_by) = if let Some(lock) = lock_map.get(project) {
            let mut by = format!("{} ({})", lock.locked_by, lock.machine);
            if is_stale(lock) {
                by.push_str(" (stale?)");
            }
            ("locked".to_string(), by)
        } else {
            ("available".to_string(), "-".to_string())
        };

        let (last_pushed_by, last_pushed_at) = if let Some(entry) = metadata.projects.get(project) {
            let at = entry
                .last_pushed_at
                .format("%Y-%m-%d %H:%M UTC")
                .to_string();
            (entry.last_pushed_by.clone(), at)
        } else {
            ("-".to_string(), "-".to_string())
        };

        rows.push(Row {
            project: project.clone(),
            status,
            locked_by,
            last_pushed_by,
            last_pushed_at,
        });
    }

    // Compute column widths (minimum = header width).
    let col_project = rows
        .iter()
        .map(|r| r.project.len())
        .max()
        .unwrap_or(0)
        .max("PROJECT".len());
    let col_status = rows
        .iter()
        .map(|r| r.status.len())
        .max()
        .unwrap_or(0)
        .max("STATUS".len());
    let col_locked_by = rows
        .iter()
        .map(|r| r.locked_by.len())
        .max()
        .unwrap_or(0)
        .max("LOCKED BY".len());
    let col_last_pushed_by = rows
        .iter()
        .map(|r| r.last_pushed_by.len())
        .max()
        .unwrap_or(0)
        .max("LAST PUSHED BY".len());
    let col_last_pushed_at = rows
        .iter()
        .map(|r| r.last_pushed_at.len())
        .max()
        .unwrap_or(0)
        .max("LAST PUSHED AT".len());

    // Header.
    println!(
        "{:<width_p$}  {:<width_s$}  {:<width_lb$}  {:<width_lpb$}  {}",
        "PROJECT",
        "STATUS",
        "LOCKED BY",
        "LAST PUSHED BY",
        "LAST PUSHED AT",
        width_p = col_project,
        width_s = col_status,
        width_lb = col_locked_by,
        width_lpb = col_last_pushed_by,
    );

    // Separator line.
    println!(
        "{:-<width_p$}  {:-<width_s$}  {:-<width_lb$}  {:-<width_lpb$}  {:-<width_lpa$}",
        "",
        "",
        "",
        "",
        "",
        width_p = col_project,
        width_s = col_status,
        width_lb = col_locked_by,
        width_lpb = col_last_pushed_by,
        width_lpa = col_last_pushed_at,
    );

    // Data rows.
    for row in &rows {
        println!(
            "{:<width_p$}  {:<width_s$}  {:<width_lb$}  {:<width_lpb$}  {}",
            row.project,
            row.status,
            row.locked_by,
            row.last_pushed_by,
            row.last_pushed_at,
            width_p = col_project,
            width_s = col_status,
            width_lb = col_locked_by,
            width_lpb = col_last_pushed_by,
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// pull handler
// ---------------------------------------------------------------------------

async fn run_pull(
    config: &Config,
    sync_engine: &SyncEngine,
    project: &str,
) -> Result<(), AppError> {
    let local_dir = config.local.working_dir.join(project);

    std::fs::create_dir_all(&local_dir).map_err(|e| AppError::IoError {
        path: local_dir.display().to_string(),
        source: e,
    })?;

    println!("Pulling {}...\n", project);

    // sync.rs prints per-file lines and the summary; we just propagate errors.
    sync_engine.pull(project, &local_dir).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// push handler
// ---------------------------------------------------------------------------

async fn run_push(
    config: &Config,
    lock_manager: &LockManager,
    sync_engine: &SyncEngine,
    metadata_manager: &MetadataManager,
    project: &str,
    no_lock: bool,
) -> Result<(), AppError> {
    let local_dir = config.local.working_dir.join(project);

    if !local_dir.exists() {
        eprintln!(
            "Local directory '{}' does not exist. Run `whirlwind pull {}` first.",
            local_dir.display(),
            project
        );
        process::exit(1);
    }

    // Acquire lock unless --no-lock was passed.
    if no_lock {
        eprintln!(
            "WARNING: pushing without lock. Any concurrent changes by your collaborator will be silently overwritten."
        );
    }
    let _guard = if !no_lock {
        Some(lock_manager.acquire(project).await?)
    } else {
        None
    };

    println!("Pushing {}...\n", project);

    let summary = sync_engine.push(project, &local_dir).await.map_err(|e| {
        // Lock is still held (guard hasn't dropped yet). Inform the user.
        if !no_lock {
            eprintln!(
                "Upload failed. Lock retained — run `whirlwind push {}` to retry.",
                project
            );
        }
        e
    })?;

    // Record push metadata — best-effort, warn on errors.
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

    // _guard drops here, releasing the lock.
    Ok(())
}
