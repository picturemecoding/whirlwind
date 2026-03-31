# TDD: Four Bug Fixes — Track Matching, Mic Offsets, Media Paths, Session RPP Discovery

**Status**: Ready for implementation
**Last updated**: 2026-03-30
**Related files**: `src/new.rs`, `src/project.rs`, `src/session.rs`, `src/main.rs`,
`tests/rpp_parsing_test.rs`, `tests/fixtures/episode-base-template.RPP`

---

## 1. Problem Statement

Four independent bugs exist in the `whirlwind new` and `whirlwind session` workflows. All four
have been reported from real usage. Integration tests must be written first, using the real
`tests/fixtures/episode-base-template.RPP` fixture, before any fix is applied (test-first).

**Command path summary:**
- **Bugs 1, 2, and 3** are all in the `new` command / RPP template-building path:
  `src/new.rs` and `src/project.rs`.
- **Bug 4** is in the `session` command path: `src/session.rs`.

### Bug 1 — Track-name-to-wav-file matching broken for `mike`

The track named `mike` in the template does not receive audio when `whirlwind new` is run with
a Riverside-format filename for mike. The matching pattern `*_mike_*.wav` should match
`riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav` but does not.

### Bug 2 — Inserted mic tracks start at position 0 instead of the correct intro-relative offset

When audio is inserted into the `erik` or `mike` template tracks via `set_track_item`, the
generated `<ITEM>` block always sets `POSITION 0`. Mic audio must start at the point where the
intro track ends, minus a 2-second overlap — so that mic audio fades in as the intro fades out.

The required start offset is computed dynamically from the template's intro track:

```
mic_start = intro_POSITION + intro_LENGTH - 2.0
```

This value must **not** be hardcoded. `run_new` (or a helper it calls) must:
1. Parse the intro track's `POSITION` and `LENGTH` values from the downloaded template.
2. Compute `mic_start = intro_pos + intro_length - 2.0`.
3. Pass that value into `set_track_item` / `item_block` for every mic track (erik, mike, and any
   optional guest track).

### Bug 3 — Intro/outro wav paths are relative, not absolute; `init` does not ask for their location

The template at `tests/fixtures/episode-base-template.RPP` contains:
```
FILE "Media/intro-only.wav"
FILE "Media/outro-only.wav"
```
These are **relative paths**. When two collaborators open the same `.rpp` file from different
working directories, Reaper resolves relative paths from the `.rpp` file's location. This works
only if both machines have the identical directory structure. The fix requires:

1. During `whirlwind init`, ask the user where `intro-only.wav` and `outro-only.wav` are stored.
2. Store those absolute paths in `config.toml` under a new `[media]` section.
3. When `whirlwind new` processes the template, rewrite the `FILE` paths for intro and outro
   tracks to use `config.working_dir/Media/intro-only.wav` (and outro). The canonical form is
   `<working_dir>/Media/<filename>` — the filename is preserved but the directory is always
   `working_dir + /Media`.

### Bug 4 — `session` command opens Reaper with the wrong path (directory name instead of `.rpp` file)

In `src/session.rs`, the `.rpp` path is constructed as:
```rust
let rpp_path = local_dir.join(format!("{}.rpp", project));
```
This hardcodes `<episode-name>.rpp` as the filename. In practice the `.rpp` file inside the
episode directory is the episode name but with an `.RPP` (uppercase) extension — as Reaper saves
files. Additionally, if a user has a file with a slightly different name, the wrong path is passed
to Reaper and Reaper opens saying "project not found."

The fix: after pulling, scan the episode directory for exactly one file matching `*.rpp` or
`*.RPP` (case-insensitive; either extension). Open that file. If zero or more than one are found,
return a clear error.

### Success criteria

- All four integration tests in `tests/rpp_parsing_test.rs` pass against the real fixture.
- `just check` passes (no new clippy warnings, no fmt issues).
- No regressions in existing unit tests in `src/project.rs` and `src/new.rs`.

---

## 2. Context and Prior Art

### Template fixture (the ground truth)

`tests/fixtures/episode-base-template.RPP` contains four track blocks relevant to this TDD:

| Track | NAME attribute | Has existing `<ITEM>`? | Item POSITION |
|---|---|---|---|
| Bus track | `""` (empty) | No | — |
| `erik` | `NAME erik` (unquoted) | No | — |
| `mike` | `NAME mike` (unquoted) | No | — |
| `intro-only` | `NAME intro-only` | Yes | `0` |
| `outro-only` | `NAME outro-only` | Yes | `3492.5505063931737` |

The intro-only item has `POSITION 0` and `LENGTH 17.82993197278912`. The correct mic start offset
is therefore `0 + 17.82993197278912 - 2.0 = 15.82993197278912` seconds. This value must be
derived by parsing the intro track at runtime — see Bug 2 description and Phase C below.

### `parse_name_value` — Bug 1 root cause

The `parse_name_value` function in `src/project.rs` (line 127–134) parses track `NAME` lines.
It correctly handles both quoted form (`NAME "mike"`) and unquoted form (`NAME mike`). The
fixture uses the **unquoted** form. The existing unit test
`set_track_item_handles_unquoted_track_name` already covers this path and passes.

The actual failure for Bug 1 is therefore in `match_track_config` in `src/new.rs`. The pattern
is `*_mike_*.wav` (from `TrackConfig`). The filename is:
```
riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav
```
This filename contains **spaces**. The `glob::Pattern::matches` function is used (not
`glob::Pattern::matches_path`). The `glob` crate treats spaces as literal characters in the
pattern — the issue is not spaces per se. Investigation is required: the existing unit test
`match_track_config_returns_correct_track_for_mike` in `src/new.rs` (lines 483–491) tests this
exact filename and asserts it matches. If that test passes, the bug may instead be triggered by
how `discover_audio_files` forms the filename string (including spaces causing filesystem
iteration issues), or by an off-by-one in filename splitting. The integration test against the
real fixture will expose the actual failure path.

### `item_block` — Bug 2 root cause

In `src/project.rs`, `item_block` (line 18–28) is the function that generates the `<ITEM>` text
inserted into a named track. It unconditionally writes `POSITION 0`:
```rust
format!(
    "    <ITEM\n      POSITION 0\n      ...
```
There is no parameter to supply a starting position offset. `set_track_item` calls `item_block`
directly; it does not read any existing POSITION from the template track.

The fix requires adding a `position_secs: f64` parameter to `item_block` and threading an
offset value through `set_track_item`. The caller (`run_new` in `src/new.rs`) is responsible for
computing `mic_start` from the intro track before calling `set_track_item`.

### `FILE` path in template — Bug 3 root cause

The template's intro and outro `SOURCE WAVE` blocks contain relative `FILE` paths:
```
FILE "Media/intro-only.wav"
FILE "Media/outro-only.wav"
```
The `whirlwind new` command downloads the template, performs track manipulation, and writes the
output `.rpp`. It does not currently rewrite these `FILE` paths. The fix requires:

1. A new `[media]` section (or fields) in `Config` to hold `intro_wav` and `outro_wav` absolute
   paths.
2. A new `project::set_source_file` function (or a rewrite of the existing FILE line for a named
   track) that replaces the relative path with an absolute path rooted at
   `config.working_dir/Media/`.
3. The `run_init` function in `src/main.rs` must prompt for these paths.

### `.rpp` file discovery — Bug 4 root cause

`src/session.rs` line 45:
```rust
let rpp_path = local_dir.join(format!("{}.rpp", project));
```
This assumes the `.rpp` file is lowercase `.rpp` and named exactly after the project. Reaper
saves files with `.RPP` (uppercase) extension by default on macOS. The fix: after pull, scan
the episode directory for files ending in `.rpp` or `.RPP`, assert exactly one exists, and use
that path.

---

## 3. Architecture and System Design

All four fixes are self-contained. They touch different modules and can be implemented in parallel
by different engineers after the integration test skeleton is established (Phase A).

```
tests/rpp_parsing_test.rs    (Phase A — tests first, all four stubs)
         │
         ├── Bug 1 fix → src/new.rs (match_track_config investigation + fix)
         │                src/project.rs (no change likely)
         │                [new command / template-building path]
         │
         ├── Bug 2 fix → src/project.rs (item_block + set_track_item signatures)
         │                src/new.rs (parse intro track, compute mic_start, pass to set_track_item)
         │                [new command / template-building path]
         │
         ├── Bug 3 fix → src/config.rs (new MediaConfig struct + fields)
         │                src/main.rs (run_init: add prompts; run_new: rewrite FILE paths)
         │                src/project.rs (new set_source_file function)
         │                [new command / template-building path]
         │
         └── Bug 4 fix → src/session.rs (find_rpp_file helper + use it)
                          [session command path]
```

No new crate dependencies are required for any of the four fixes.

---

## 4. Data Models and Storage

### Bug 3: new `MediaConfig` in `src/config.rs`

A new optional section is needed in `Config` to store the absolute paths the user provides
during `init`:

```toml
[media]
intro_wav = "/Users/alice/podcast/Media/intro-only.wav"
outro_wav = "/Users/alice/podcast/Media/outro-only.wav"
```

Corresponding Rust struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaConfig {
    pub intro_wav: PathBuf,
    pub outro_wav: PathBuf,
}
```

Added to `Config` as:
```rust
#[serde(default)]
pub media: Option<MediaConfig>,
```

The field is `Option<MediaConfig>` so existing configs without a `[media]` section continue to
deserialize. When absent during `whirlwind new`, the FILE paths are left unchanged (no rewrite),
and a warning is printed directing the user to run `whirlwind init` to configure media paths.

The absolute path stored in config does NOT need to match `working_dir/Media/` — the user may
keep their media files anywhere. What the test must validate is that the resulting `.rpp`
contains an **absolute path** whose **parent directory** matches `config.working_dir.join("Media")`.
This is the canonical output location, not the storage location. During `init`, the user provides
the source path; during `new`, the tool writes the canonical `working_dir/Media/<filename>` form.

---

## 5. API Contracts

### Bug 2: `item_block` signature change (`src/project.rs`)

Current:
```rust
fn item_block(file_path: &str, duration_secs: f64) -> String
```

New:
```rust
fn item_block(file_path: &str, duration_secs: f64, position_secs: f64) -> String
```

`set_track_item` signature also changes to accept `position_secs`:

Current:
```rust
pub fn set_track_item(rpp: &str, track_name: &str, file_path: &str, duration_secs: f64) -> String
```

New:
```rust
pub fn set_track_item(
    rpp: &str,
    track_name: &str,
    file_path: &str,
    duration_secs: f64,
    position_secs: f64,
) -> String
```

All call sites in `src/new.rs` must pass the correct offset. For mic tracks (erik, mike, guest),
the caller is responsible for supplying `mic_start` (computed from the intro track — see Bug 2
description and Phase C). For the `outro` track, `set_item_position` is already used separately
— no change to `set_item_position`.

### Bug 3: `set_source_file` new function (`src/project.rs`)

```rust
/// Rewrite the FILE path inside the `<SOURCE WAVE>` block of a named track's `<ITEM>`.
/// Returns the string unchanged if the track or item is not found.
pub fn set_source_file(rpp: &str, track_name: &str, new_file_path: &str) -> String
```

This function locates the named track, then the first `<ITEM>` inside it, then the
`<SOURCE WAVE>` block inside the item, then replaces the `FILE "..."` line. It does not change
the item position, length, or any other attribute.

### Bug 4: `find_rpp_file` helper (`src/session.rs`)

```rust
/// Scan `dir` for exactly one .rpp or .RPP file (case-insensitive extension check).
/// Returns the path if exactly one is found.
/// Returns AppError::Other if zero or more than one are found.
fn find_rpp_file(dir: &Path) -> Result<PathBuf, AppError>
```

---

## 6. Migration and Rollout Strategy

All four changes are additive or narrowly scoped:

- Bug 1: behavior fix, no data model change. Existing `.rpp` files are unaffected.
- Bug 2: `set_track_item` gains a new parameter. All existing call sites are in `src/new.rs`
  (one call per discovered audio file). The call site passes the desired offset. No stored data
  changes.
- Bug 3: `Config` gains an optional `[media]` section. Existing configs that omit `[media]`
  continue to work — `whirlwind new` prints a warning and leaves FILE paths as-is. No migration
  of existing `.rpp` files is required; the fix only affects newly generated files.
- Bug 4: The `.rpp` path discovery is purely local and runtime. No stored data changes.

No database migrations. No R2 schema changes. No breaking changes to the CLI surface.

---

## 7. Risks and Open Questions

### RESOLVED: exact offset value for Bug 2

The required mic track start offset is:

```
mic_start = intro_POSITION + intro_LENGTH - 2.0
```

Concretely, using the fixture values (`POSITION 0`, `LENGTH 17.82993197278912`):

```
mic_start = 0 + 17.82993197278912 - 2.0 = 15.82993197278912
```

This value must **not** be hardcoded. `run_new` (or a dedicated helper it calls) must parse the
intro track's `POSITION` and `LENGTH` from the downloaded template at runtime, compute `mic_start`,
and pass it into `set_track_item` for every mic track (erik, mike, guest). The `-2.0` constant
(the overlap duration in seconds) may be hardcoded as a named constant.

### Risk: spaces in filenames and glob matching (Bug 1)

Riverside audio filenames contain spaces (e.g., `_the cohost_`, `_picture_me coding_`). The
`glob` crate's `Pattern::matches` treats spaces as literal characters and should match. However
`glob` on some platforms may behave differently. The investigation step must include running the
existing unit test in a debug build to confirm it actually passes, then tracing the integration
test failure path precisely.

### Risk: `set_track_item` signature is a breaking change for all callers (Bug 2)

If any external code (outside `src/new.rs`) calls `set_track_item`, the signature change will
cause a compile error. Currently only `src/new.rs` calls it. Confirm this with a grep before
implementing.

### Risk: `find_rpp_file` must tolerate mixed-case on case-insensitive filesystems (Bug 4)

macOS HFS+ and APFS are case-insensitive by default. A file named `ep-42.RPP` will be returned
by a directory scan regardless of casing. The implementation should normalize extension
comparison to lowercase (`to_lowercase()`) rather than matching literal `.RPP` and `.rpp`
separately, to be correct on case-sensitive Linux filesystems as well.

---

## 8. Testing Strategy

### Structure

All integration tests live in `tests/rpp_parsing_test.rs`. They are pure in-memory string
operations using the fixture file — no R2 calls, no spawned processes, no temp directories.
The test file imports directly from the library crate.

Tests read the fixture once:
```rust
const TEMPLATE: &str = include_str!("fixtures/episode-base-template.RPP");
```

The `WAV_FILES` constant already defined at the top of the file can be used directly in tests.

### Test 1 — Bug 1: `mike` track receives audio from Riverside-format filename

**Test name**: `test_mike_track_matches_riverside_filename`

**Setup**: Use `TEMPLATE` as the base. Apply `set_track_item` with track name `"mike"` and the
filename `"riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav"`.

**Assertions**:
- The returned string contains `FILE "riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav"`.
- The returned string contains `LENGTH` with a positive non-zero value (use a known test
  duration, e.g., `3600.0`).
- The `mike` track block in the output contains exactly one `<ITEM>`.
- The `<FXCHAIN>` for the `mike` track is preserved (confirm `<FXCHAIN>` appears inside the
  `mike` track block).

**What this tests**: that `set_track_item("mike", ...)` against the real fixture, where the
template uses the unquoted form `NAME mike`, correctly locates the track and inserts the item.
If this test fails, Bug 1 is reproduced. If it passes, Bug 1 must be elsewhere (in
`match_track_config` + `glob` interaction — see below).

**Test name**: `test_match_track_config_mike_riverside_filename`

**Setup**: Construct the `TrackConfig` vec with pattern `*_mike_*.wav`. Call `match_track_config`
with `"riverside_mike_the cohost_raw-audio_picture_me coding_0242.wav"`.

**Assertions**:
- Result is `Some(tc)` where `tc.track == "mike"`.

**What this tests**: that glob pattern matching correctly handles filenames with spaces. If this
test fails, the bug is in `match_track_config` / `glob::Pattern::matches`.

### Test 2 — Bug 2: Inserted mic tracks start at the correct offset

**Test name**: `test_mic_track_item_position_is_intro_end_minus_two`

**Setup**: Parse `POSITION` and `LENGTH` from the `intro-only` track in `TEMPLATE`. Compute
`mic_start = intro_pos + intro_length - 2.0` (expected: `15.82993197278912`). Call
`set_track_item` with `track_name = "erik"`, a test filename, duration `3600.0`, and
`position_secs = mic_start`.

**Assertions**:
- The output contains `POSITION 15.82993197278912` (or the value computed above) inside the
  erik track's `<ITEM>`.
- The output does NOT contain `POSITION 0` inside the erik track's `<ITEM>` block.

**Test name**: `test_outro_position_is_preserved_and_mic_position_is_offset`

**Setup**: Compute `mic_start` from the intro track as above. Apply `set_track_item` for both
`erik` and `mike` with `position_secs = mic_start`. Then apply `set_item_position` for `outro`
to a computed project-end position.

**Assertions**:
- The `erik` item has `POSITION 15.82993197278912` (or the dynamically computed value).
- The `mike` item has `POSITION 15.82993197278912` (or the dynamically computed value).
- The `outro` item has the project-end-derived POSITION (not `3492.55...` from the template).
- The `intro-only` item still has `POSITION 0` (untouched).

### Test 3 — Bug 3: Intro/outro FILE paths use absolute working_dir/Media/ prefix

**Test name**: `test_intro_file_path_is_rewritten_to_absolute`

**Setup**: Use a synthetic `working_dir = PathBuf::from("/Users/alice/podcast")`. Call
`set_source_file(template, "intro-only", "/Users/alice/podcast/Media/intro-only.wav")`.

**Assertions**:
- The output contains `FILE "/Users/alice/podcast/Media/intro-only.wav"`.
- The output does NOT contain `FILE "Media/intro-only.wav"` (the relative form is gone).
- The intro item's `POSITION`, `LENGTH`, `FADEOUT`, and `FADEIN` attributes are unchanged
  (verify at least one: `FADEOUT 4 12.95786172096238`).

**Test name**: `test_outro_file_path_is_rewritten_to_absolute`

**Setup**: Same working_dir. Call `set_source_file(template, "outro-only", "/Users/alice/podcast/Media/outro-only.wav")`.

**Assertions**:
- Output contains `FILE "/Users/alice/podcast/Media/outro-only.wav"`.
- Output does NOT contain `FILE "Media/outro-only.wav"`.
- The outro item's `POSITION 3492.5505063931737` is unchanged.

**Test name**: `test_intro_outro_paths_use_working_dir_media_prefix`

**Setup**: Programmatically construct the canonical paths using:
```rust
let working_dir = PathBuf::from("/Users/alice/podcast");
let expected_intro = working_dir.join("Media").join("intro-only.wav");
let expected_outro = working_dir.join("Media").join("outro-only.wav");
```

**Assertions**:
- The string representation of `expected_intro` is an absolute path.
- `expected_intro.parent().unwrap()` equals `working_dir.join("Media")`.
- (Structural: the parent directory is exactly `working_dir + /Media`, not a user-supplied
  arbitrary directory.)

This last test validates the path construction rule regardless of the user's machine path.

### Test 4 — Bug 4: `session` finds the `.RPP` file by scanning the directory

**Test name**: `test_find_rpp_file_finds_uppercase_extension`

**Setup**: Create a temporary directory. Write one file named `ep-42.RPP` into it. Call
`find_rpp_file(&tmpdir)`.

**Assertions**:
- Returns `Ok(path)` where `path.file_name() == "ep-42.RPP"`.

**Test name**: `test_find_rpp_file_finds_lowercase_extension`

**Setup**: Create a temporary directory. Write one file named `ep-42.rpp` into it.

**Assertions**:
- Returns `Ok(path)` where `path.file_name() == "ep-42.rpp"`.

**Test name**: `test_find_rpp_file_errors_on_zero_rpp_files`

**Setup**: Create a temporary directory with no `.rpp` or `.RPP` files (only `.wav` files).

**Assertions**:
- Returns `Err(AppError::Other(_))`.
- Error message mentions "no .rpp file found" (or similar).

**Test name**: `test_find_rpp_file_errors_on_multiple_rpp_files`

**Setup**: Create a temporary directory. Write two files: `ep-42.rpp` and `ep-42-old.RPP`.

**Assertions**:
- Returns `Err(AppError::Other(_))`.
- Error message mentions "multiple .rpp files" (or similar).

**Note**: Tests 4a–4d require actual temp directories. Use `tempfile::tempdir()` (already a
dev-dependency given its use in `tests/integration_test.rs`). Confirm `tempfile` is in
`[dev-dependencies]` in `Cargo.toml` before using it.

### Edge cases to cover

- `set_track_item` with a filename containing spaces must not corrupt surrounding FXCHAIN lines.
- `set_source_file` on a track with no `<ITEM>` must return the input unchanged (no panic).
- `find_rpp_file` in a directory containing only subdirectories (no files) must return an error.

---

## 9. Implementation Phases

### Phase A — Integration tests (prerequisite for all other phases)

**Complexity**: Small
**Files**: `tests/rpp_parsing_test.rs`
**Dependencies**: None (can start immediately)

Write all integration tests described in Section 8. Every test should initially **fail** or
**not compile** (because the APIs being tested do not yet have the correct signatures). After
Phase A is complete, Phase B–E can be worked in parallel.

Concrete actions:
1. Add `const TEMPLATE: &str = include_str!("fixtures/episode-base-template.RPP");` at the top.
2. Add all test functions as stubs with `todo!()` bodies, then fill in assertions.
3. Confirm `just test` shows the expected failures.

### Phase B — Bug 1: Glob matching investigation and fix

**Complexity**: Small (likely a one-line fix or confirmed working)
**Files**: `src/new.rs` (primary), possibly `src/project.rs`
**Command path**: `new` command / template-building path
**Dependencies**: Phase A tests must exist

Steps:
1. Run the existing unit test `match_track_config_returns_correct_track_for_mike` to confirm
   whether it passes.
2. Run the new integration test `test_match_track_config_mike_riverside_filename`.
3. If the unit test passes but the integration test fails, the bug is in how the filename is
   formed by `discover_audio_files` (filesystem encoding, NFC/NFD normalization on macOS) or
   how `set_track_item` parses the track NAME in the real template.
4. If both tests fail, the bug is in `glob::Pattern::matches` with spaces.
5. Apply the minimal fix: if `glob::Pattern::matches` is the culprit, consider switching to
   `glob::Pattern::matches_path` or a simpler `str::contains`-based fallback for the name
   segment matching.

### Phase C — Bug 2: Mic tracks start at correct position

**Complexity**: Medium (signature change propagates through call chain, plus intro parsing)
**Files**: `src/project.rs`, `src/new.rs`
**Command path**: `new` command / template-building path
**Dependencies**: Phase A (integration test must exist)

Steps:
1. Add `position_secs: f64` parameter to `item_block` in `src/project.rs`.
2. Add `position_secs: f64` parameter to `set_track_item` in `src/project.rs`.
3. In `run_new` (or a dedicated helper it calls) in `src/new.rs`:
   a. After downloading the template, parse the `intro-only` track's `POSITION` and `LENGTH`
      values from the template string.
   b. Compute `mic_start = intro_pos + intro_length - 2.0`. The `2.0` overlap constant may be
      defined as a named constant (e.g., `MIC_INTRO_OVERLAP_SECS: f64 = 2.0`).
   c. Pass `mic_start` as `position_secs` to every `set_track_item` call for mic tracks (erik,
      mike, and any optional guest track).
4. Update all `set_track_item` call sites in `src/new.rs` to pass the offset (mic tracks get
   `mic_start`; any other tracks that do not need an offset pass `0.0`).
5. Update all existing unit tests in `src/project.rs` that call `set_track_item` to pass `0.0`
   (maintaining backward-compatible behavior for those tests).

### Phase D — Bug 3: Intro/outro absolute paths

**Complexity**: Medium (touches config, init prompt, project.rs, and new.rs)
**Files**: `src/config.rs`, `src/main.rs`, `src/project.rs`, `src/new.rs`
**Command path**: `new` command / template-building path (config written during `init`)
**Dependencies**: Phase A (integration tests must exist)

Steps:
1. Add `MediaConfig` struct and `Option<MediaConfig>` field to `Config` in `src/config.rs`.
2. Add two prompts to `run_init` in `src/main.rs`:
   - "Path to intro wav file (will be stored as Media/intro-only.wav in your project)"
   - "Path to outro wav file (will be stored as Media/outro-only.wav in your project)"
3. Implement `set_source_file(rpp: &str, track_name: &str, new_file_path: &str) -> String`
   in `src/project.rs`.
4. In `run_new` in `src/new.rs`, after the template is downloaded and before writing the output,
   call `set_source_file` for `"intro-only"` and `"outro-only"` if `config.media` is `Some`.
   If `config.media` is `None`, print a warning: "Warning: media paths not configured. Run
   `whirlwind init` to set intro/outro paths. FILE paths in the output .rpp will be relative."
5. Write unit tests for `set_source_file` in `src/project.rs`.

### Phase E — Bug 4: RPP file discovery in session

**Complexity**: Small
**Files**: `src/session.rs`
**Command path**: `session` command path
**Dependencies**: Phase A (integration tests must exist)

Steps:
1. Implement `find_rpp_file(dir: &Path) -> Result<PathBuf, AppError>` in `src/session.rs`.
2. Replace the hardcoded `local_dir.join(format!("{}.rpp", project))` line with
   `find_rpp_file(&local_dir)?`.
3. The function scans `local_dir` entries, filters to files where
   `extension().to_lowercase() == "rpp"`, collects into a `Vec`, and:
   - If `len() == 1`: return the single path.
   - If `len() == 0`: return `Err(AppError::Other(format!("No .rpp file found in {}", dir.display())))`.
   - If `len() > 1`: return `Err(AppError::Other(format!("Multiple .rpp files found in {}; expected exactly one", dir.display())))`.

### Phase summary and parallelism

```
Phase A (tests)
    └── unblocks all of:
          Phase B (Bug 1) — independent of C, D, E
          Phase C (Bug 2) — independent of B, D, E
          Phase D (Bug 3) — independent of B, C, E
          Phase E (Bug 4) — independent of B, C, D
```

All of Phase B, C, D, E can be implemented concurrently after Phase A.

### Estimated complexity

| Phase | Bug | Complexity | Primary files |
|---|---|---|---|
| A | All (tests) | Small | `tests/rpp_parsing_test.rs` |
| B | Bug 1 (glob match) | Small | `src/new.rs` |
| C | Bug 2 (position offset) | Medium | `src/project.rs`, `src/new.rs` |
| D | Bug 3 (media paths) | Medium | `src/config.rs`, `src/main.rs`, `src/project.rs`, `src/new.rs` |
| E | Bug 4 (session rpp) | Small | `src/session.rs` |

---

## Appendix: Fixture Reference

Key values from `tests/fixtures/episode-base-template.RPP` for use in test assertions:

| Attribute | Value |
|---|---|
| `intro-only` item POSITION | `0` |
| `intro-only` item LENGTH | `17.82993197278912` |
| Computed `mic_start` (`intro_pos + intro_length - 2.0`) | `15.82993197278912` |
| `intro-only` item FADEOUT onset | `17.82993197278912 - 12.95786172096238 = 4.87207025182674s` |
| `outro-only` item POSITION | `3492.5505063931737` |
| `outro-only` item LENGTH | `19.31600907029497` |
| Relative FILE for intro | `Media/intro-only.wav` |
| Relative FILE for outro | `Media/outro-only.wav` |
| `erik` track GUID | `{BC53243F-D046-9F4B-9981-46FCD3EF7945}` |
| `mike` track GUID | `{8548330D-4653-E448-AE76-98B083AB5B20}` |
