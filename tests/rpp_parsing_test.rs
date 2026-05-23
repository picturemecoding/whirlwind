//! Integration tests for RPP template parsing and manipulation.
//!
//! These tests use the real `tests/fixtures/episode-base-template.RPP` fixture
//! to validate bug-fix behaviors before implementation begins (test-first).
//!
//! All tests are pure in-memory string operations or use temp dirs.
//! No R2 calls. No spawned processes.
//!
//! # Expected compile failures (until fixes are implemented)
//!
//! - `set_track_item` currently takes 4 args; these tests call the new 5-arg
//!   signature `(rpp, track_name, file_path, duration_secs, position_secs)`.
//!   All `set_track_item` calls will fail to compile until Bug 2 is fixed.
//!
//! - `project::set_source_file` does not exist yet.
//!   Bug 3 tests will fail to compile until it is added.
//!
//! - `session::find_rpp_file` does not exist / is not public yet.
//!   Bug 4 tests will fail to compile until it is added.

use pmc_whirlwind::{error::AppError, project, session};
use std::path::PathBuf;

const TEMPLATE: &str = include_str!("fixtures/episode-base-template.RPP");

/// Key fixture values from `tests/fixtures/episode-base-template.RPP`.
/// intro-only item: POSITION 0, LENGTH 17.82993197278912
/// mic_start = 0 + 17.82993197278912 - 2.0 = 15.82993197278912
const INTRO_LENGTH: f64 = 17.82993197278912;
const INTRO_POSITION: f64 = 0.0;
// The expected mic track start: intro ends at INTRO_POSITION + INTRO_LENGTH, minus 2s overlap.
const EXPECTED_MIC_START: f64 = INTRO_POSITION + INTRO_LENGTH - 2.0; // = 15.82993197278912
const OUTRO_ORIGINAL_POSITION: f64 = 3492.5505063931737;

// ---------------------------------------------------------------------------
// Bug 1 — Track matching: mike track receives Riverside-format filename
// ---------------------------------------------------------------------------

/// Assert that `set_track_item` against the real template correctly inserts
/// audio into the `mike` track when given a Riverside-format filename with spaces.
///
/// The template uses the unquoted form `NAME mike`. The filename contains spaces.
/// If track NAME parsing or track lookup is broken this test will fail.
///
/// NOTE: Calls the new 5-arg signature `(rpp, track, file, duration, position)`.
/// Will fail to compile until Bug 2 adds `position_secs` to `set_track_item`.
#[test]
fn test_mike_track_receives_riverside_filename() {
    let filename = "riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav";
    let duration = 3600.0_f64;

    let result = project::set_track_item(TEMPLATE, "mike", filename, duration, EXPECTED_MIC_START);

    assert!(
        result.contains(&format!("FILE \"{filename}\"")),
        "FILE path for mike's Riverside filename must appear in output:\n{result}"
    );
    assert!(
        result.contains("LENGTH 3600"),
        "LENGTH 3600 must appear in output:\n{result}"
    );
    assert_ne!(
        result, TEMPLATE,
        "Output must differ from template after inserting into mike track"
    );
    // The mike track's FXCHAIN must be preserved.
    assert!(
        result.contains("<FXCHAIN"),
        "FXCHAIN blocks must be preserved in output:\n{result}"
    );
}

/// Assert that `set_track_item` against the real template also works for
/// the `erik` track with a Riverside-format filename containing spaces.
///
/// NOTE: Calls the new 5-arg signature — will fail to compile until Bug 2 is fixed.
#[test]
fn test_erik_track_receives_riverside_filename() {
    let filename = "riverside_erik_aker_raw-audio_picture_me coding_0241.wav";
    let duration = 3600.0_f64;

    let result = project::set_track_item(TEMPLATE, "erik", filename, duration, EXPECTED_MIC_START);

    assert!(
        result.contains(&format!("FILE \"{filename}\"")),
        "FILE path for erik's Riverside filename must appear:\n{result}"
    );
    assert_ne!(
        result, TEMPLATE,
        "Output must differ from template after inserting into erik track"
    );
}

// ---------------------------------------------------------------------------
// Bug 2 — Mic track POSITION equals intro_LENGTH - 2.0
// ---------------------------------------------------------------------------

/// Assert that after `set_track_item` the inserted ITEM's POSITION equals
/// `intro_POSITION + intro_LENGTH - 2.0`, derived from the real template.
///
/// Concrete value: `0 + 17.82993197278912 - 2.0 = 15.82993197278912`.
/// This value must come from parsing the template at runtime — not hardcoded.
///
/// NOTE: Calls the new 5-arg signature — will fail to compile until Bug 2 is fixed.
#[test]
fn test_mic_track_item_position_is_intro_end_minus_two() {
    let filename = "riverside_erik_aker_raw-audio_picture_me coding_0241.wav";
    let duration = 3600.0_f64;

    let result = project::set_track_item(TEMPLATE, "erik", filename, duration, EXPECTED_MIC_START);

    let expected_position_str = format!("POSITION {EXPECTED_MIC_START}");
    assert!(
        result.contains(&expected_position_str),
        "POSITION {EXPECTED_MIC_START} must appear in erik's ITEM block:\n{result}"
    );
}

/// Assert that both erik and mike items get the correct mic start offset,
/// the outro gets an updated project-end position, and the intro POSITION
/// remains 0 (untouched).
///
/// NOTE: Calls the new 5-arg signature — will fail to compile until Bug 2 is fixed.
#[test]
fn test_outro_position_is_preserved_and_mic_position_is_offset() {
    let erik_file = "riverside_erik_aker_raw-audio_picture_me coding_0241.wav";
    let mike_file = "riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav";
    let duration = 3600.0_f64;
    let project_end_position = 3597.0_f64;

    let rpp = project::set_track_item(TEMPLATE, "erik", erik_file, duration, EXPECTED_MIC_START);
    let rpp = project::set_track_item(&rpp, "mike", mike_file, duration, EXPECTED_MIC_START);
    let rpp = project::set_item_position(&rpp, "outro-only", project_end_position);

    let expected_position_str = format!("POSITION {EXPECTED_MIC_START}");

    // Both mic tracks should have the computed start offset (at least 2 occurrences).
    let position_count = rpp.matches(&expected_position_str).count();
    assert!(
        position_count >= 2,
        "Expected POSITION {EXPECTED_MIC_START} at least twice (erik + mike), found {position_count}:\n{rpp}"
    );

    // Outro must have the project-end-derived position.
    assert!(
        rpp.contains(&format!("POSITION {project_end_position}")),
        "Outro must have updated POSITION {project_end_position}:\n{rpp}"
    );
    assert!(
        !rpp.contains(&format!("POSITION {OUTRO_ORIGINAL_POSITION}")),
        "Original outro POSITION {OUTRO_ORIGINAL_POSITION} must be replaced:\n{rpp}"
    );

    // The intro-only track must still have POSITION 0 (it was not touched).
    assert!(
        rpp.contains("POSITION 0"),
        "intro-only POSITION 0 must still be present in output:\n{rpp}"
    );
}

// ---------------------------------------------------------------------------
// Bug 3 — Intro/outro FILE paths must be relative to the episode directory
// ---------------------------------------------------------------------------

/// Assert that `set_source_file` rewrites the intro-only FILE path to whatever
/// path string is passed to it (relative path in this case).
///
/// `set_source_file` is a dumb string replacer — it writes exactly the path
/// given.  The caller (`run_new`) is responsible for computing relative paths.
#[test]
fn test_intro_file_path_is_rewritten_to_relative() {
    let rel_intro = "../Media/intro-only.wav";

    let result = project::set_source_file(TEMPLATE, "intro-only", rel_intro);

    assert!(
        result.contains(&format!("FILE \"{rel_intro}\"")),
        "Relative intro FILE path must appear in output:\n{result}"
    );
    // The intro item's FADEOUT must be unchanged.
    assert!(
        result.contains("FADEOUT 4 12.95786172096238"),
        "intro FADEOUT attribute must be preserved:\n{result}"
    );
    // The intro item's POSITION must still be 0.
    assert!(
        result.contains("POSITION 0"),
        "intro POSITION 0 must still be present:\n{result}"
    );
}

/// Assert that `set_source_file` rewrites the outro-only FILE path to whatever
/// path string is passed to it (relative path in this case).
#[test]
fn test_outro_file_path_is_rewritten_to_relative() {
    let rel_outro = "../Media/outro-only.wav";

    let result = project::set_source_file(TEMPLATE, "outro-only", rel_outro);

    assert!(
        result.contains(&format!("FILE \"{rel_outro}\"")),
        "Relative outro FILE path must appear in output:\n{result}"
    );
    // The outro item's POSITION must be unchanged.
    assert!(
        result.contains(&format!("POSITION {OUTRO_ORIGINAL_POSITION}")),
        "outro POSITION {OUTRO_ORIGINAL_POSITION} must be unchanged:\n{result}"
    );
}

/// Assert that the relative path from the episode directory to the shared
/// Media folder is `../Media/<file>`, and that this relative path is NOT
/// machine-specific (i.e. it does not contain the absolute working_dir prefix).
///
/// This documents the structural invariant that `run_new` relies on when
/// writing intro/outro FILE paths into the generated .rpp.
#[test]
fn test_intro_outro_paths_are_relative_to_episode_dir() {
    let working_dir = PathBuf::from("/Users/alice/podcast");
    let episode_dir = working_dir.join("ep96-database-history");

    // The relative paths that run_new writes into the .rpp.
    let intro_rel = "../Media/intro-only.wav";
    let outro_rel = "../Media/outro-only.wav";

    // These must NOT be absolute.
    assert!(
        !PathBuf::from(intro_rel).is_absolute(),
        "intro path written into .rpp must be relative, not absolute"
    );
    assert!(
        !PathBuf::from(outro_rel).is_absolute(),
        "outro path written into .rpp must be relative, not absolute"
    );

    // Joining the relative path onto the episode dir must reach working_dir/Media/.
    // We check the string contains the expected suffix since PathBuf::join does not
    // normalise ".." components without a filesystem call.
    let resolved_intro = episode_dir.join(intro_rel).to_string_lossy().into_owned();
    let resolved_outro = episode_dir.join(outro_rel).to_string_lossy().into_owned();
    assert!(
        resolved_intro.contains("Media/intro-only.wav"),
        "resolved intro path must contain Media/intro-only.wav: {resolved_intro}"
    );
    assert!(
        resolved_outro.contains("Media/outro-only.wav"),
        "resolved outro path must contain Media/outro-only.wav: {resolved_outro}"
    );

    // Most importantly: the path must not embed the machine-specific working_dir.
    let working_dir_str = working_dir.to_string_lossy();
    assert!(
        !intro_rel.contains(working_dir_str.as_ref()),
        "relative intro path must not embed the absolute working_dir prefix"
    );
    assert!(
        !outro_rel.contains(working_dir_str.as_ref()),
        "relative outro path must not embed the absolute working_dir prefix"
    );
}

// ---------------------------------------------------------------------------
// Bug 4 — Session RPP discovery: find_rpp_file helper
// ---------------------------------------------------------------------------

/// Assert that `find_rpp_file` returns the path of a single `.RPP` (uppercase) file.
///
/// NOTE: `session::find_rpp_file` does not exist / is not public yet.
/// Will fail to COMPILE until Bug 4 is implemented and the function is made public.
#[test]
fn test_find_rpp_file_finds_uppercase_extension() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    std::fs::write(tmpdir.path().join("ep-42.RPP"), b"<REAPER_PROJECT 0.1>")
        .expect("write RPP file");

    let result = session::find_rpp_file(tmpdir.path());

    assert!(result.is_ok(), "Expected Ok, got: {result:?}");
    let found = result.unwrap();
    assert_eq!(
        found.file_name().and_then(|n: &std::ffi::OsStr| n.to_str()),
        Some("ep-42.RPP"),
        "Expected filename ep-42.RPP, got: {found:?}"
    );
}

/// Assert that `find_rpp_file` returns the path of a single `.rpp` (lowercase) file.
///
/// NOTE: will fail to compile until Bug 4 is implemented.
#[test]
fn test_find_rpp_file_finds_lowercase_extension() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    std::fs::write(tmpdir.path().join("ep-42.rpp"), b"<REAPER_PROJECT 0.1>")
        .expect("write rpp file");

    let result = session::find_rpp_file(tmpdir.path());

    assert!(result.is_ok(), "Expected Ok, got: {result:?}");
    let found = result.unwrap();
    assert_eq!(
        found.file_name().and_then(|n: &std::ffi::OsStr| n.to_str()),
        Some("ep-42.rpp"),
        "Expected filename ep-42.rpp, got: {found:?}"
    );
}

/// Assert that `find_rpp_file` returns an error when no `.rpp` files are present.
///
/// NOTE: will fail to compile until Bug 4 is implemented.
#[test]
fn test_find_rpp_file_errors_on_zero_rpp_files() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    // Only a .wav file — no .rpp files.
    std::fs::write(tmpdir.path().join("audio.wav"), b"fake audio data").expect("write wav");

    let result = session::find_rpp_file(tmpdir.path());

    assert!(
        result.is_err(),
        "Expected Err when no .rpp files are present, got: {result:?}"
    );
    match result.unwrap_err() {
        AppError::Other(msg) => {
            let msg_lower = msg.to_lowercase();
            assert!(
                msg_lower.contains("rpp") || msg_lower.contains("no"),
                "Error message should mention rpp or no file: {msg}"
            );
        }
        other => panic!("Expected AppError::Other, got: {other:?}"),
    }
}

/// Assert that `find_rpp_file` returns an error when multiple `.rpp` files are present.
///
/// NOTE: will fail to compile until Bug 4 is implemented.
#[test]
fn test_find_rpp_file_errors_on_multiple_rpp_files() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    std::fs::write(tmpdir.path().join("ep-42.rpp"), b"<REAPER_PROJECT 0.1>")
        .expect("write first rpp");
    std::fs::write(tmpdir.path().join("ep-42-old.RPP"), b"<REAPER_PROJECT 0.1>")
        .expect("write second rpp");

    let result = session::find_rpp_file(tmpdir.path());

    assert!(
        result.is_err(),
        "Expected Err when multiple .rpp files are present, got: {result:?}"
    );
    match result.unwrap_err() {
        AppError::Other(msg) => {
            let msg_lower = msg.to_lowercase();
            assert!(
                msg_lower.contains("multiple") || msg_lower.contains("rpp"),
                "Error message should mention multiple or rpp: {msg}"
            );
        }
        other => panic!("Expected AppError::Other, got: {other:?}"),
    }
}
