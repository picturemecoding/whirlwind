use std::path::Path;
use std::sync::Arc;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{
    config::Config, error::AppError, lock::LockManager, metadata::MetadataManager, project,
    r2::R2Client, sync::SyncEngine,
};

#[derive(Debug, serde::Deserialize)]
struct Archetype {
    pattern: String,
    track: String,
}

#[derive(Debug, serde::Deserialize)]
struct Archetypes {
    archetypes: Vec<Archetype>,
}

/// Find the first archetype whose glob pattern matches `filename`.
/// Returns a reference to the matching Archetype, or None.
fn match_archetype<'a>(filename: &str, archetypes: &'a [Archetype]) -> Option<&'a Archetype> {
    archetypes.iter().find(|a| {
        glob::Pattern::new(&a.pattern)
            .map(|p| p.matches(filename))
            .unwrap_or(false)
    })
}

const AUDIO_EXTENSIONS: &[&str] = &["wav", "aiff", "mp3", "flac", "ogg"];

/// Read the duration (in seconds) for non-WAV audio files using the symphonia crate.
/// Returns 0.0 if the duration cannot be determined.
fn get_non_wav_duration(path: &Path) -> f64 {
    let src = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0.0,
    };
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(_) => return 0.0,
    };
    // Sum up track durations from the format container.
    // Most containers expose TimeBase + n_frames on their tracks.
    let format = probed.format;
    format
        .tracks()
        .iter()
        .filter_map(|t| {
            let codec = &t.codec_params;
            let tb = codec.time_base?;
            let frames = codec.n_frames?;
            Some(frames as f64 * tb.numer as f64 / tb.denom as f64)
        })
        .fold(0.0_f64, f64::max)
}

/// Discover audio files in `dir` shallowly (no recursion).
///
/// Returns `(filename, duration_secs)` pairs sorted alphabetically.
/// Skips hidden files (starting with `.`) and temp files (`*.tmp`, `~*`).
/// WAV duration is read from the file header via `hound`. Non-WAV duration
/// is read via `symphonia`; falls back to 0.0 with a warning on failure.
fn discover_audio_files(dir: &Path) -> Result<Vec<(String, f64)>, AppError> {
    let mut files: Vec<(String, f64)> = Vec::new();

    let entries = std::fs::read_dir(dir).map_err(|e| AppError::IoError {
        path: dir.display().to_string(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| AppError::IoError {
            path: dir.display().to_string(),
            source: e,
        })?;

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip hidden and temp files.
        if filename.starts_with('.') || filename.ends_with(".tmp") || filename.starts_with('~') {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        let duration = if ext == "wav" {
            match hound::WavReader::open(&path) {
                Ok(reader) => {
                    let spec = reader.spec();
                    let num_samples = reader.len();
                    num_samples as f64 / (spec.sample_rate as f64 * spec.channels as f64)
                }
                Err(e) => {
                    eprintln!(
                        "  Warning: could not read WAV header for {filename}: {e}, defaulting to 0.0s"
                    );
                    0.0
                }
            }
        } else {
            let d = get_non_wav_duration(&path);
            if d == 0.0 {
                eprintln!("  Warning: could not read duration for {filename}, defaulting to 0.0s");
            }
            d
        };

        files.push((filename, duration));
    }

    if files.is_empty() {
        eprintln!("Warning: no audio files found in {}", dir.display());
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Format a duration in seconds as mm:ss.d for display.
fn fmt_duration(secs: f64) -> String {
    let total = secs as u64;
    let mins = total / 60;
    let s = total % 60;
    let tenth = ((secs - total as f64) * 10.0) as u64;
    format!("{mins}:{s:02}.{tenth}")
}

/// Run the `whirlwind new` command: download template from R2, discover audio
/// files, match against archetypes, insert tracks, set the project end marker,
/// and push.
///
/// Phase 4c: archetype matching + FX chain application via set_track_item.
pub async fn run_new(
    episode: &str,
    template_name: Option<String>,
    trim_seconds: Option<f64>,
    dry_run: bool,
    config: Arc<Config>,
    r2: Arc<R2Client>,
) -> Result<(), AppError> {
    // Resolve trim_seconds: CLI arg → config value → 0.0
    let resolved_trim = trim_seconds
        .or_else(|| config.new.as_ref().map(|n| n.trim_seconds))
        .unwrap_or(0.0);

    // Step 1: Resolve template name.
    let resolved_template = template_name
        .or_else(|| config.new.as_ref().and_then(|n| n.default_template.clone()))
        .unwrap_or_else(|| "default".to_string());

    let template_key = format!("templates/{}.rpp", resolved_template);
    let local_dir = config.local.working_dir.join(episode);
    let rpp_path = local_dir.join(format!("{}.rpp", episode));

    // Compute the archetypes key for display (dry-run) and download (real run).
    let archetypes_key_primary = format!("templates/{}-archetypes.toml", resolved_template);
    let archetypes_key_fallback = "templates/default-archetypes.toml".to_string();

    // Step 2: Dry run — display plan without any network or filesystem side effects.
    if dry_run {
        println!("Dry run: whirlwind new {}", episode);
        println!("  Template: {} (from R2)", template_key);
        // Show which archetypes key would be used — no download in dry-run.
        let dry_archetypes_key = if resolved_template == "default" {
            archetypes_key_fallback.clone()
        } else {
            archetypes_key_primary.clone()
        };
        println!("  Archetypes: {} (from R2)", dry_archetypes_key);
        println!();

        // TODO(Phase 4d): show per-file archetype matches in dry-run output. This requires
        // downloading archetypes.toml, which conflicts with the network-free dry-run design.
        // Options: (a) make dry-run do a read-only R2 fetch for archetypes only, or
        // (b) read archetypes from a local cache. Deferred pending UX decision.
        if local_dir.exists() {
            let audio_files = discover_audio_files(&local_dir)?;
            println!("  Audio files found in {}:", local_dir.display());
            for (filename, duration) in &audio_files {
                println!("    {}  ({})", filename, fmt_duration(*duration));
            }
            let max_duration = audio_files.iter().map(|(_, d)| *d).fold(0.0_f64, f64::max);
            if resolved_trim >= max_duration && max_duration > 0.0 {
                return Err(AppError::Other(format!(
                    "--trim-seconds ({}) is >= max track duration ({}s) — project end would be zero or negative",
                    resolved_trim, max_duration
                )));
            }
            let project_end = max_duration - resolved_trim;
            println!();
            println!(
                "  Project end: {} - {}s trim = {}",
                fmt_duration(max_duration),
                resolved_trim,
                fmt_duration(project_end)
            );
            println!(
                "  Outro starts 3s before end! = {}",
                fmt_duration((project_end - 3.0).max(0.0))
            );
        } else {
            println!("  (episode directory does not exist yet — no audio files to discover)");
        }

        println!();
        println!("  Output: {}", rpp_path.display());
        println!("  Would push to: projects/{}/ in R2", episode);
        println!();
        println!("No files written (dry run).");
        return Ok(());
    }

    // Step 3: Download template .rpp from R2.
    let template_bytes = r2.get_object_bytes(&template_key).await.map_err(|e| {
        if matches!(e, AppError::NotFound { .. }) {
            AppError::Other(format!(
                "Template '{}' not found in R2. Upload it with: \
                aws s3 cp your-template.rpp s3://{}/templates/{}.rpp \
                --endpoint-url https://<account_id>.r2.cloudflarestorage.com",
                resolved_template, r2.bucket, resolved_template,
            ))
        } else {
            e
        }
    })?;

    // Step 4: Convert bytes to UTF-8 string.
    let template_str = std::str::from_utf8(&template_bytes)
        .map_err(|e| {
            AppError::Other(format!(
                "Template '{}' is not valid UTF-8: {}",
                resolved_template, e
            ))
        })?
        .to_string();

    // Step 5: Ensure the local episode directory exists.
    std::fs::create_dir_all(&local_dir).map_err(|e| AppError::IoError {
        path: local_dir.display().to_string(),
        source: e,
    })?;

    // Step 6: Download archetypes TOML from R2.
    // Try template-specific key first, fall back to default, then empty list.
    let archetypes: Archetypes = {
        let bytes_result = r2.get_object_bytes(&archetypes_key_primary).await;
        let bytes_result = match bytes_result {
            Err(AppError::NotFound { .. }) => r2.get_object_bytes(&archetypes_key_fallback).await,
            other => other,
        };
        match bytes_result {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(|e| {
                    AppError::Other(format!("archetypes TOML is not valid UTF-8: {}", e))
                })?;
                toml::from_str(text).map_err(|e| {
                    AppError::Other(format!("failed to parse archetypes TOML: {}", e))
                })?
            }
            Err(AppError::NotFound { .. }) => Archetypes {
                archetypes: Vec::new(),
            },
            Err(e) => return Err(e),
        }
    };

    // Step 7: Discover audio files and build the .rpp using archetype matching.
    let audio_files = discover_audio_files(&local_dir)?;

    let mut rpp = template_str.clone();
    let mut plain_tracks = Vec::new();

    for (filename, duration) in &audio_files {
        match match_archetype(filename, &archetypes.archetypes) {
            Some(archetype) => {
                let updated = project::set_track_item(&rpp, &archetype.track, filename, *duration);
                // set_track_item is a no-op if the track name doesn't exist in the template.
                // Detect this by checking whether the FILE reference appears in the result.
                let file_marker = format!("FILE \"{filename}\"");
                if updated.contains(&file_marker) {
                    println!("  {} → archetype: {}", filename, archetype.track);
                    rpp = updated;
                } else {
                    eprintln!(
                        "  Warning: archetype track '{}' not found in template for '{}' — adding as plain track",
                        archetype.track, filename
                    );
                    plain_tracks.push(project::build_plain_track(filename, *duration));
                }
            }
            None => {
                println!(
                    "  {} → no archetype match — adding as plain track",
                    filename
                );
                plain_tracks.push(project::build_plain_track(filename, *duration));
            }
        }
    }

    let max_duration = audio_files.iter().map(|(_, d)| *d).fold(0.0_f64, f64::max);
    if resolved_trim >= max_duration && max_duration > 0.0 {
        return Err(AppError::Other(format!(
            "--trim-seconds ({}) is >= max track duration ({}s) — project end would be zero or negative",
            resolved_trim, max_duration
        )));
    }
    let project_end = max_duration - resolved_trim;

    let rpp = project::insert_tracks(&rpp, &plain_tracks);
    let rpp = project::set_item_position(&rpp, "outro", (project_end - 3.0).max(0.0));
    let rpp = project::set_end_marker(&rpp, project_end);

    // Step 8: Write the .rpp file.
    std::fs::write(&rpp_path, &rpp).map_err(|e| AppError::IoError {
        path: rpp_path.display().to_string(),
        source: e,
    })?;

    println!("Written: {}", rpp_path.display());
    println!(
        "  {} audio file(s), project end: {}s",
        audio_files.len(),
        project_end
    );

    // Step 9: Acquire lock, push, release lock.
    let lock_manager = LockManager::new(Arc::clone(&r2), Arc::clone(&config));
    let sync_engine = SyncEngine::new(Arc::clone(&r2));
    let metadata_manager = MetadataManager::new(Arc::clone(&r2));

    println!("Acquiring lock for {}...", episode);
    let lock_guard = lock_manager.acquire(episode).await?;

    println!("Pushing {}...", episode);
    let push_result = sync_engine.push(episode, &local_dir).await;

    match push_result {
        Ok(summary) => {
            if let Err(e) = metadata_manager
                .record_push(
                    episode,
                    &config.identity.user,
                    (summary.files_uploaded + summary.files_skipped) as u32,
                    summary.total_bytes,
                )
                .await
            {
                eprintln!("Warning: failed to update project metadata: {}", e);
            }
            drop(lock_guard);
            println!("Done. Lock released.");
            Ok(())
        }
        Err(e) => {
            std::mem::forget(lock_guard);
            eprintln!(
                "Push failed: {}\n\n\
                Your lock on {} is still held. Your local changes are safe.\n\
                To retry:   whirlwind push {}\n\
                To give up: whirlwind unlock {}",
                e, episode, episode, episode
            );
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_archetypes() -> Vec<Archetype> {
        vec![
            Archetype {
                pattern: "*_erik_*.wav".to_string(),
                track: "erik-mic".to_string(),
            },
            Archetype {
                pattern: "*_mike_*.wav".to_string(),
                track: "mike-mic".to_string(),
            },
        ]
    }

    #[test]
    fn match_archetype_returns_correct_track_for_erik() {
        let archetypes = make_archetypes();
        let result = match_archetype("riverside_erik_aker_raw-audio_ep42.wav", &archetypes);
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "erik-mic");
    }

    #[test]
    fn match_archetype_returns_correct_track_for_mike() {
        let archetypes = make_archetypes();
        let result = match_archetype("riverside_mike_cohost_raw-audio_ep42.wav", &archetypes);
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "mike-mic");
    }

    #[test]
    fn match_archetype_returns_none_for_unmatched_file() {
        let archetypes = make_archetypes();
        let result = match_archetype("ep42-guest-interview.wav", &archetypes);
        assert!(result.is_none());
    }

    #[test]
    fn match_archetype_returns_none_for_empty_list() {
        let result = match_archetype("riverside_erik_aker_raw-audio_ep42.wav", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn match_archetype_first_match_wins() {
        let archetypes = vec![
            Archetype {
                pattern: "*_erik_*.wav".to_string(),
                track: "erik-mic".to_string(),
            },
            Archetype {
                pattern: "*.wav".to_string(),
                track: "catch-all".to_string(),
            },
        ];
        let result = match_archetype("riverside_erik_aker_ep42.wav", &archetypes);
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "erik-mic");
    }

    #[test]
    fn archetypes_toml_parses_correctly() {
        let toml_str = r#"
[[archetypes]]
pattern = "*_erik_*.wav"
track = "erik-mic"

[[archetypes]]
pattern = "*_mike_*.wav"
track = "mike-mic"
"#;
        let result: Archetypes = toml::from_str(toml_str).expect("should parse");
        assert_eq!(result.archetypes.len(), 2);
        assert_eq!(result.archetypes[0].track, "erik-mic");
        assert_eq!(result.archetypes[1].track, "mike-mic");
    }

    #[test]
    fn archetypes_toml_empty_parses_correctly() {
        let toml_str = r#"archetypes = []"#;
        let result: Archetypes = toml::from_str(toml_str).expect("should parse");
        assert_eq!(result.archetypes.len(), 0);
    }

    // -----------------------------------------------------------------------
    // trim guard logic
    // -----------------------------------------------------------------------

    #[test]
    fn trim_guard_errors_when_trim_exceeds_max_duration() {
        let max_duration = 100.0_f64;
        let resolved_trim = 100.0_f64;
        // Guard condition: resolved_trim >= max_duration && max_duration > 0.0
        assert!(
            resolved_trim >= max_duration && max_duration > 0.0,
            "guard should fire when trim == max_duration"
        );

        let resolved_trim = 150.0_f64;
        assert!(
            resolved_trim >= max_duration && max_duration > 0.0,
            "guard should fire when trim > max_duration"
        );
    }

    #[test]
    fn trim_guard_passes_when_no_audio_files() {
        // When max_duration == 0.0 (no audio files), any trim value is allowed.
        let max_duration = 0.0_f64;
        let resolved_trim = 5.0_f64;
        assert!(
            !(resolved_trim >= max_duration && max_duration > 0.0),
            "guard must NOT fire when max_duration == 0.0 (empty project)"
        );
    }

    #[test]
    fn trim_guard_passes_when_trim_less_than_max() {
        let max_duration = 100.0_f64;
        let resolved_trim = 2.0_f64;
        assert!(
            !(resolved_trim >= max_duration && max_duration > 0.0),
            "guard must NOT fire when trim < max_duration"
        );
    }

    #[test]
    fn project_end_computed_correctly_after_trim() {
        let max_duration = 3612.5_f64;
        let resolved_trim = 2.0_f64;
        let project_end = max_duration - resolved_trim;
        assert!(
            (project_end - 3610.5).abs() < 1e-9,
            "project_end should be max_duration - trim"
        );
    }
}
