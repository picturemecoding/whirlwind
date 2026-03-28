# Security Specification

**Project**: whirlwind
**Language**: Rust (edition 2024)
**Last updated**: 2026-03-28
**Status**: Phase 1 complete — core implementation exists

---

## Current State

`whirlwind` is a Rust CLI tool for collaborative Reaper DAW project sync backed by Cloudflare R2.
Phase 1 is code-complete. The security surface now includes:

- R2 credentials stored on disk in `~/.config/whirlwind/config.toml`
- Credential file written with mode `0600` (owner read/write only) on Unix
- Network I/O to Cloudflare R2 via HTTPS (aws-sdk-s3 handles TLS)
- User identity embedded in lock files and metadata written to shared R2 storage
- CLI input collection via `dialoguer` for interactive `init` wizard
- No `unsafe` blocks (`#![forbid(unsafe_code)]` enforced at crate root)

---

## Authentication and Authorization

**Authentication model**: Static R2 API token credentials (Access Key ID + Secret Access Key).
Credentials are loaded exclusively from `~/.config/whirlwind/config.toml` — the R2 client is
explicitly constructed to not inherit ambient `AWS_*` environment variables, preventing accidental
use of real AWS credentials when R2 credentials are absent.

**Authorization model**: None beyond R2 bucket-level access. Whirlwind performs no application-level
access control. Both users share a single R2 bucket and are trusted equally. The lock protocol
provides coordination, not access restriction — either user can break any lock.

**Trust boundaries**:
- `config.toml` is the sole source of credentials. It is not committed to version control.
- Lock files and metadata written to R2 contain user-supplied identity strings (`identity.user`,
  `identity.machine` from config). These are not authenticated — any user with bucket access can
  write any identity claim. For a two-user trusted setup this is acceptable; it would not be
  acceptable for a multi-tenant system.

---

## Secret Management

**Credential storage**: `~/.config/whirlwind/config.toml`. Written with `0o600` permissions (owner
read/write only) on Unix via `std::os::unix::fs::PermissionsExt`. Windows does not apply this
restriction.

**In-process handling**: Credentials are stored in the `Config` struct as plain `String` values.
They are not zeroized on drop (no `zeroize` crate). For a local CLI tool run by its owner this is
an accepted tradeoff, but it means credentials persist in process memory for the lifetime of the
command invocation.

**Known gap — `init` wizard echoes secret key**: The `dialoguer::Input` prompt used to collect
the R2 Secret Access Key during `whirlwind init` echoes characters as they are typed. This should
use `dialoguer::Password` instead to mask terminal input and prevent credential exposure via
terminal scrollback or shoulder-surfing.

**Known gap — `$HOSTNAME` resolution on macOS**: `init` defaults the machine name from
`std::env::var("HOSTNAME")`, which is not reliably set in non-interactive macOS shells. This
produces `"unknown"` as the machine name, degrading the quality of lock messages.

**`.gitignore` credential exclusions** are not present in the current `.gitignore`. Before any
credentials or secrets are introduced to the working directory, add:

```
.env
.env.*
*.pem
*.key
*.p12
secrets/
```

---

## Lock Protocol Security Properties

The distributed lock relies on `If-None-Match: *` conditional PUT to R2. Key properties:

- **Atomic acquisition**: Cloudflare R2 honors `If-None-Match: *` (HTTP 412 on conflict), making
  lock acquisition a single atomic operation.
- **No authentication of lock contents**: Lock files contain `locked_by` and `machine` strings
  taken from config. These are trust-on-first-write — a malicious actor with bucket access could
  write any identity into a lock file. This is acceptable for the two-user trusted use case.
- **Self-lock detection**: The lock acquire logic distinguishes a lock held by the same
  user+machine combination (recoverable) from one held by the collaborator (contention). This is
  done by comparing `locked_by == config.identity.user && machine == config.identity.machine`.
- **Known gap — `DownloadFailed` swallows network errors in lock read**: `LockManager::read`
  currently maps all `AppError::DownloadFailed` variants to "no lock found." A transient network
  error reading the lock file will be misinterpreted as "lock is absent," potentially allowing a
  push to proceed when the lock is actually held. This requires distinguishing 404 Not Found from
  other download failures in the error type.
- **Stale lock threshold**: 4 hours (hard-coded). Advisory only — stale detection warns but does
  not automatically break the lock.

---

## Dependency Supply Chain

Current runtime dependencies and their security relevance:

| Crate | Purpose | Notes |
|---|---|---|
| `aws-sdk-s3` | R2 S3-compatible client | Official AWS SDK; TLS handled internally |
| `aws-config` | SDK config builder | Explicit override prevents ambient env var inheritance |
| `aws-credential-types` | Hardcoded credentials | `hardcoded-credentials` feature explicitly opted in |
| `tokio` | Async runtime | Full features; large attack surface but industry standard |
| `clap` | CLI parsing | Well-maintained |
| `serde` / `serde_json` / `toml` | Serialization | Deserializes user-supplied JSON from R2 (lock files, metadata) |
| `thiserror` | Error derive | No network access |
| `dirs` | Home dir resolution | Read-only filesystem access |
| `chrono` | Timestamps | No network access |
| `walkdir` | Directory traversal | Traverses user-specified `working_dir` |
| `dialoguer` | Interactive prompts | Input echoing concern noted above |
| `indicatif` | Progress bars | No security concern |
| `md-5` | MD5 hashing (Phase 3) | Pure Rust; no native dep; not yet used |
| `anyhow` | Error propagation | Currently unused — should be removed |

**`Cargo.lock`** should be committed to version control to enable reproducible builds and
`cargo audit` scanning.

**CI security checks** (not yet set up):
```yaml
- run: cargo audit         # CVE scanning
- run: cargo deny check    # license + advisory policy
- run: cargo clippy        # lint
```

---

## Unsafe Code

`#![forbid(unsafe_code)]` is enforced at the crate root in `src/lib.rs`. No `unsafe` blocks exist.

---

## Input Validation

**R2 object key injection**: Project names are used directly in R2 object key construction
(`locks/<project>.lock`, `projects/<project>/`). S3/R2 keys are opaque strings and do not perform
path traversal, but a project name containing `/` or `..` could produce confusing key structures.
CLI-layer validation should restrict project names to `[a-zA-Z0-9_-]` to prevent malformed keys.

**Config validation**: `Config::validate()` checks that all required string fields are non-empty
and that `working_dir` is not an empty path. It does not verify that `working_dir` is an absolute
path. A relative working directory would be resolved relative to the current working directory at
invocation time, which is a latent correctness issue.

**Deserialization of remote data**: Lock files and `metadata.json` are deserialized from R2 bytes
via `serde_json`. A malformed or intentionally corrupted file produces a descriptive error and
does not cause a crash. The `#![forbid(unsafe_code)]` attribute and Rust's memory safety guarantees
mean JSON parsing cannot cause memory corruption.

---

## Cryptography

No cryptographic operations are present in Phase 1. Phase 3 will add MD5 hashing (via `md-5`)
for ETag comparison. This is used for data integrity checking, not for security purposes —
MD5 is appropriate for ETag comparison (matching S3/R2 semantics) and should not be used for
any security-sensitive purpose.

TLS for R2 communication is handled entirely by `aws-sdk-s3` using the SDK's bundled TLS
implementation.

---

## Gaps Summary

| Gap | Severity | Status |
|---|---|---|
| `init` wizard echoes R2 secret key to terminal | High | Open — use `dialoguer::Password` |
| `DownloadFailed` conflation in `LockManager::read` allows silent lock bypass on network error | High | Open — needs `NotFound` distinction in error type |
| `--no-lock` flag has insufficient friction for a data-loss-capable bypass | Medium | Open — add runtime warning or confirmation |
| Project name not validated against injection characters (`/`, `..`) | Medium | Open |
| `working_dir` not validated as absolute path | Medium | Open |
| `Cargo.lock` not committed | Medium | Open |
| `.gitignore` lacks credential file exclusions | Medium | Open |
| No CI pipeline (`cargo audit`, `cargo clippy`) | Medium | Open |
| R2 secret not zeroized from memory on drop | Low | Accepted tradeoff for CLI tool |
| `$HOSTNAME` unreliable on macOS non-interactive shells | Low | Open |
| `anyhow` unused dependency (supply chain surface) | Low | Open — remove |
