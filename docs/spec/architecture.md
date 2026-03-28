# Architecture Specification

**Project**: whirlwind
**Language**: Rust (edition 2024)
**Last updated**: 2026-03-28
**Status**: Phase 1 complete — core implementation exists

---

## Project Overview

`whirlwind` is a CLI tool for collaborative Reaper DAW project sync backed by Cloudflare R2.
Two users share a project directory in R2 and coordinate edits through a distributed lock protocol
built on `If-None-Match: *` conditional PUT. The tool is a locally-installed binary; there is no
server-side component.

---

## Repository Layout

```
whirlwind/
├── .gitignore
├── Cargo.toml              # binary crate + [lib] section; edition 2024
├── Cargo.lock              # should be committed (binary crate)
├── README.md
├── docs/
│   ├── spec/               # architecture, security, etc.
│   └── tdd/
│       └── whirlwind-core.md
├── src/
│   ├── main.rs             # entry point + command handlers
│   ├── lib.rs              # re-exports modules; #![forbid(unsafe_code)]
│   ├── cli.rs              # clap command/argument definitions (main.rs module, not lib)
│   ├── config.rs           # Config struct, load/save/validate, config file path
│   ├── r2.rs               # R2Client wrapper around aws-sdk-s3
│   ├── lock.rs             # LockManager + LockGuard RAII type
│   ├── metadata.rs         # MetadataManager: metadata.json read/write
│   ├── sync.rs             # SyncEngine: push/pull file operations
│   └── error.rs            # unified AppError type via thiserror
└── tests/
    └── spike_r2_conditional_put.rs
```

Notable: `cli.rs` is declared as a module inside `src/main.rs`, not via `src/lib.rs`. It is a
binary-only concern. The library (`lib.rs`) exposes `config`, `error`, `lock`, `metadata`, `r2`,
and `sync` — everything needed for command implementation.

Session orchestration (`session.rs`) and progress bars (`progress.rs`) are deferred to Phase 2.

---

## Crate Structure

`Cargo.toml` defines a single binary crate with a `[[bin]]` entry pointing at `src/main.rs`. There
is no explicit `[lib]` section — the library is inferred from `src/lib.rs`. This is the
binary-with-library pattern that allows `main.rs` to `use whirlwind::...` imports and keeps core
logic testable in isolation.

---

## Component Map

```
┌─────────────────────────────────────────────────────────────┐
│                   main.rs — command handlers                 │
│  run_init | run_list | run_pull | run_push                   │
│  (session, status, unlock: stub placeholders for Phase 2/3)  │
└───────────────────────┬─────────────────────────────────────┘
                        │ uses
        ┌───────────────┼───────────────────┐
        ▼               ▼                   ▼
┌──────────────┐ ┌─────────────┐  ┌─────────────────────┐
│ config.rs    │ │ lock.rs     │  │ sync.rs             │
│ Config       │ │ LockManager │  │ SyncEngine          │
│ load/save/   │ │ LockGuard   │  │ push / pull         │
│ validate     │ │ (RAII drop) │  └──────────┬──────────┘
└──────────────┘ └──────┬──────┘             │
                        │ uses               │ uses
                        ▼                   ▼
                ┌─────────────────────────────┐
                │ r2.rs — R2Client            │
                │ list_objects                │
                │ get_object_bytes            │
                │ put_object (unconditional)  │
                │ put_object_if_not_exists    │
                │ delete_object               │
                │ head_object                 │
                └─────────────────────────────┘
                        │ uses (via Arc)
                        ▼
                ┌─────────────────────┐
                │ metadata.rs         │
                │ MetadataManager     │
                │ load / save /       │
                │ record_push         │
                └─────────────────────┘
```

---

## Module Responsibilities

### `error.rs`

Defines `AppError`, the single unified error type for the entire crate, using `thiserror`. All
public module functions return `Result<_, AppError>`. Key variants:

- `ConfigMissing`, `ConfigInvalid` — config layer
- `R2AuthFailure`, `R2Error` — R2 layer
- `LockContention`, `SelfLock`, `LockNotFound` — lock layer
- `DownloadFailed`, `UploadFailed`, `IoError` — sync/file layer
- `ReaperNotFound`, `ReaperSpawnFailed` — process layer (Phase 2)
- `Other` — catch-all

`AppError::exit_code()` maps error variants to exit codes: `LockContention` and `SelfLock` exit 2
(scripting-distinguishable); all others exit 1.

Note: `DownloadFailed` is currently used for both 404-not-found and genuine download failures.
This conflation is a known issue (see security spec) that requires a `NotFound` distinction before
Phase 2 when lock read reliability is more critical.

### `config.rs`

Loads and validates `~/.config/whirlwind/config.toml`. On Unix, saves with `0o600` file
permissions. Validation checks for non-empty required fields but does not verify that
`local.working_dir` is an absolute path (open gap).

### `r2.rs`

Wraps `aws_sdk_s3::Client` with R2-specific initialization:
- Endpoint: `https://<account_id>.r2.cloudflarestorage.com`
- Region: `"auto"` (required by R2)
- Credentials: from config struct only — ambient `AWS_*` env vars are intentionally not inherited

412 detection in `put_object_if_not_exists` uses `raw_response().status()` as the primary signal,
with a `Debug`-string fallback. The fallback is fragile (implementation-detail parsing) and should
be narrowed or removed in a future pass.

### `lock.rs`

`LockManager` provides `acquire`, `release`, and `read`. `acquire` uses `put_object_if_not_exists`
for atomic lock creation. On conflict (412), it fetches the existing lock and returns either
`LockContention` or `SelfLock` based on whether the lock matches the current user+machine.

`LockGuard` is an RAII wrapper: when dropped, it calls `delete_object` to release the lock. The
drop implementation uses `tokio::task::block_in_place` + `Handle::current().block_on()`. This
works correctly when the tokio multi-thread runtime is active but will panic if `Handle::current()`
is called outside a runtime context (e.g., during abnormal runtime shutdown). See the review
findings for the recommended fix using `Handle::try_current()`.

### `metadata.rs`

`MetadataManager` reads and writes `metadata.json` (stored at the bucket root, no prefix).
Uses a HEAD-then-GET pattern to distinguish "file not found" from "read error" — two round trips
per load. A simpler single-GET approach that handles `DownloadFailed` directly would be preferable
once the `NotFound` error variant is introduced.

`record_push` performs a read-modify-write; this is intentionally non-atomic. The lock protocol
prevents concurrent pushes to the same project, making races on `metadata.json` benign.

### `sync.rs`

`SyncEngine::push` walks the local directory with `walkdir::WalkDir`. Walk errors are silently
discarded via `filter_map(|e| e.ok())` — this means files that cannot be read are silently skipped
without failing the push (open gap).

`SyncEngine::pull` reconstructs the full R2 key by re-prefixing (`format!("{}{}", prefix, obj.key)`).
This is correct because `list_objects` strips the prefix from returned keys. Phase 1 is
unconditional (all files transferred); Phase 3 adds ETag-based skip logic.

### `cli.rs` (binary-only)

Defined as a local `mod cli` inside `main.rs`, not part of the library. Declares the `clap`
command structure. The `--no-lock` flag on `push` and `--force` on `unlock` are present but carry
insufficient runtime friction for operations that can cause data loss.

### `main.rs`

Command dispatch. `load_config_and_r2` is a shared helper that calls `process::exit(1)` directly
on failure rather than propagating `Result`. This is a structural limitation: it loses
`AppError::exit_code()` differentiation and is untestable.

---

## Key Architectural Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Crate type | Binary + inferred library (`src/lib.rs`) | Enables unit testing of core modules |
| Async runtime | `tokio` full features | Required by `aws-sdk-s3`; standard for async Rust |
| Error strategy | `thiserror` + unified `AppError` | Type-safe variants; user-facing messages in `#[error]` attrs; exit codes on enum |
| Config format | TOML via `toml` + `serde` | Human-editable; idiomatic for Rust config files |
| Lock primitive | `If-None-Match: *` conditional PUT | Only atomic primitive available on R2 (no server-side compute) |
| Lock RAII | `LockGuard` Drop impl | Guarantees release even on early return from error path |
| S3 client | `aws-sdk-s3` v1 | Official SDK; R2-compatible via endpoint override |
| Serialization | `serde_json` for R2 data; `toml` for config | JSON is R2-portable; TOML is human-editable |
| Directory traversal | `walkdir` | Handles nested subdirectories correctly |
| CLI | `clap` derive | Ergonomic; widely used; generates `--help` from doc comments |
| `anyhow` | Declared but unused | Should be removed — `AppError` propagates cleanly without it |

---

## R2 Object Key Conventions

| Resource | Key Pattern |
|---|---|
| Project files | `projects/<project-name>/<relative-path>` |
| Lock file | `locks/<project-name>.lock` |
| Metadata | `metadata.json` |

Project names are passed directly into key construction without validation. The accepted characters
for project names should be restricted to `[a-zA-Z0-9_-]` to prevent key ambiguity.

---

## Data Flow: Push Command

```
run_push
  ├── validate local_dir exists
  ├── LockManager::acquire(project)          — If-None-Match: * PUT to locks/<project>.lock
  │   └── returns LockGuard (RAII)
  ├── SyncEngine::push(project, local_dir)
  │   └── WalkDir local_dir
  │       └── for each file: R2Client::put_object(projects/<project>/<rel_path>, bytes)
  ├── MetadataManager::record_push(...)      — best-effort, errors silently discarded (gap)
  └── LockGuard drops → R2Client::delete_object(locks/<project>.lock)
```

---

## Data Flow: Pull Command

```
run_pull
  ├── create local_dir if not exists
  └── SyncEngine::pull(project, local_dir)
      ├── R2Client::list_objects("projects/<project>/")
      └── for each object: R2Client::get_object_bytes(full_key) → write to local_path
```

No lock is acquired during pull. This matches the design intent (pull is read-only from R2's
perspective) but means a pull can race with another user's push. For the two-user sequential-edit
model, this is acceptable.

---

## Dependency Graph

```
main.rs ──► lib.rs ──► config.rs
                  ──► error.rs
                  ──► lock.rs ──► r2.rs
                  ──► metadata.rs ──► r2.rs
                  ──► r2.rs
                  ──► sync.rs ──► r2.rs
```

`Arc<R2Client>` is shared across `LockManager`, `SyncEngine`, and `MetadataManager`. Constructed
once in the command handler and passed via `Arc::clone`.

---

## Integration Points

- **Cloudflare R2**: S3-compatible object storage. Accessed via HTTPS. Endpoint:
  `https://<account_id>.r2.cloudflarestorage.com`. No server-side compute, triggers, or
  native versioning.
- **Local filesystem**: `~/.config/whirlwind/config.toml` (credentials); `working_dir/<project>/`
  (project files).
- **Reaper DAW** (Phase 2): spawned as a child process via `std::process::Command`.

---

## Phase Roadmap

| Phase | Status | Key Additions |
|---|---|---|
| Phase 1 | Complete | `init`, `list`, `pull`, `push`, `lock`, R2 client, metadata, error types |
| Phase 2 | Planned | `session` command, progress bars (`indicatif`), Reaper process management |
| Phase 3 | Planned | ETag-based skip on push/pull, `status` command, `unlock` command |

---

## Known Gaps and Open Issues

| Gap | Impact |
|---|---|
| `LockGuard::Drop` panics if `Handle::current()` called outside runtime | Correctness — panic on abnormal shutdown |
| `DownloadFailed` conflates 404 with network errors | `lock.rs::read` may treat network error as "no lock" |
| `sync.rs` silently skips WalkDir errors | Files that fail to read are silently not uploaded |
| `load_config_and_r2` calls `process::exit` directly | Loses exit code differentiation; untestable |
| `metadata.json` write failure silently discarded | User sees stale metadata with no warning |
| `metadata.rs::load` uses HEAD+GET (two round trips) | Minor latency overhead per command |
| No integration tests against live R2 | Spike test exists but no automated coverage |
| `session.rs`, `progress.rs` not yet implemented | Phase 2 modules absent |
