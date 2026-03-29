# TDD: `whirlwind new` — Templated Episode Project Creation

**Status**: Spec only — no implementation planned until Phase 3 is complete
**Phase**: 4 (post-Phase 3)
**Last updated**: 2026-03-28

---

## Problem Statement

Setting up a new Reaper project for a podcast episode involves the same manual steps every time:
1) create a project from the template (in object-storage at R2 `${bucket}/templates/${template-name}.rpp`), 2) copy each audio file as a track into existing host-mic track (with existing plugins, EQ, etc., staying the same), 3) set the outro at a few seconds before the host tracks end, and push to R2. This takes hours over time and is error-prone.

`whirlwind new <episode-name>` automates this setup. The episode directory may already exist and contain recorded audio files. The command produces a fully configured Reaper project ready to open and edit.

### What this command does

1. Downloads a Reaper project template from R2 (`${bucket}/templates/${template-name}.rpp`, example: `whirlwind/templates/episode-base-template.RPP`).
2. Discovers audio files already present in the episode directory
3. Inserts each audio file as a track in the project (remove existing similar-named (for host) track and insert similar-named track in same slot (with all plugins preserved))
4. Calculates project end point from track lengths minus a configurable trim offset
5. Pushes the resulting project to R2

### Explicit non-goals

- No audio processing — this tool manipulates project files only, not audio data
- No real-time Reaper integration — Reaper is not running during this command
- No plugin installation — plugins referenced in the template must already be installed on both machines
- No file deletion — consistent with the rest of whirlwind's no-deletion policy
- No handling of MIDI, video, or non-audio media items

### Acceptance criteria (Phase 4 complete)

- `whirlwind new ep-42` in a directory containing WAV files produces a `.rpp` file that opens
  correctly in Reaper with all audio files on tracks
- Tracks matching a filename pattern have been inserted into the right track
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
      FILE "audio/erik-mic.wav"
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

- **`POSITION`** — media item position in seconds from project start. For a podcast, all tracks (EXCEPT THE OUTRO!)
  start at position 0.

- **`LENGTH`** — media item length in seconds. Must be accurate or Reaper will show a truncated
  or extended item.

- **`FILE`** — path to the audio file, relative to the `.rpp` file location. Use forward slashes
  on all platforms. The path should be relative (e.g., `audio/erik-mic.wav`), not absolute.

- **Project end marker** — set via a `MARKER` line at the project root level:
  ```
  MARKER 1 <end_seconds> "End" 0 0 1 R {GUID} 0
  ```
  Reaper uses marker index 1 as the project end by convention (configurable, but standard).

---

## Template Design

Track-to-file matching is configured locally in `~/.config/whirlwind/config.toml` — there is
no separate `archetypes.toml` file in R2. This keeps all whirlwind configuration in one place
and eliminates a per-team R2 bootstrap step.

### Template structure

`templates/default.rpp` — a normal Reaper project with named tracks:
```
<TRACK {GUID}
  NAME "erik-mic"
  <FXCHAIN ... > (fully configured EQ + compression)
  ← NO <ITEM> block — tool inserts the episode audio here
>
<TRACK {GUID}
  NAME "mike-mic"
  <FXCHAIN ... > (host EQ chain)
  ← NO <ITEM> block — tool inserts the episode audio here
>
<TRACK {GUID}
  NAME "intro"
  <FXCHAIN ... > (music chain, different levels)
  <ITEM POSITION 0 ...>  ← kept as-is; tool never touches intro
    <SOURCE WAVE FILE "audio/intro.wav">
  >
>
<TRACK {GUID}
  NAME "outro"
  <FXCHAIN ... > (music chain, different levels)
  <ITEM ...>  ← POSITION updated to project_end - 3.0; FILE left as-is
    <SOURCE WAVE FILE "audio/outro.wav">
  >
>
```

### Matching logic

For each discovered audio file, matching proceeds in order (first match wins):

1. **CLI `--assign <track>=<file>`** — explicit per-run assignment; bypasses pattern matching entirely
2. **`[[new.tracks]]` pattern** in `config.toml` — glob pattern match against the filename
3. **No match** — file becomes a plain track (no FX chain); a notice is printed

This means a user can rely on patterns day-to-day and only use `--assign` when Riverside
generates a filename that doesn't match the configured pattern.

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

### Key design decision: update items in-place, never rebuild tracks

The template is treated as the source of truth for all track-level config (FX chains, EQ,
compression, fades, volume, pan). The tool only updates the parts that change per episode:

- **Host mic tracks** (erik, mike): have no `<ITEM>` in the template — the tool inserts one.
- **Intro track**: left completely untouched — audio, fades, and position are already correct.
- **Outro track**: only `POSITION` is updated to `project_end - 3.0` seconds.
- **Unmatched audio files**: appended as plain tracks with no FX chain.

This means `<FXCHAIN>` base64 blobs are **never extracted, copied, or reconstructed** —
they stay in the template untouched, eliminating the main source of corruption risk.

### `src/project.rs` public API

```rust
/// Insert an <ITEM> block into an existing named track (track must have no existing <ITEM>).
pub fn set_track_item(rpp: &str, track_name: &str, file_path: &str, duration_secs: f64) -> String;

/// Update the POSITION of the <ITEM> in a named track (used for outro placement).
pub fn set_item_position(rpp: &str, track_name: &str, position_secs: f64) -> String;

/// Append new plain TRACK blocks (no FX chain) before the closing `>` of the project root.
pub fn insert_tracks(rpp: &str, tracks: &[String]) -> String;

/// Set or replace the project end marker.
pub fn set_end_marker(rpp: &str, end_secs: f64) -> String;
```

### State machine approach

Track blocks are delimited by `<TRACK` and a matching `>` at the same nesting depth.
The parser maintains a depth counter: `<` increments, `>` decrements, and we're inside a track
block when depth > 0 after seeing `<TRACK`. Item insertion uses the same depth tracking to
find the correct closing `>` of the target track.

### Testing

Unit tests use small fixture strings — not full `.rpp` files. For example:

```rust
#[test]
fn inserts_item_into_empty_named_track() {
    let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {GUID}
  NAME "erik-mic"
  <FXCHAIN
    <VST "VST3: ReaEQ" reaEQ.vst3 0 "" >
  >
>
>"#;
    let result = set_track_item(rpp, "erik-mic", "audio/erik-ep42.wav", 3612.5);
    assert!(result.contains(r#"FILE "audio/erik-ep42.wav""#));
    assert!(result.contains("LENGTH 3612.5"));
    assert!(result.contains("<FXCHAIN")); // FX chain preserved
}
```

---

## Template Storage in R2

### Key conventions

| Resource | R2 Key |
|---|---|
| Default template | `templates/default.rpp` |
| Named template | `templates/<name>.rpp` |

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

[[new.tracks]]
track = "erik-mic"              # track name in the Reaper template
pattern = "*_erik_*.wav"        # glob pattern matched against the audio filename

[[new.tracks]]
track = "mike-mic"
pattern = "*_mike_*.wav"
```

- `tracks` is a list of `{ track, pattern }` entries. `pattern` is optional; if absent the entry
  can only be assigned via `--assign`.
- Use `#[serde(default)]` so existing configs without a `[new]` section continue to work.
- `trim_seconds` defaults to `0.0` if omitted. `tracks` defaults to an empty list.

Rust structs:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackConfig {
    pub track: String,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewConfig {
    pub default_template: Option<String>,
    #[serde(default)]
    pub trim_seconds: f64,
    #[serde(default)]
    pub tracks: Vec<TrackConfig>,
}
```

---

## CLI Surface

```
whirlwind new <episode-name> [OPTIONS]

Arguments:
  <episode-name>  Name of the episode (must match working_dir/<episode-name>/)

Options:
  --template <name>         Template to use (default: from config, else "default")
  --trim-seconds <secs>     Seconds to trim from project end (default: from config, else 0)
  --assign <TRACK=FILE>     Assign a specific file to a named track (repeatable)
  --dry-run                 Show what would happen without writing or pushing anything
  -h, --help                Print help
```

`--assign` can be repeated to assign multiple files:

```sh
whirlwind new ep-42 \
  --assign "erik-mic=riverside_ERIKLONGNAME_raw-audio_ep42.wav" \
  --assign "mike-mic=riverside_MIKELONGNAME_raw-audio_ep42.wav"
```

`--assign` takes precedence over any `[[new.tracks]]` pattern in the config.

### --dry-run output

```
Dry run: whirlwind new ep-42
  Template: templates/default.rpp (from R2)

  Audio files found in /Users/alice/podcast/episodes/ep-42:
    riverside_erik_aker_raw-audio_picture_me_coding_0241.wav  (58:32.4)  → track: erik-mic
    riverside_mike_cohost_raw-audio_picture_me_coding_0242.wav  (58:31.1)  → track: mike-mic
    ep-42-guest-name.wav  ( 1:00.0)  → no match — plain track

  Project end: 58:32.4 - 2.0s trim = 58:30.4
  Outro starts 3s before end! = 58:27.4
  Output: /Users/erewok/podcast/episodes/ep-42/ep-42.rpp
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
run_new(episode_name, template_name, trim_seconds, dry_run, assign: Vec<String>)
  ├── parse_assign(assign)  → HashMap<filename, track_name>
  ├── R2Client::get_object_bytes("templates/<name>.rpp")  → rpp: String  (used as-is)
  ├── discover_audio_files(local_dir)  → Vec<(filename, duration_secs)>
  ├── for each (filename, duration):
  │   ├── resolve_track(filename, &assign_map, config.new.tracks)
  │   │   1. assign_map.get(filename)           → explicit CLI assignment
  │   │   2. config tracks glob pattern match   → pattern-based assignment
  │   │   3. None                               → plain track
  │   ├── if Some(track): project::set_track_item(rpp, track, filename, duration)
  │   └── if None: collect into plain_tracks
  ├── project::insert_tracks(rpp, plain_tracks)
  ├── project_end = max(durations) - trim_seconds
  ├── project::set_item_position(rpp, "outro", project_end - 3.0)
  ├── project::set_end_marker(rpp, project_end)
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
| Track match ambiguity | Medium | A file matching multiple `[[new.tracks]]` patterns gets the first match. Document this in config comments. `--assign` bypasses all patterns and is unambiguous. |
| No audio files in directory | Low | Warn the user and prompt to continue (create an empty project) or abort. |
| Template not found in R2 | High | Requires `NotFound` error variant (known gap from architecture.md) to give a good error message. |
| `whirlwind template push` UX gap | Low | Users must use `aws s3 cp` to upload their template initially. A future `whirlwind template push <path>` command would close this. |

---

## Testing Strategy

### Unit tests (`src/project.rs`)

All pure functions are tested with small inline fixture strings:
- `set_track_item` — inserts `<ITEM>` into correct named track; `<FXCHAIN>` is preserved
- `set_track_item` — returns error (or unchanged) if track not found
- `set_item_position` — POSITION value updated in correct track; other tracks unaffected
- `insert_tracks` — plain tracks appear before final `>` in output; no `<FXCHAIN>` present
- `set_end_marker` — existing marker is replaced, not duplicated; inserted if absent

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
- Insert matching tracks from template
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

### Phase 4c — Track matching + FX chain application
Complexity: Medium-High

- `[[new.tracks]]` config entries with glob pattern matching
- `--assign <track>=<file>` CLI flag for explicit per-run overrides
- `resolve_track` logic: CLI assign → config pattern → plain track
- `project::set_track_item` inserts audio into matching template tracks
- Unmatched files become plain tracks

Acceptance: matched tracks have correct FX chains; unmatched tracks are plain but present.

### Phase 4d — Full format support + `--dry-run` + trim offset
Complexity: Low-Medium

- Non-WAV duration via `symphonia`
- `--trim-seconds` flag and config value wired up
- `--dry-run` output implemented
- `whirlwind template push` subcommand (optional, nice-to-have)

Acceptance: all format and UX requirements met; spec acceptance criteria above satisfied.
