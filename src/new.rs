use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{
    config::{Config, TrackConfig},
    error::AppError,
    lock::LockManager,
    metadata::MetadataManager,
    project,
    r2::R2Client,
    sync::SyncEngine,
};

/// Parse `--assign track=file` pairs into a filename→track map.
///
/// Entries that do not contain `=` are silently skipped.
fn parse_assign(assigns: &[String]) -> HashMap<String, String> {
    assigns
        .iter()
        .filter_map(|s| {
            let (track, file) = s.split_once('=')?;
            Some((file.to_string(), track.to_string()))
        })
        .collect()
}

/// Find the first `[[new.tracks]]` entry whose glob pattern matches `filename`.
fn match_track_config<'a>(filename: &str, tracks: &'a [TrackConfig]) -> Option<&'a TrackConfig> {
    tracks.iter().find(|t| {
        t.pattern
            .as_deref()
            .and_then(|p| glob::Pattern::new(p).ok())
            .map(|p| p.matches(filename))
            .unwrap_or(false)
    })
}

/// Resolve which template track (if any) an audio file should be inserted into.
///
/// Priority: CLI `--assign` > config pattern > None (plain track).
fn resolve_track<'a>(
    filename: &str,
    assign_map: &'a HashMap<String, String>,
    config_tracks: &'a [TrackConfig],
) -> Option<&'a str> {
    if let Some(track) = assign_map.get(filename) {
        return Some(track.as_str());
    }
    match_track_config(filename, config_tracks).map(|t| t.track.as_str())
}

const AUDIO_EXTENSIONS: &[&str] = &["wav", "aiff", "mp3", "flac", "ogg"];

/// How many seconds before the intro ends the mic tracks should start.
const MIC_TRACK_LEAD_IN_SECS: f64 = 2.0;

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
/// files, match against configured tracks or explicit `--assign` overrides,
/// insert tracks, set the project end marker, and push.
pub async fn run_new(
    episode: &str,
    template_name: Option<String>,
    trim_seconds: Option<f64>,
    dry_run: bool,
    assign: Vec<String>,
    config: Arc<Config>,
    r2: Arc<R2Client>,
) -> Result<(), AppError> {
    // Resolve trim_seconds: CLI arg → config value → 0.0
    let resolved_trim = trim_seconds
        .or_else(|| config.new.as_ref().map(|n| n.trim_seconds))
        .unwrap_or(0.0);

    // Resolve template name: CLI arg → config → "default"
    let resolved_template = template_name
        .or_else(|| config.new.as_ref().and_then(|n| n.default_template.clone()))
        .unwrap_or_else(|| "default".to_string());

    // Template extensions may be .RPP or .rpp depending on how the user uploaded it, so we'll try both.
    let template_upper_ext = R2Client::template_key(&resolved_template, true);
    let template_lower_ext = R2Client::template_key(&resolved_template, false);

    // Local paths.
    let local_dir = config.local.working_dir.join(episode);
    let rpp_path = local_dir.join(format!("{}.rpp", episode));

    // CLI assignments look like `--assign track-name=file.wav` and take priority over config patterns.
    let assign_map = parse_assign(&assign);
    // Configured tracks from `[[new.tracks]]` in the config file; we fallback to these.
    let config_tracks = config
        .new
        .as_ref()
        .map(|n| n.tracks.as_slice())
        .unwrap_or(&[]);

    // Dry run: display plan without any network or filesystem side effects.
    if dry_run {
        println!("Dry run: whirlwind new {}", episode);
        println!("  Template: {} (from R2)", template_upper_ext);
        println!();

        if local_dir.exists() {
            let audio_files = discover_audio_files(&local_dir)?;
            println!("  Audio files found in {}:", local_dir.display());
            for (filename, duration) in &audio_files {
                let label = match resolve_track(filename, &assign_map, config_tracks) {
                    Some(track) => format!("track: {}", track),
                    None => "no match — plain track".to_string(),
                };
                println!(
                    "    {}  ({})  → {}",
                    filename,
                    fmt_duration(*duration),
                    label
                );
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
                "  Outro-only starts 3s before end = {}",
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

    // Download template .rpp from R2.
    // Reaper saves files as `.RPP` (uppercase) by default, but users may also
    // upload as `.rpp`. Try uppercase first, then lowercase on 404.
    let template_bytes = {
        match r2.get_object_bytes(&template_upper_ext).await {
            Ok(b) => b,
            Err(AppError::NotFound { .. }) => r2
                .get_object_bytes(&template_lower_ext)
                .await
                .map_err(|e| {
                    if matches!(e, AppError::NotFound { .. }) {
                        AppError::Other(format!(
                            "Template '{}' not found in R2. Upload it with:\n  \
                            aws s3 cp your-template.rpp s3://{}/{} \\\n  \
                            --endpoint-url https://<account_id>.r2.cloudflarestorage.com",
                            resolved_template, r2.bucket, template_lower_ext,
                        ))
                    } else {
                        e
                    }
                })?,
            Err(e) => return Err(e),
        }
    };

    let template_str = std::str::from_utf8(&template_bytes)
        .map_err(|e| {
            AppError::Other(format!(
                "Template '{}' is not valid UTF-8: {}",
                resolved_template, e
            ))
        })?
        .to_string();

    // Ensure the local episode directory exists.
    std::fs::create_dir_all(&local_dir).map_err(|e| AppError::IoError {
        path: local_dir.display().to_string(),
        source: e,
    })?;

    // Discover audio files and build the .rpp.
    let audio_files = discover_audio_files(&local_dir)?;

    // Compute mic track start: intro_length - MIC_TRACK_LEAD_IN_SECS.
    let intro_length = project::get_track_item_length(&template_str, "intro-only");
    if intro_length == 0.0 {
        eprintln!(
            "Warning: intro-only track not found in template — mic tracks will start at position 0"
        );
    }
    let mic_start = (intro_length - MIC_TRACK_LEAD_IN_SECS).max(0.0);

    // Rewrite intro/outro FILE paths to absolute paths under working_dir/Media/.
    let media_dir = config.local.working_dir.join("Media");
    let intro_abs = media_dir.join("intro-only.wav");
    let outro_abs = media_dir.join("outro-only.wav");
    let intro_abs_str = intro_abs.to_string_lossy().into_owned();
    let outro_abs_str = outro_abs.to_string_lossy().into_owned();

    // actuall rpp project is loaded as a string here.
    let mut rpp = template_str.clone();
    rpp = project::set_source_file(&rpp, "intro-only", &intro_abs_str);
    rpp = project::set_source_file(&rpp, "outro-only", &outro_abs_str);
    let mut plain_tracks = Vec::new();

    for (filename, duration) in &audio_files {
        match resolve_track(filename, &assign_map, config_tracks) {
            Some(track) => {
                let updated = project::set_track_item(&rpp, track, filename, *duration, mic_start);
                if updated != rpp {
                    println!("  {} → track: {}", filename, track);
                    rpp = updated;
                } else {
                    eprintln!(
                        "  Warning: track '{}' not found in template for '{}' — adding as plain track",
                        track, filename
                    );
                    plain_tracks.push(project::build_plain_track(filename, *duration));
                }
            }
            None => {
                println!("  {} → no match — adding as plain track", filename);
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
    let rpp = project::set_item_position(&rpp, "outro-only", (project_end - 3.0).max(0.0));
    let rpp = project::set_end_marker(&rpp, project_end);

    // Write the .rpp file.
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

    // Acquire lock, push, release lock.
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
    use crate::config::TrackConfig;

    fn erik_mike_tracks() -> Vec<TrackConfig> {
        vec![
            TrackConfig {
                track: "erik".to_string(),
                pattern: Some("*_erik_*.wav".to_string()),
            },
            TrackConfig {
                track: "mike".to_string(),
                pattern: Some("*_mike_*.wav".to_string()),
            },
        ]
    }

    // -----------------------------------------------------------------------
    // parse_assign
    // -----------------------------------------------------------------------

    #[test]
    fn parse_assign_single_entry() {
        let assigns = vec!["erik=riverside_eriklongname_ep42.wav".to_string()];
        let map = parse_assign(&assigns);
        assert_eq!(
            map.get("riverside_eriklongname_ep42.wav")
                .map(|s| s.as_str()),
            Some("erik")
        );
    }

    #[test]
    fn parse_assign_multiple_entries() {
        let assigns = vec![
            "erik=file_erik.wav".to_string(),
            "mike=file_mike.wav".to_string(),
        ];
        let map = parse_assign(&assigns);
        assert_eq!(map.get("file_erik.wav").map(|s| s.as_str()), Some("erik"));
        assert_eq!(map.get("file_mike.wav").map(|s| s.as_str()), Some("mike"));
    }

    #[test]
    fn parse_assign_empty() {
        let map = parse_assign(&[]);
        assert!(map.is_empty());
    }

    #[test]
    fn parse_assign_skips_entries_without_equals() {
        let assigns = vec!["no-equals-here".to_string(), "track=file.wav".to_string()];
        let map = parse_assign(&assigns);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("file.wav"));
    }

    // -----------------------------------------------------------------------
    // match_track_config
    // -----------------------------------------------------------------------

    #[test]
    fn match_track_config_returns_correct_track_for_erik() {
        let tracks = erik_mike_tracks();
        let result = match_track_config("riverside_erik_aker_raw-audio_0242.wav", &tracks);
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "erik");
    }

    #[test]
    fn match_track_config_returns_correct_track_for_mike() {
        let tracks = erik_mike_tracks();
        let result = match_track_config(
            "riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav",
            &tracks,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "mike");
    }

    #[test]
    fn match_track_config_returns_none_for_unmatched_file() {
        let tracks = erik_mike_tracks();
        let result = match_track_config("ep42-guest-interview.wav", &tracks);
        assert!(result.is_none());
    }

    #[test]
    fn match_track_config_returns_none_for_empty_list() {
        let result = match_track_config("riverside_erik_aker_raw-audio_ep42.wav", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn match_track_config_first_match_wins() {
        let tracks = vec![
            TrackConfig {
                track: "erik".to_string(),
                pattern: Some("*_erik_*.wav".to_string()),
            },
            TrackConfig {
                track: "catch-all".to_string(),
                pattern: Some("*.wav".to_string()),
            },
        ];
        let result = match_track_config("riverside_erik_aker_ep42.wav", &tracks);
        assert!(result.is_some());
        assert_eq!(result.unwrap().track, "erik");
    }

    // -----------------------------------------------------------------------
    // resolve_track — CLI assign takes priority
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_track_cli_assign_overrides_config_pattern() {
        let tracks = erik_mike_tracks();
        let mut assign_map = HashMap::new();
        // Explicitly assign this file to "erik" even though it wouldn't match the glob
        assign_map.insert("oddname.wav".to_string(), "erik".to_string());
        let result = resolve_track("oddname.wav", &assign_map, &tracks);
        assert_eq!(result, Some("erik"));
    }

    #[test]
    fn resolve_track_falls_back_to_config_pattern() {
        let tracks = erik_mike_tracks();
        let assign_map = HashMap::new();
        let result = resolve_track("riverside_erik_aker_ep42.wav", &assign_map, &tracks);
        assert_eq!(result, Some("erik"));
    }

    #[test]
    fn resolve_track_returns_none_when_no_match() {
        let tracks = erik_mike_tracks();
        let assign_map = HashMap::new();
        let result = resolve_track("guest-interview.wav", &assign_map, &tracks);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // trim guard logic
    // -----------------------------------------------------------------------

    #[test]
    fn trim_guard_errors_when_trim_exceeds_max_duration() {
        let max_duration = 100.0_f64;
        let resolved_trim = 100.0_f64;
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
