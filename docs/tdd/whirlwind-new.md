# TDD: `whirlwind new` — Templated Episode Project Creation

**Status**: Spec only — no implementation planned until Phase 3 is complete
**Phase**: 4 (post-Phase 3)
**Last updated**: 2026-03-28

---

## Problem Statement

Setting up a new Reaper project for a podcast episode involves the same manual steps every time:
create a project from the template, add each audio file as a track, apply the correct EQ/plugin
chain to each track based on what it is (host mic, guest mic, outro music, etc.), set the project
end point, and push to R2. This takes hours per episode and is error-prone.

`whirlwind new <episode-name>` automates this setup. The episode directory may already exist
and contain recorded audio files. The command produces a fully configured Reaper project ready
to open and edit.

### What this command does

1. Downloads a Reaper project template from R2
2. Discovers audio files already present in the episode directory
3. Inserts each audio file as a track in the project
4. Applies EQ and plugin chain from a matching archetype (by filename pattern) to each track
5. Calculates project end point from track lengths minus a configurable trim offset
6. Pushes the resulting project to R2

### Explicit non-goals

- No audio processing — this tool manipulates project files only, not audio data
- No real-time Reaper integration — Reaper is not running during this command
- No plugin installation — plugins referenced in the template must already be installed on both machines
- No file deletion — consistent with the rest of whirlwind's no-deletion policy
- No handling of MIDI, video, or non-audio media items

### Acceptance criteria (Phase 4 complete)

- `whirlwind new ep-42` in a directory containing WAV files produces a `.rpp` file that opens
  correctly in Reaper with all audio files on tracks
- Tracks matching a filename pattern have the correct FX chain applied
- Tracks not matching any pattern are present as plain tracks, not silently dropped
- The project end marker is set to `max(track_lengths) - trim_seconds`
- The resulting project is pushed to R2 and locked correctly

---

## Reaper .rpp Format

Reaper projects are a hierarchical text format. Each block is opened with `<KEYWORD` and closed
with `>`. Attributes are space-separated tokens on the opening line or on child lines.

### Track block

```
<TRACK {GUID}
  NAME "Host Mic"
  PEAKCOL 16576
  BEAT -1
  AUTOMODE 0
  VOLPAN 1 0 -1 -1 1
  MUTE 0
  <FXCHAIN
    SHOW 0
    LASTSEL 0
    DOCKED 0
    <VST "VST3: ReaEQ (Cockos)" reaEQ.vst3 0 "" ...
      BASE64BLOB==
    >
    <VST "VST3: ReaComp (Cockos)" reaComp.vst3 0 "" ...
      BASE64BLOB==
    >
  >
  <ITEM
    POSITION 0
    SNAPOFFSET 0
    LENGTH 3612.5
    LOOP 0
    ALLTAKES 0
    FADEIN 1 0.01 0 1 0 0
    FADEOUT 1 0.01 0 1 0 0
    MUTE 0 0
    <SOURCE WAVE
      FILE "audio/host-mic.wav"
    >
  >
>
```

### Critical observations

- **`<FXCHAIN>` plugin state is opaque base64** — VST/VST3 state blobs must be copied verbatim
  from the template. Never attempt to parse or modify them. This is the key reason the archetype
  approach copies entire `<FXCHAIN>` blocks from template tracks.

- **GUIDs** — every `<TRACK>` has a GUID. New tracks need freshly generated GUIDs to avoid
  collisions when Reaper loads the project. Generate with UUID v4, formatted as
  `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.

- **`POSITION`** — media item position in seconds from project start. For a podcast, all tracks
  start at position 0.

- **`LENGTH`** — media item length in seconds. Must be accurate or Reaper will show a truncated
  or extended item.

- **`FILE`** — path to the audio file, relative to the `.rpp` file location. Use forward slashes
  on all platforms. The path should be relative (e.g., `audio/host-mic.wav`), not absolute.

- **Project end marker** — set via a `MARKER` line at the project root level:
  ```
  MARKER 1 <end_seconds> "End" 0 0 1 R {GUID} 0
  ```
  Reaper uses marker index 1 as the project end by convention (configurable, but standard).

---

## Template Design

**Recommendation: Option B — separate `templates/archetypes.toml` in R2**

Three options were considered:

| Option | Description | Problem |
|---|---|---|
| A: Track name patterns | Template tracks named `ARCHETYPE:*-host.wav` | Track names appear in Reaper UI — glob patterns are confusing |
| **B: Separate TOML config** | `templates/archetypes.toml` maps patterns to track names in template | Clean separation; Reaper-stable; independently editable |
| C: Embedded `.rpp` comments | Special comment markers in the `.rpp` | Reaper may strip unknown comments on save |

### Option B design

`templates/default.rpp` — a normal Reaper project with named tracks for each archetype:
```
<TRACK {GUID}
  NAME "host-mic"
  <FXCHAIN ... > (fully configured EQ + compression)
>
<TRACK {GUID}
  NAME "guest-mic"
  <FXCHAIN ... > (guest EQ chain)
>
<TRACK {GUID}
  NAME "outro"
  <FXCHAIN ... > (music chain, different levels)
>
```

`templates/archetypes.toml` — maps filename glob patterns to template track names:
```toml
[[archetypes]]
pattern = "*-host*.wav"
track = "host-mic"

[[archetypes]]
pattern = "*-guest*.wav"
track = "guest-mic"

[[archetypes]]
pattern = "*-outro*"
track = "outro"
```

**Matching logic**: for each discovered audio file, test patterns in order (first match wins).
If no pattern matches, the file gets a plain track with no FX chain.

---

## Audio File Discovery

### Supported extensions

WAV (primary), AIFF, MP3, FLAC, OGG. Reaper supports all of these natively.

### Discovery rules

- Scan the episode directory **shallowly by default** (not recursive) — podcast audio is typically
  flat. Add `--recursive` flag if nested subdirectory support is needed.
- Sort alphabetically so track order is deterministic across machines.
- Skip hidden files (starting with `.`) and temp files (`*.tmp`, `~*`).
- Files matching no archetype pattern: include as **plain tracks** (no FX chain). Never silently
  drop files. Print a notice: `  (no archetype match — adding as plain track)`.
- If the directory contains no audio files: print a warning and offer to continue with an empty
  project or abort.

---

## Track Duration Calculation

### WAV files

Use the `hound` crate to read WAV headers:

```rust
let reader = hound::WavReader::open(path)?;
let spec = reader.spec();
let num_samples = reader.len(); // total samples (all channels interleaved)
let duration_secs = num_samples as f64 / (spec.sample_rate as f64 * spec.channels as f64);
```

This reads only the header — no audio data is loaded into memory.

### Non-WAV files

For AIFF: manual header parsing or the `symphonia` crate.
For MP3/FLAC/OGG: `symphonia` crate (pure Rust, no native deps).
Phase 4b can handle WAV only and treat non-WAV duration as 0 with a warning.

### End-of-project calculation

```
project_end = max(track_durations) - trim_seconds
```

Where `trim_seconds` is:
- CLI flag `--trim-seconds` (takes precedence)
- Config value `new.trim_seconds` (default: `0.0`)

A negative result (trim longer than all tracks) is an error.

---

## .rpp Manipulation Strategy

**Recommendation: targeted line-by-line state machine** rather than a full parser.

A naive angle-bracket parser fails because VST attribute lines contain unbalanced `<` characters
in base64 blobs. A full parser is a significant undertaking (500+ lines). The targeted approach
is ~150 lines, fully testable with small fixture snippets, and sufficient for this use case.

### `src/project.rs` public API

```rust
/// Parse a .rpp file and extract the FX chain block for a named track.
pub fn extract_fxchain(rpp: &str, track_name: &str) -> Option<String>;

/// Build a new TRACK block for a given audio file, with an optional FX chain.
pub fn build_track(
    file_path: &str,     // relative path to audio file
    track_name: &str,    // display name
    duration_secs: f64,
    fxchain: Option<&str>,
) -> String;

/// Insert track blocks before the closing `>` of the project root.
pub fn insert_tracks(rpp: &str, tracks: &[String]) -> String;

/// Set or replace the project end marker.
pub fn set_end_marker(rpp: &str, end_secs: f64) -> String;

/// Strip all existing TRACK blocks from a template (leaving project-level config intact).
pub fn strip_tracks(rpp: &str) -> String;
```

### State machine approach

Track blocks are delimited by `<TRACK` and a matching `>` at the same nesting depth.
The parser maintains a depth counter: `<` increments, `>` decrements, and we're inside a track
block when depth > 0 after seeing `<TRACK`. FX chain extraction uses the same depth tracking
within the track block.

### Testing

Unit tests use small fixture strings — not full `.rpp` files. For example:

```rust
#[test]
fn extracts_fxchain_from_named_track() {
    let rpp = r#"<TRACK {GUID}
  NAME "host-mic"
  <FXCHAIN
    <VST "VST3: ReaEQ" ... >
  >
>"#;
    let chain = extract_fxchain(rpp, "host-mic").unwrap();
    assert!(chain.contains("<FXCHAIN"));
    assert!(chain.contains("ReaEQ"));
}
```

---

## Template Storage in R2

### Key conventions

| Resource | R2 Key |
|---|---|
| Default template | `templates/default.rpp` |
| Named template | `templates/<name>.rpp` |
| Archetypes config | `templates/<name>-archetypes.toml` (falls back to `templates/default-archetypes.toml`) |

### Fallback logic

1. Use `--template <name>` if provided
2. Use `config.new.default_template` if set
3. Fall back to `"default"`

If the template key does not exist in R2: return a clear error:
`"Template 'default' not found in R2. Upload it with: aws s3 cp your-template.rpp s3://your-bucket/templates/default.rpp --endpoint-url https://<account_id>.r2.cloudflarestorage.com"`

Note: this requires distinguishing 404-Not-Found from other R2 errors — the `NotFound` error
variant gap (from the architecture spec) must be resolved before Phase 4 is implemented.

### How users upload their template

Initially: use `aws s3 cp` or the Cloudflare dashboard directly. A `whirlwind template push`
subcommand is a nice-to-have but not required for Phase 4.

---

## Config Additions

New optional section in `~/.config/whirlwind/config.toml`:

```toml
[new]
default_template = "default"    # template name to use (omit to use "default")
trim_seconds = 2.0              # trim this many seconds from project end
```

Use `#[serde(default)]` so existing configs without a `[new]` section continue to work.
`trim_seconds` defaults to `0.0` if omitted.

---

## CLI Surface

```
whirlwind new <episode-name> [OPTIONS]

Arguments:
  <episode-name>  Name of the episode (must match working_dir/<episode-name>/)

Options:
  --template <name>       Template to use (default: from config, else "default")
  --trim-seconds <secs>   Seconds to trim from project end (default: from config, else 0)
  --dry-run               Show what would happen without writing or pushing anything
  -h, --help              Print help
```

### --dry-run output

```
Dry run: whirlwind new ep-42
  Template: templates/default.rpp (from R2)
  Archetypes: templates/default-archetypes.toml (from R2)

  Audio files found in /Users/alice/podcast/episodes/ep-42:
    ep-42-host-mic.wav      (58:32.4)  → archetype: host-mic
    ep-42-guest-mic.wav     (58:31.1)  → archetype: guest-mic
    ep-42-outro.wav         ( 0:42.0)  → archetype: outro
    ep-42-room-tone.wav     ( 1:00.0)  → no archetype match — plain track

  Project end: 58:32.4 - 2.0s trim = 58:30.4
  Output: /Users/alice/podcast/episodes/ep-42/ep-42.rpp
  Would push to: projects/ep-42/ in R2

No files written (dry run).
```

---

## Architecture

### New module: `src/project.rs`

Pure functions only — no I/O, no async, no R2 client. Takes strings, returns strings. Fully
unit-testable without filesystem or network.

### New dependency

```toml
hound = "3"          # WAV header reading
glob = "0.3"         # filename pattern matching for archetypes
uuid = { version = "1", features = ["v4"] }  # GUID generation for new tracks
```

### `run_new` data flow

```
run_new(episode_name, template_name, trim_seconds, dry_run)
  ├── R2Client::get_object_bytes("templates/<name>.rpp")  → template_rpp: String
  ├── R2Client::get_object_bytes("templates/<name>-archetypes.toml")  → archetypes: Vec<Archetype>
  ├── discover_audio_files(local_dir)  → Vec<AudioFile { path, duration_secs }>
  ├── project::strip_tracks(template_rpp)  → base_rpp: String
  ├── for each audio_file:
  │   ├── match_archetype(audio_file, archetypes)  → Option<&Archetype>
  │   ├── if Some(archetype): project::extract_fxchain(template_rpp, archetype.track)
  │   └── project::build_track(file_path, track_name, duration, fxchain)
  ├── project::insert_tracks(base_rpp, tracks)
  ├── project::set_end_marker(rpp, max_duration - trim_seconds)
  ├── write rpp to local_dir/<episode_name>.rpp
  └── if !dry_run:
      ├── LockManager::acquire(episode_name)
      ├── SyncEngine::push(episode_name, local_dir)
      ├── MetadataManager::record_push(...)
      └── LockGuard drops → lock released
```

---

## Risks and Open Questions

| Risk | Severity | Notes |
|---|---|---|
| `.rpp` format changes across Reaper versions | Medium | Format has been stable for 10+ years but is undocumented. Parse only the specific sections needed; avoid assumptions about ordering. |
| Plugin chain portability | High | FX chains reference plugins by name and VST ID. If the guest editor doesn't have the same plugins installed, Reaper will show missing-plugin warnings. This is a user/workflow concern, not a tool concern — document it. |
| Sample rate mismatches | Low | Reaper handles sample rate conversion natively. Track duration calculation is still accurate (it reads the actual sample count from the header). |
| Archetype ambiguity | Medium | A file matching multiple patterns gets the first match. Document this clearly in the archetypes TOML. |
| No audio files in directory | Low | Warn the user and prompt to continue (create an empty project) or abort. |
| Template not found in R2 | High | Requires `NotFound` error variant (known gap from architecture.md) to give a good error message. |
| `whirlwind template push` UX gap | Low | Users must use `aws s3 cp` to upload their template initially. A future `whirlwind template push <path>` command would close this. |

---

## Testing Strategy

### Unit tests (`src/project.rs`)

All pure functions are tested with small inline fixture strings:
- `extract_fxchain` — finds named track, returns None for missing track
- `build_track` — output contains correct FILE path and LENGTH
- `insert_tracks` — tracks appear before final `>` in output
- `set_end_marker` — existing marker is replaced, not duplicated
- `strip_tracks` — no `<TRACK` blocks remain in output

### Duration tests

- WAV header parsing returns correct duration for known test files
- Zero-sample WAV returns 0.0, not NaN
- Trim offset larger than max duration returns an error

### Integration test

Manual end-to-end checklist (not automated):
1. Create an episode directory with 2-3 WAV files
2. Run `whirlwind new <name> --dry-run` and verify output matches expected
3. Run without `--dry-run`, open the resulting `.rpp` in Reaper
4. Verify track count, track names, FX chains present, project end marker correct

---

## Implementation Phases

### Phase 4a — Template download + empty project + push
Complexity: Low

- Download template from R2
- Strip all tracks from template
- Write empty project to local dir
- Push to R2 (acquires lock, uploads, releases)

Acceptance: `whirlwind new ep-42` creates a valid `.rpp` with no tracks and pushes it.

### Phase 4b — Audio file discovery + plain track insertion
Complexity: Medium

- `discover_audio_files`: WAV only initially
- `project::build_track` + `project::insert_tracks`
- Duration calculation via `hound`
- End marker set from max duration

Acceptance: all WAV files appear as tracks; project end is correct; no FX chains yet.

### Phase 4c — Archetype matching + FX chain application
Complexity: Medium-High

- `archetypes.toml` download and parsing
- `match_archetype` with glob matching
- `project::extract_fxchain` + chain injection into new tracks
- Unmatched files become plain tracks

Acceptance: matched tracks have correct FX chains; unmatched tracks are plain but present.

### Phase 4d — Full format support + `--dry-run` + trim offset
Complexity: Low-Medium

- Non-WAV duration via `symphonia`
- `--trim-seconds` flag and config value wired up
- `--dry-run` output implemented
- `whirlwind template push` subcommand (optional, nice-to-have)

Acceptance: all format and UX requirements met; spec acceptance criteria above satisfied.
