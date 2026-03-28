# Technical Design Document: whirlwind Core

**Project**: whirlwind
**Author**: Staff Engineer
**Date**: 2026-03-28
**Status**: Draft — awaiting implementation

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Context & Prior Art](#2-context--prior-art)
3. [Architecture & System Design](#3-architecture--system-design)
4. [Data Models & Storage](#4-data-models--storage)
5. [API Contracts](#5-api-contracts)
6. [Concurrency & Lock Protocol](#6-concurrency--lock-protocol)
7. [Sync Algorithm](#7-sync-algorithm)
8. [Session Command Flow](#8-session-command-flow)
9. [Risks & Open Questions](#9-risks--open-questions)
10. [Testing Strategy](#10-testing-strategy)
11. [Implementation Phases](#11-implementation-phases)

---

## 1. Problem Statement

### The Problem

Two podcast co-editors share a Reaper DAW project stored in Cloudflare R2. Each editing session is discrete: one person edits, saves, and is done. The other person then picks it up. The challenge is:

1. **Preventing simultaneous edits** — if both editors open Reaper at the same time and push, one will silently overwrite the other's work.
2. **Avoiding unnecessary re-uploads** — Reaper projects include audio files that can be gigabytes in size. Re-uploading everything every session is impractical.
3. **Reducing friction** — the workflow should be a single command for the 95% case, not a multi-step manual procedure.

### Constraints

- **Exactly 2 users**. No need for multi-tenant access control or team management.
- **No simultaneous editing**. The lock model is intentional and correct for this use case.
- **Cloudflare R2 as the backing store**. R2 is S3-compatible. It has no server-side compute, triggers, or native versioning. The `If-None-Match: *` conditional PUT must be the atomic primitive for locking — there is nothing else available.
- **Rust implementation**. The codebase is a Rust 2024 edition binary crate at scaffold stage. All crate choices must be compatible with the Rust async ecosystem (`tokio`).
- **CLI tool, not a daemon**. The tool is invoked explicitly; it does not run in the background.
- **Relative media paths required in Reaper**. This is a user-configuration prerequisite, not something `whirlwind` enforces or automates. It must be documented prominently.

### Acceptance Criteria by Phase

**Phase 1 complete when:**
- `whirlwind init` writes a valid `config.toml` and confirms R2 connectivity
- `whirlwind list` shows all projects from `metadata.json` with their lock status
- `whirlwind pull <project>` downloads all project files from R2 to the local working directory
- `whirlwind push <project>` acquires a lock, uploads all project files, updates `metadata.json`, and releases the lock
- Attempting to push while a lock is held by another user produces a clear error message naming the lock holder
- All commands work against a real R2 bucket

**Phase 2 complete when:**
- `whirlwind session <project>` executes the full pull → lock → launch Reaper → wait → push → unlock sequence
- Progress bars are shown for multi-file uploads and downloads
- Reaper crash (non-zero exit) still triggers a push attempt
- A push failure after Reaper exits leaves the lock held and instructs the user to run `whirlwind push` manually

**Phase 3 complete when:**
- `whirlwind push` and `whirlwind pull` skip files whose local MD5 matches the R2 ETag, printing "N files unchanged, skipped"
- `whirlwind status <project>` shows lock holder, lock age, and last-push metadata
- `whirlwind unlock <project>` requires explicit confirmation before breaking a lock and prints the lock contents before asking

---

## 2. Context & Prior Art

### Existing Codebase

The project is at scaffold stage: `cargo new` output only. `src/main.rs` is a hello-world. `Cargo.toml` has no dependencies. There is no module structure, no error handling strategy, no tests. The full implementation must be built from scratch. See `docs/spec/architecture.md`.

### Prior Art

**git-lfs** — handles large files in git repositories by storing them in object storage. Similar in that it separates large binary assets from version history. Different in that it integrates tightly with git; `whirlwind` has no git dependency and models workflows at the session level, not the commit level.

**rclone** — a general-purpose cloud storage sync tool with R2 support. Could be used directly but provides no DAW-specific primitives (no lock, no session concept, no Reaper integration). `whirlwind` is a thin, purpose-built layer over the same underlying S3 semantics.

**Dropbox** — the incumbent solution for this use case. Works, but uses a daemon model. DAW project files are large; Dropbox syncs on every save, causing bandwidth waste and potential mid-session conflicts. `whirlwind`'s session model is the direct response to this limitation.

**S3-backed conditional PUT locking** — used in Terraform's S3 backend for state locking. The `If-None-Match: *` pattern is well-established for S3-compatible stores. Terraform's state lock is the direct precedent for `whirlwind`'s lock file design.

### Architecture Constraints from Spec

From `docs/spec/security.md`: R2 credentials will live in `~/.config/whirlwind/config.toml`. The `.gitignore` must be updated to exclude credential-adjacent files. `Cargo.lock` should be committed for a binary crate.

From `docs/spec/operations.md`: no CI exists; the TDD should note what CI setup is needed before first release. The tool is a locally-installed binary — no server deployment model exists.

---

## 3. Architecture & System Design

### Component Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      CLI Layer (clap)                       │
│  init | list | status | pull | push | session | unlock      │
└───────────────────────┬─────────────────────────────────────┘
                        │
        ┌───────────────┼───────────────────┐
        │               │                   │
        ▼               ▼                   ▼
┌──────────────┐ ┌─────────────┐  ┌─────────────────────┐
│ Config       │ │ Lock        │  │ Sync Engine         │
│ System       │ │ Manager     │  │                     │
│              │ │             │  │  ┌───────────────┐  │
│ config.toml  │ │ conditional │  │  │  R2 Client    │  │
│ load/save/   │ │ PUT acquire │  │  │  (aws-sdk-s3) │  │
│ validate     │ │ DELETE rel. │  │  └───────────────┘  │
└──────────────┘ └─────────────┘  └─────────────────────┘
                                           │
                              ┌────────────┘
                              ▼
                   ┌─────────────────────┐
                   │ Process Manager     │
                   │ (session command)   │
                   │ spawn Reaper        │
                   │ wait for exit       │
                   └─────────────────────┘
```

### Module Structure

The project should be structured as a binary crate with a library module (`src/lib.rs`) exposing the core logic, allowing future testability. The `src/main.rs` entry point handles only CLI parsing and dispatch.

```
src/
├── main.rs          — entry point: parse CLI, dispatch to command handlers
├── lib.rs           — re-exports public modules; crate-level #![forbid(unsafe_code)]
├── cli.rs           — clap command/argument definitions
├── config.rs        — Config struct, load/save/validate, config file path resolution
├── r2.rs            — R2Client wrapper around aws-sdk-s3 with R2-specific setup
├── lock.rs          — LockManager: acquire, release, read, stale detection
├── sync.rs          — SyncEngine: enumerate, diff, upload, download, skip logic
├── metadata.rs      — MetadataJson: read/write project inventory in R2
├── session.rs       — session command orchestration: pull → lock → spawn → wait → push
├── progress.rs      — indicatif-based progress bar helpers
└── error.rs         — unified error type via thiserror
```

### Key Crates

| Concern | Crate | Rationale |
|---|---|---|
| S3/R2 client | `aws-sdk-s3` | Official AWS SDK; supports endpoint override for R2; async-native |
| Async runtime | `tokio` (full features) | Required by aws-sdk-s3; standard for async Rust |
| CLI | `clap` (derive feature) | Ergonomic derive-based CLI; well-maintained; widely used |
| Serialization | `serde` + `serde_json` | Industry standard; required for config/metadata/lock JSON |
| Config file | `toml` + `serde` | TOML is human-editable; serde integration is idiomatic |
| Progress bars | `indicatif` | Best-in-class terminal progress for Rust CLIs |
| Error handling | `thiserror` | Derive-based error types; cleaner than `anyhow` for a library module |
| User-facing errors | `anyhow` | Ergonomic error propagation in binary command handlers |
| MD5 hashing | `md-5` (from RustCrypto) | Pure Rust MD5 for ETag comparison; no native dependency |
| Process spawning | `std::process::Command` | Stdlib; no external crate needed for Reaper launch |
| Path handling | `std::path` | Stdlib; augmented by `dirs` crate for `~/.config` resolution |
| Home dir | `dirs` | Cross-platform `~/.config` path resolution |
| Timestamp | `chrono` | RFC3339 timestamps for lock/metadata JSON |
| Interactive prompt | `dialoguer` | Confirmation prompt for `unlock` command |

### R2 Client Layer

`r2.rs` wraps `aws_sdk_s3::Client` with R2-specific initialization:

- Endpoint URL set to `https://<account-id>.r2.cloudflarestorage.com`
- Region set to `"auto"` (R2 requires this literal string)
- Credentials loaded from `Config` struct (not from environment — credentials are in config.toml)
- All public methods are `async` and return `Result<_, AppError>`

Key operations exposed by `R2Client`:
- `list_objects(bucket, prefix)` — returns `Vec<R2Object>` (key, etag, size, last_modified)
- `get_object(bucket, key)` — returns byte stream for download
- `put_object(bucket, key, body)` — unconditional upload
- `put_object_if_not_exists(bucket, key, body)` — PUT with `If-None-Match: *`; returns `AcquireResult` (Acquired | AlreadyExists)
- `delete_object(bucket, key)` — for lock release
- `head_object(bucket, key)` — for ETag check without downloading

### Config System

`config.rs` loads `~/.config/whirlwind/config.toml` at startup. All commands except `init` require a valid config. If the file is missing, the user is directed to run `whirlwind init`. Validation at load time catches missing required fields before any R2 calls are made.

### Lock Manager

`lock.rs` provides three operations: `acquire(project)`, `release(project)`, and `read(project)`. It delegates to `R2Client::put_object_if_not_exists` for acquisition. Lock release uses `R2Client::delete_object`. Stale lock detection (`read` + compare timestamp against threshold) is advisory only — it warns but does not auto-break.

### Sync Engine

`sync.rs` implements `push(project, local_dir)` and `pull(project, local_dir)`. Both operations:
1. Enumerate files on the source side
2. Enumerate objects on the destination side
3. Compute the diff (what to transfer, what to skip)
4. Execute transfers with progress reporting

The ETag comparison is the skip mechanism: if a file's local MD5 (hex-encoded, lowercase) matches the R2 ETag (stripped of quotes), the file is skipped. See section 7 for the full algorithm.

### Process Manager

`session.rs` orchestrates the full session flow. It composes `LockManager`, `SyncEngine`, and `std::process::Command`. The Reaper binary path comes from config. The project `.rpp` file path is constructed from `local_working_dir / project_name / project_name.rpp`. See section 8 for the detailed flow.

---

## 4. Data Models & Storage

### `config.toml` Schema

Location: `~/.config/whirlwind/config.toml`
Permissions: created with mode `0600` (owner read/write only).

```toml
[r2]
account_id    = "abc123def456"          # Cloudflare account ID
access_key_id = "your-access-key-id"   # R2 API token Access Key ID
secret_access_key = "your-secret"      # R2 API token Secret Access Key
bucket        = "podcast-projects"     # R2 bucket name

[local]
working_dir = "/Users/alice/podcast"   # root dir; projects are subdirs here
                                       # e.g. /Users/alice/podcast/episode-47/

[reaper]
binary_path = "/Applications/REAPER.app/Contents/MacOS/REAPER"
             # full path to the Reaper executable

[identity]
user = "alice"                         # short name used in lock files and metadata
machine = "alice-macbook"              # hostname used in lock files
```

All four sections (`[r2]`, `[local]`, `[reaper]`, `[identity]`) are required. `whirlwind init` writes this file interactively and tests the R2 connection before saving. The `[reaper]` section is required in config but the `reaper binary_path` value is only accessed by the `session` command — other commands do not validate that the path exists.

The Rust type is:

```
Config {
    r2: R2Config { account_id, access_key_id, secret_access_key, bucket },
    local: LocalConfig { working_dir: PathBuf },
    reaper: ReaperConfig { binary_path: PathBuf },
    identity: IdentityConfig { user: String, machine: String },
}
```

`Config` implements `serde::Deserialize` and `serde::Serialize`.

### `metadata.json` Schema

Location in R2: `metadata.json` (bucket root — no prefix).

```json
{
  "version": 1,
  "projects": {
    "episode-47": {
      "last_pushed_by": "alice",
      "last_pushed_at": "2026-03-28T10:00:00Z",
      "object_count": 3,
      "total_bytes": 847392810
    },
    "episode-48": {
      "last_pushed_by": "bob",
      "last_pushed_at": "2026-03-27T14:30:00Z",
      "object_count": 5,
      "total_bytes": 1203948200
    }
  }
}
```

The `version` field is for forward compatibility. `metadata.json` is updated **after** a successful push, not before. If the push partially fails, the previous `metadata.json` remains. It is informational only — it is NOT the source of truth for what files exist in R2 (that comes from `list_objects`). It IS the source of truth for `last_pushed_by` and `last_pushed_at`.

Write strategy: read-modify-write. The operation is not atomic (R2 has no compare-and-swap for arbitrary objects). Races on `metadata.json` are acceptable: the lock protocol prevents simultaneous pushes to the same project, and `metadata.json` is informational, not load-bearing.

The Rust types:

```
MetadataJson {
    version: u32,
    projects: HashMap<String, ProjectMeta>,
}

ProjectMeta {
    last_pushed_by: String,
    last_pushed_at: DateTime<Utc>,
    object_count: u64,
    total_bytes: u64,
}
```

### Lock File Schema

Location in R2: `locks/<project-name>.lock`

A lock file exists if and only if the lock is held. Its absence means the project is unlocked.

```json
{
  "locked_by": "alice",
  "locked_at": "2026-03-28T10:00:00Z",
  "machine": "alice-macbook"
}
```

The Rust type:

```
LockFile {
    locked_by: String,
    locked_at: DateTime<Utc>,
    machine: String,
}
```

### Local State

No persistent local state file beyond `config.toml`. There is no "currently checked-out project" registry. This keeps the implementation simple and avoids stale state problems.

The consequence: `push` does not know which project was last pulled. The project name is always passed explicitly as a CLI argument.

Local file layout under `working_dir`:

```
/Users/alice/podcast/
  episode-47/
    episode-47.rpp
    audio/
      intro.wav
      interview.wav
  episode-48/
    episode-48.rpp
    audio/
      ...
```

The local project directory name must match the R2 project prefix name. This is enforced by using the CLI argument as both the local subdirectory name and the R2 prefix.

### R2 Object Key Conventions

| Resource | Key Pattern |
|---|---|
| Project files | `projects/<project-name>/<relative-path>` |
| Lock file | `locks/<project-name>.lock` |
| Metadata | `metadata.json` |

Example: a file at `episode-47/audio/intro.wav` locally maps to `projects/episode-47/audio/intro.wav` in R2. The `projects/` prefix is fixed and not configurable.

---

## 5. API Contracts

### Command: `init`

```
whirlwind init
```

**Purpose**: Interactive setup wizard. Writes `~/.config/whirlwind/config.toml`, then tests R2 connectivity.

**Behavior**:
1. Check if `config.toml` already exists. If so, prompt: "Config already exists. Overwrite? [y/N]"
2. Prompt interactively for each required field (account_id, access_key_id, secret_access_key, bucket, working_dir, reaper binary_path, user, machine). Provide defaults where possible (machine: `hostname`, working_dir: `~/podcast`).
3. Write config file with mode `0600`.
4. Attempt `list_objects(bucket, "")` to verify credentials and connectivity.
5. On success: print "Config saved. R2 connection verified."
6. On failure: print the error, do NOT save the config file, prompt user to fix and retry.

**Output on success**:
```
Config written to /Users/alice/.config/whirlwind/config.toml
Testing R2 connection... OK
```

**Error conditions**:
- R2 credentials invalid → "R2 authentication failed: check your access_key_id and secret_access_key"
- Bucket not found → "Bucket 'podcast-projects' not found: check bucket name and account_id"
- Network unreachable → "R2 connection failed: [underlying error message]"

---

### Command: `list`

```
whirlwind list
```

**Purpose**: List all known projects with their lock status and last-push info.

**Behavior**:
1. Load config.
2. Fetch `metadata.json` from R2. If it does not exist, treat as empty (no projects).
3. List all lock objects under `locks/` prefix.
4. Cross-reference to produce output.

**Output format**:
```
PROJECT          LOCKED BY    LOCKED AT             LAST PUSHED BY  LAST PUSHED AT
episode-47       alice        2026-03-28 10:00 UTC  alice           2026-03-28 09:55 UTC
episode-48       (unlocked)   -                     bob             2026-03-27 14:30 UTC
```

Column widths are dynamic (pad to longest value). If `metadata.json` is absent, last-push columns show "-".

**Error conditions**:
- Config missing → "No config found. Run `whirlwind init` first."
- R2 unreachable → "Failed to connect to R2: [error]"

---

### Command: `status`

```
whirlwind status <project>
```

**Purpose**: Show detailed status for a single project.

**Arguments**:
- `<project>` — required positional; the project name (e.g., `episode-47`)

**Output format (unlocked)**:
```
Project:        episode-47
Status:         UNLOCKED
Last pushed by: alice
Last pushed at: 2026-03-28 09:55 UTC
Object count:   3 files
Total size:     808.6 MB
```

**Output format (locked)**:
```
Project:        episode-47
Status:         LOCKED
Locked by:      alice (alice-macbook)
Locked at:      2026-03-28 10:00 UTC (28 minutes ago)
Last pushed by: alice
Last pushed at: 2026-03-28 09:55 UTC
Object count:   3 files
Total size:     808.6 MB

WARNING: Lock is 28 minutes old. If alice's session has ended, run:
  whirlwind unlock episode-47
```

The stale lock warning threshold is 4 hours. If the lock age exceeds this, print the warning block. The threshold is hard-coded for now (not configurable).

**Error conditions**:
- Project not found in `metadata.json` and no lock file → "Project 'episode-47' not found."
- R2 unreachable → "Failed to connect to R2: [error]"

---

### Command: `pull`

```
whirlwind pull <project> [--force]
```

**Purpose**: Download all project files from R2 to the local working directory. Does NOT acquire a lock.

**Arguments**:
- `<project>` — required positional
- `--force` — optional flag; skip the "project directory already exists, overwrite?" confirmation

**Behavior**:
1. Resolve local directory: `config.local.working_dir / project`.
2. List R2 objects under `projects/<project>/`.
3. If no objects found: "Project 'episode-47' not found in R2."
4. For each R2 object: compute local path, compare ETags (Phase 3; Phase 1 downloads unconditionally), download if needed.
5. Create local directories as needed.
6. Print summary.

**Output**:
```
Pulling episode-47...
  Downloading episode-47.rpp          (12 KB)
  Downloading audio/intro.wav         (234 MB)
  Downloading audio/interview.wav     (412 MB)
Pull complete: 3 files, 646 MB downloaded.
```

Phase 3 variant:
```
  Skipping audio/intro.wav            (unchanged)
  Downloading audio/interview.wav     (412 MB)
Pull complete: 1 file downloaded, 1 file unchanged.
```

**Error conditions**:
- Project not found in R2 → "Project 'episode-47' not found in R2. Has it been pushed yet?"
- Download failure for a file → "Failed to download 'audio/intro.wav': [error]. Pull aborted."
- Local disk full → propagated as I/O error with path context

---

### Command: `push`

```
whirlwind push <project> [--no-lock]
```

**Purpose**: Acquire lock, upload changed files, update `metadata.json`, release lock.

**Arguments**:
- `<project>` — required positional
- `--no-lock` — optional flag; skip lock acquisition (escape hatch for recovery scenarios; not recommended for normal use)

**Behavior**:
1. Resolve local directory: `config.local.working_dir / project`.
2. If local directory does not exist: "Local project directory not found: [path]. Run `whirlwind pull episode-47` first."
3. Unless `--no-lock`: acquire lock. On 412: abort with lock-held message (see section 6).
4. Enumerate local files under project directory.
5. For each local file: compare ETag (Phase 3; Phase 1 uploads unconditionally), upload if needed.
6. Update `metadata.json` with new push info.
7. Release lock.
8. Print summary.

**Output**:
```
Pushing episode-47...
  Uploading episode-47.rpp            (12 KB)
  Skipping audio/intro.wav            (unchanged)
Push complete: 1 file uploaded, 1 file unchanged.
Lock released.
```

**Error conditions**:
- Lock held by another user → see section 6 for exact message format
- Upload failure → "Failed to upload 'audio/intro.wav': [error]. Lock retained — run `whirlwind push episode-47` to retry."
- `metadata.json` update failure → log warning, do not fail the push (informational only)
- Lock release failure → "WARNING: Push succeeded but lock release failed. Run `whirlwind unlock episode-47` to clean up."

---

### Command: `session`

```
whirlwind session <project>
```

**Purpose**: The primary command. Pull, lock, launch Reaper, wait for exit, push, unlock.

**Arguments**:
- `<project>` — required positional

See section 8 for the full flow specification.

---

### Command: `unlock`

```
whirlwind unlock <project> [--force]
```

**Purpose**: Emergency lock break. Shows who holds the lock, asks for confirmation, deletes the lock file.

**Arguments**:
- `<project>` — required positional
- `--force` — skip confirmation prompt (for scripting / non-interactive use)

**Behavior**:
1. Fetch and display the lock file contents.
2. Unless `--force`: prompt "Break lock held by alice since 2026-03-28 10:00 UTC? [y/N]"
3. On confirmation: `delete_object("locks/episode-47.lock")`.
4. Print "Lock broken."

**Output**:
```
Lock details:
  Locked by: alice (alice-macbook)
  Locked at: 2026-03-28 10:00 UTC (3 hours 12 minutes ago)

Break this lock? [y/N]: y
Lock broken.
```

**Error conditions**:
- No lock exists → "Project 'episode-47' is not locked."
- R2 delete failure → "Failed to release lock: [error]"
- User says N at prompt → "Aborted." (exit 0)

---

### Exit Codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error (R2 failure, I/O error, config error) |
| 2 | Lock contention (another user holds the lock) |
| 3 | User abort (declined confirmation) |

---

## 6. Concurrency & Lock Protocol

### Lock Acquisition

Lock acquisition is a single conditional PUT:

```
PUT /locks/<project-name>.lock
If-None-Match: *
Content-Type: application/json
Body: {"locked_by":"alice","locked_at":"2026-03-28T10:00:00Z","machine":"alice-macbook"}
```

**Response 200/201**: Lock acquired. Proceed.

**Response 412 Precondition Failed**: Lock is held by another user. The acquiring client must:
1. `GET /locks/<project-name>.lock` to retrieve the lock file contents.
2. Print the following and exit with code 2:

```
episode-47 is currently locked.

  Locked by: alice (alice-macbook)
  Locked at: 2026-03-28 10:00 UTC (42 minutes ago)

Wait for alice to finish, or run `whirlwind unlock episode-47` to break the lock.
```

### Lock Release

Lock release is a `DELETE /locks/<project-name>.lock`. A 404 on delete is treated as success (idempotent). Any other error is surfaced as a warning but does not fail the command.

### Lock in `session` vs. `push`

- `push` acquires the lock at the start and releases it at the end.
- `session` acquires the lock before the pull (so the pull itself reflects the locked state) and releases it after the push.
- If `push --no-lock` is used after a session failure, it skips acquisition because the session already holds (or has lost) the lock.

### Lock Release on Panic / Crash

Rust panics do not run `Drop` implementations in all cases (e.g., `abort` on double-panic). For the `session` command, the lock is acquired before Reaper is launched. If `whirlwind` itself panics between lock acquisition and lock release, the lock remains held.

Mitigation:
1. The `status` command shows lock age. After 4 hours, a stale lock warning is shown.
2. The `unlock` command provides the manual escape hatch.
3. A `Drop` guard on `LockGuard` handles clean exits and normal panics (stack unwinding). This covers Ctrl+C and most crash scenarios.

The `LockGuard` pattern:

```
LockGuard {
    project: String,
    r2_client: Arc<R2Client>,
}
impl Drop for LockGuard {
    fn drop(&mut self) {
        // best-effort async-in-drop via tokio::task::block_in_place or a sync RT call
        // release the lock; log error on failure; do not panic
    }
}
```

The limitation: `Drop` does not run on `SIGKILL` or power loss. This is acceptable — those scenarios require manual `unlock`.

### Stale Lock Detection

A lock is "stale" if `locked_at` is more than 4 hours in the past. Stale lock detection is advisory:
- `status` prints a warning block (see section 5).
- `list` marks the lock with "(stale?)" in the status column.
- No automatic breaking of stale locks — only `unlock` does that.

The 4-hour threshold is hard-coded as a constant `STALE_LOCK_THRESHOLD_HOURS = 4`. It is not user-configurable in Phase 1-3.

### The Self-Lock Scenario

If a user runs `push` on a project they have already locked (e.g., via a prior crashed `session`), `push` will receive a 412. The client compares `locked_by` and `machine` against its own config identity. If they match, it prints:

```
episode-47 is locked by you (alice on alice-macbook) from a previous session.

If your previous session failed, run:
  whirlwind push episode-47 --no-lock    # push using the existing lock, then release
  whirlwind unlock episode-47            # or just break the lock if you don't need to push
```

---

## 7. Sync Algorithm

### File Enumeration

**Local side**: Recursively walk `config.local.working_dir / <project>` using `std::fs::read_dir` (or `walkdir` crate). Collect all files as relative paths (relative to the project root directory).

**R2 side**: Call `list_objects(bucket, "projects/<project>/")`. Strip the `"projects/<project>/"` prefix from each key to get relative paths. Note: R2 returns up to 1,000 objects per `list_objects` call; pagination must be handled via `continuation_token` for projects with >1,000 files (uncommon for podcasts but must not silently truncate).

### ETag Computation

R2's ETag for a single-part upload is the lowercase hex MD5 of the object content, wrapped in double quotes (e.g., `"d41d8cd98f00b204e9800998ecf8427e"`). For multipart uploads, the ETag format is `"<md5_of_md5s>-<part_count>"`, which is NOT a simple MD5 of the file. If multipart ETags are encountered (detected by the `-<N>` suffix), skip ETag comparison for that file and always re-upload/re-download (conservative fallback).

Local ETag computation: read the file, compute MD5, hex-encode lowercase. Compare against R2 ETag with quotes stripped.

```
fn compute_local_etag(path: &Path) -> Result<String, AppError> {
    // read file bytes, run through md5::Md5, return hex string
}

fn etags_match(local_etag: &str, r2_etag: &str) -> bool {
    let r2_stripped = r2_etag.trim_matches('"');
    if r2_stripped.contains('-') {
        // multipart upload — cannot compare; treat as changed
        return false;
    }
    local_etag == r2_stripped
}
```

### Push Algorithm

```
push(project, local_dir):
  local_files = enumerate_local_files(local_dir)  // relative paths → file metadata
  r2_objects  = enumerate_r2_objects(bucket, "projects/" + project)  // relative paths → R2 metadata

  r2_map = HashMap<relative_path, R2Object>

  to_upload = []
  for (rel_path, local_meta) in local_files:
      if r2_map.contains(rel_path):
          local_etag = compute_local_etag(local_dir / rel_path)
          if etags_match(local_etag, r2_map[rel_path].etag):
              record as "skipped (unchanged)"
              continue
      to_upload.append(rel_path)

  // Note: whirlwind does NOT delete R2 objects that no longer exist locally.
  // Rationale: audio files might be intentionally on one machine but not yet
  // pulled to the other. Deletion is out of scope for Phase 1-3.

  for rel_path in to_upload:
      upload(local_dir / rel_path → "projects/" + project + "/" + rel_path)
      show progress bar per file

  update_metadata(project, count=len(local_files), bytes=sum(local_file_sizes))
```

### Pull Algorithm

```
pull(project, local_dir):
  r2_objects  = enumerate_r2_objects(bucket, "projects/" + project)
  local_files = enumerate_local_files(local_dir)  // empty dict if dir doesn't exist

  local_map = HashMap<relative_path, local_etag>

  to_download = []
  for (rel_path, r2_obj) in r2_objects:
      if local_map.contains(rel_path):
          local_etag = compute_local_etag(local_dir / rel_path)
          if etags_match(local_etag, r2_obj.etag):
              record as "skipped (unchanged)"
              continue
      to_download.append((rel_path, r2_obj))

  for (rel_path, r2_obj) in to_download:
      create_parent_dirs(local_dir / rel_path)
      download(r2_obj → local_dir / rel_path)
      show progress bar per file
```

### The `.rpp` File

The `.rpp` file is treated no differently than any other file in the sync algorithm. It is small (typically under 1 MB), so the cost of always comparing its ETag is negligible. The `.rpp` file is expected to change every session; it will almost always be uploaded/downloaded.

Convention: the `.rpp` file is expected at `<local_dir>/<project-name>.rpp`. The `session` command passes this path as Reaper's project argument.

### Deletion Semantics

`whirlwind` does NOT delete files from R2 or from the local filesystem during push or pull. Rationale:
- Accidental deletion of multi-GB audio files is catastrophic and irreversible on R2 (no versioning enabled by default).
- The two-editor use case does not require deletion sync — if a file exists in R2 but not locally, it likely hasn't been pulled yet by that editor, not deleted.
- Deletion can be added as an opt-in flag in a future phase if needed.

### Progress Reporting

Each file transfer (upload or download) gets a single `indicatif::ProgressBar` showing:
- File name (truncated to 40 chars if needed)
- Transfer size
- Progress percentage / bytes transferred
- Transfer speed (KB/s or MB/s)

A `MultiProgress` wraps individual bars. On completion, each bar is replaced with a single summary line:
```
  ✓ audio/interview.wav (412 MB)
```

For Phase 1, a simple println-per-file approach without `indicatif` is acceptable. `indicatif` is introduced in Phase 2.

---

## 8. Session Command Flow

### Happy Path

```
whirlwind session episode-47
```

**Step 1: Validate prerequisites**
- Load config. Fail fast if missing or invalid.
- Verify `config.reaper.binary_path` exists on disk. If not: "Reaper not found at [path]. Check `reaper.binary_path` in config.toml."

**Step 2: Acquire lock**
- Call `lock_manager.acquire("episode-47")`.
- On 412: print lock-held message and exit with code 2. Do not proceed.
- On success: instantiate `LockGuard` (see section 6).

**Step 3: Pull**
- Call `sync_engine.pull("episode-47", local_dir)`.
- On failure: release lock, print error, exit 1.
  - Rationale: if we can't pull, don't launch Reaper with stale files.

**Step 4: Launch Reaper**
- Construct the `.rpp` path: `local_dir / "episode-47.rpp"`.
- If `.rpp` file does not exist locally (first pull): this is expected and okay. Reaper will create it.
- Spawn Reaper: `Command::new(binary_path).arg(rpp_path).spawn()`.
- On spawn failure: release lock, print "Failed to launch Reaper: [error]", exit 1.
- Print: "Reaper launched (PID [pid]). Waiting for Reaper to exit..."

**Step 5: Wait for Reaper exit**
- Call `child.wait()` (blocking). This blocks the `whirlwind session` process until Reaper exits.
- Capture exit status.

**Step 6: Push**
- Regardless of Reaper exit status (clean or crash), attempt push.
- Call `sync_engine.push("episode-47", local_dir)` with the lock already held (use `push_with_held_lock` variant that skips lock acquisition).
- On success: `LockGuard` drops, lock is released. Print summary.
- On failure: **do not release the lock**. Print:

```
Reaper exited. Push failed: [error]

Your lock on episode-47 is still held. Your local changes are safe.
To retry: whirlwind push episode-47
To give up: whirlwind unlock episode-47
```

Exit with code 1. The lock remains in R2 until the user manually retries or unlocks.

**Step 7: Complete**
- Print: "Session complete. Lock released."

### Reaper Exit Status Handling

| Scenario | Reaper exit code | whirlwind behavior |
|---|---|---|
| Normal close (File → Quit) | 0 | Push, release lock, exit 0 |
| Force quit (kill, Cmd+Q crash) | non-zero | Push anyway, release lock, exit 0 |
| Reaper killed by SIGKILL | — | `child.wait()` returns non-zero; same as above |

The rationale for always pushing regardless of exit code: users expect their edits to be saved even if Reaper crashes. If there are no changed files, the push is effectively a no-op.

### Ctrl+C During Session

If the user presses Ctrl+C while `whirlwind session` is waiting for Reaper:
- Rust's default SIGINT handler terminates the process.
- `LockGuard::drop` runs (stack unwinding), releasing the lock.
- Reaper continues running (it is a child process but its stdin/stdout are not the terminal; it will keep running).
- The user's edits are not pushed.

This behavior is intentional: Ctrl+C during a session means "cancel the session wrapper, let me handle this manually." The user can then run `whirlwind push episode-47` manually when done.

A future enhancement could intercept SIGINT to ask "Push current changes before exiting? [y/N]" but this is out of scope for Phase 1-3.

### Reaper Binary Path Across Platforms

| Platform | Default path |
|---|---|
| macOS | `/Applications/REAPER.app/Contents/MacOS/REAPER` |
| Linux | `/usr/bin/reaper` (varies by install method) |
| Windows | `C:\Program Files\REAPER (x64)\reaper.exe` |

`whirlwind init` should default the Reaper binary path prompt based on `std::env::consts::OS`. The user can always override. The path is stored verbatim — no auto-detection at runtime.

---

## 9. Risks & Open Questions

### Risk 1: R2 Support for `If-None-Match: *` on PUT

**Risk**: Cloudflare R2's S3 compatibility may not fully implement conditional PUT with `If-None-Match: *`. The `aws-sdk-s3` SDK sends this header as a standard S3 operation, but R2 may silently ignore it, making lock acquisition non-atomic.

**Impact**: If R2 ignores the conditional, two simultaneous `push` calls would both succeed, and the second would silently overwrite the first's lock file. The lock protocol would be broken.

**Status**: OPEN — must be verified against a real R2 bucket before Phase 1 is considered complete. Cloudflare's documentation as of 2026-03 states S3 conditional operations are supported, but this must be confirmed empirically.

**Mitigation if not supported**: Fall back to a "lock-by-naming" approach — put a lock file with a random UUID in the key, then list objects with the lock prefix and verify only one lock exists. This is weaker (not atomic) but probabilistically correct for a 2-user system with non-simultaneous edits.

**Verification step**: Before implementation, manually PUT an object with `If-None-Match: *`, confirm 200; then attempt the same PUT again, confirm 412. Document result in this TDD.

### Risk 2: Large File Upload — Multipart Threshold

**Risk**: `aws-sdk-s3` automatically uses multipart upload for objects above a threshold (default: 8 MB in the Rust SDK). Multipart uploads produce ETags in the format `"<md5_of_part_etags>-<N>"`, not the simple MD5 of the file. This means ETag-based skip logic will not work for large audio files after their first upload.

**Impact**: Phase 3 ETag skip optimization breaks for files larger than the multipart threshold. Every push would re-upload large audio files, negating the primary performance benefit.

**Mitigation options**:
1. Set multipart threshold to a value larger than any expected file (e.g., 5 GB). Not recommended — loses multipart benefits for very large files.
2. Store a separate content hash alongside the R2 object as custom metadata (`x-amz-meta-content-md5`). On upload, attach the MD5 as user metadata. On ETag comparison, check user metadata first; fall back to ETag.
3. Accept the limitation: multipart ETag mismatches → always re-upload. Document this.

**Recommended approach**: Option 2. Set `x-amz-meta-content-md5` on every `put_object` call. On `head_object` / `list_objects` responses, check for this metadata key first; use it for comparison if present. This is backward-compatible (files without the metadata key fall back to ETag comparison or unconditional upload).

**This is a Phase 3 decision** — ETag skip is a Phase 3 feature. Phase 1 and 2 upload unconditionally.

### Risk 3: `.rpp` Files with Absolute Paths

**Risk**: Reaper defaults to storing media file paths as absolute paths in the `.rpp` XML (e.g., `/Users/alice/podcast/episode-47/audio/intro.wav`). When the `.rpp` file is pulled to Bob's machine, all media paths point to Alice's filesystem. Reaper will open but all audio will be missing ("offline").

**Impact**: This is a user-facing usability issue, not a `whirlwind` bug. But if it's not prominently documented, users will be confused and blame the tool.

**Mitigation**: `whirlwind` should:
1. Detect the OS and print a prominent warning during `whirlwind init`:

```
IMPORTANT: Configure Reaper to use relative media paths before using whirlwind.
In Reaper: Preferences → Media → "Store all paths as: Relative where possible"
Absolute paths in .rpp files will break on your collaborator's machine.
```

2. Consider adding a `whirlwind doctor` command (future phase) that scans the `.rpp` file for absolute path patterns and warns.

**This is a documentation and UX concern, not a blocking implementation risk.**

### Risk 4: Concurrent `push` and `list` on `metadata.json`

**Risk**: `metadata.json` is read-modify-written without locking. If two users call `whirlwind list` simultaneously while a push is updating `metadata.json`, one could read a corrupt write (R2 PUT is atomic, but the read-modify-write sequence is not).

**Impact**: `metadata.json` could have stale or slightly incorrect last-push data. The project files themselves are protected by the lock. `metadata.json` is informational.

**Mitigation**: The project lock prevents concurrent pushes. The only risk is a `list` read racing a `metadata.json` write mid-update. R2 PUT operations are atomic — a reader will see either the old version or the new version, never a partial write. This is acceptable.

### Risk 5: `whirlwind` Process Dies While Reaper is Running

**Risk**: If the machine running `whirlwind session` loses power, is killed, or crashes while Reaper is running, the lock remains held and Reaper continues running (it does not know about `whirlwind`).

**Impact**: The other editor cannot push until the stale lock is manually cleared.

**Mitigation**: Stale lock detection (4-hour warning) + `whirlwind unlock` command. Document this in the README.

### Open Question 1: Should `pull` acquire a lock?

`pull` in the current design does NOT acquire a lock. This means a user can pull while another is pushing. The pull will get a mix of old and new file versions if a push happens concurrently.

For a 2-user, non-simultaneous editing workflow, this is acceptable. If one user is pushing (holding the lock), the other user starting a `pull` would get whatever files R2 has (which may be mid-update). However, because pushes in practice complete quickly for unchanged files, this window is narrow.

**Decision needed**: Accept this for Phase 1-3 (document it), or add advisory "check for active lock before pull" behavior. Recommendation: check for lock before pull, print warning "episode-47 is currently being pushed by alice — pull may get partial data" but do not block.

### Open Question 2: Project name vs. directory name mapping

The design assumes `project_name == local_directory_name == R2_prefix`. If a user names their local directory differently (e.g., `ep47/` instead of `episode-47/`), they must always specify the R2 project name as the CLI argument. This is correct behavior but should be documented clearly.

---

## 10. Testing Strategy

### What to Test

**Unit tests** (in `src/` modules, gated by `#[cfg(test)]`):
- `config.rs`: valid config round-trips through TOML serialize/deserialize; missing fields produce clear errors; path resolution works on each platform.
- `lock.rs`: `LockFile` serializes/deserializes correctly; stale detection logic is correct for edge cases (exactly at threshold, just under, just over).
- `sync.rs`: ETag comparison logic (`etags_match`) — exact match, multipart ETag, case differences, quoted vs unquoted; diff algorithm produces correct upload/download/skip sets for various file states.
- `metadata.rs`: `MetadataJson` round-trips; missing projects handled gracefully.
- `error.rs`: error messages contain expected contextual strings.

**Integration tests** (in `tests/` directory):
- These require a real or local-mock R2 endpoint. Use a dedicated test R2 bucket (separate from production). Tests are gated by an env var (`WHIRLWIND_TEST_R2_BUCKET`); they are skipped in CI unless credentials are present.
- Lock acquire + release round-trip.
- Lock acquire when lock is already held → returns correct error.
- Push → pull round-trip: push files from one temp dir, pull to another, verify file contents match.
- ETag skip: push files, push again with no changes → confirm no uploads occur.

**Manual acceptance tests** (documented in `docs/testing/` — to be created by @qa-engineer):
- Full `session` flow with a real Reaper install.
- Stale lock warning after manual timestamp manipulation.
- `unlock` confirmation flow.
- Platform-specific Reaper path defaults on macOS, Linux, Windows.

### What Not to Test

- R2 infrastructure reliability (not our code).
- Reaper binary behavior (not our code).
- Multipart upload behavior of `aws-sdk-s3` (test the `etags_match` fallback logic, not the SDK).

---

## 11. Implementation Phases

### Phase 1: R2 Client + Config + pull/push/list

**Goal**: A working `whirlwind` binary that can manage project files in R2 without Reaper integration. Manually usable as a push/pull tool.

**Deliverables**:
- `Cargo.toml` with all Phase 1 dependencies pinned
- `error.rs` — unified error type
- `config.rs` — load/save/validate, `~/.config/whirlwind/config.toml`
- `r2.rs` — R2Client with list, get, put, put-if-not-exists, delete, head
- `lock.rs` — acquire, release, read, stale detection
- `metadata.rs` — read/write `metadata.json`
- `sync.rs` — push and pull (unconditional, no ETag skip in Phase 1)
- `cli.rs` — clap definitions for all 7 commands (even if some are stubs)
- `main.rs` — dispatch to command handlers
- Command implementations: `init`, `list`, `pull`, `push`
- `status` and `unlock` as stubs (print "coming in Phase 3")
- `session` as stub (print "coming in Phase 2")

**Sequential dependencies within Phase 1**:
1. `error.rs` must be done first (everything depends on the error type).
2. `config.rs` next (nearly everything depends on config).
3. `r2.rs` (depends on config for credentials).
4. `lock.rs` and `metadata.rs` can be built in parallel (both depend on `r2.rs`).
5. `sync.rs` (depends on `r2.rs`, `lock.rs`).
6. CLI command handlers (depend on all of the above).

**Can be parallelized**:
- `lock.rs` and `metadata.rs` once `r2.rs` is complete.
- `cli.rs` (clap definitions, no logic) can be written in parallel with anything.
- `config.rs` tests can be written in parallel with `r2.rs` implementation.

**Complexity**: Large. This is the majority of the codebase.

**Acceptance gate**: Passes Phase 1 acceptance criteria from section 1. The `If-None-Match: *` verification (Risk 1) must be completed before Phase 1 is signed off.

---

### Phase 2: Session Command + Progress UI

**Goal**: The `session` command works end-to-end on macOS. Progress bars shown for uploads and downloads.

**Deliverables**:
- `session.rs` — full session orchestration (pull → lock → spawn → wait → push)
- `progress.rs` — `indicatif` progress bar wrappers for upload/download
- Updated `sync.rs` — progress bar integration into upload/download loops
- Updated `main.rs` — dispatch to session command handler
- Platform default Reaper paths in `init` prompts

**Sequential dependencies within Phase 2**:
1. `progress.rs` first (sync depends on it).
2. Updated `sync.rs` with progress integration.
3. `session.rs` (depends on updated sync, lock, config).

**Can be parallelized**:
- `progress.rs` can be developed independently and wired in once `sync.rs` is being updated.
- Platform default path logic in `init` is independent of session orchestration.

**Complexity**: Medium.

**Acceptance gate**: Passes Phase 2 acceptance criteria from section 1. Must be tested on macOS with a real Reaper install. Linux/Windows testing deferred.

---

### Phase 3: ETag Skip + status/unlock Polish

**Goal**: Smart sync (skip unchanged files), full `status` command, confirmed `unlock` command.

**Deliverables**:
- Updated `sync.rs` — ETag comparison logic, `compute_local_etag`, `etags_match`, multipart fallback
- Updated `r2.rs` — attach `x-amz-meta-content-md5` on `put_object`; read it back on `head_object`/`list_objects`
- Full `status` command implementation (lock age, stale warning, last-push details)
- Full `unlock` command implementation (`dialoguer` confirmation, lock content display)
- Updated `push` and `pull` output to show "N files unchanged, skipped"

**Sequential dependencies within Phase 3**:
1. `r2.rs` update to attach/read content-md5 metadata (required before ETag logic is reliable for large files).
2. `sync.rs` ETag logic (depends on updated r2.rs).
3. `status` and `unlock` commands are independent of ETag work — can be built in parallel.

**Can be parallelized**:
- `status` command and `unlock` command can be built simultaneously.
- ETag work in `sync.rs` and the status/unlock commands are fully independent.

**Complexity**: Medium.

**Acceptance gate**: Passes Phase 3 acceptance criteria from section 1. ETag skip must be verified empirically: push once, modify only `.rpp`, push again, confirm only `.rpp` is uploaded.

---

### Cross-Phase Infrastructure (do in Phase 1, needed by all phases)

The following must be established in Phase 1 before Phase 2 begins:

- `rust-toolchain.toml` pinning the Rust version (see `docs/spec/operations.md` gap)
- `Cargo.lock` committed (see `docs/spec/security.md` gap)
- `.gitignore` updated to exclude `*.pem`, `*.key`, `.env`, `config.toml` pattern with exception for the template (see `docs/spec/security.md` gap)
- `#![forbid(unsafe_code)]` at crate root (see `docs/spec/security.md` gap)
- CI pipeline with `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo audit` (see `docs/spec/operations.md` gap)
