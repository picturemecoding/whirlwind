# Whirlwind — Initial Design Notes

A collaborative Reaper project file sync tool for podcast co-editors, backed by Cloudflare R2.

---

## The Core Insight: Check-Out/Check-In, Not a Sync Daemon

A podcast editing session has a natural shape:

1. You sit down to edit episode 47
2. You open Reaper, spend 2 hours editing
3. You close Reaper, you're done

This is a **session**, not a continuous sync. The right primitive is **checkout/push**, not
watch-and-sync. The daemon model is Dropbox's answer to "I don't know when the user is done."
But we *do* know: they're done when Reaper exits.

**The single magic command:**

```bash
whirlwind session episode-47
# 1. Acquires a lock on episode-47 in R2
# 2. Downloads only changed/missing files
# 3. Launches Reaper pointing at the project
# 4. Waits for Reaper process to exit
# 5. Uploads only changed files
# 6. Releases the lock
```

---

## Concurrency: Lock Files with Conditional PUT

R2 (like S3) supports `If-None-Match: *` on PUT — "only write this object if it doesn't already
exist." This gives atomic lock acquisition:

```
PUT locks/episode-47.lock   (If-None-Match: *)
  → 200: you have the lock, proceed
  → 412: someone else has it, abort with a helpful message
```

The lock file contains JSON with who locked it and when:

```json
{ "locked_by": "alice", "locked_at": "2026-03-28T10:00:00Z", "machine": "alice-macbook" }
```

A `metadata.json` tracks project inventory and version history (updated best-effort after
successful push) but is NOT the concurrency mechanism.

---

## File Sync: Per-File, Not Archive

Reaper projects have:
- A `.rpp` text file (small, changes every session)
- Audio files: WAVs/AIF/MP3s (potentially GB-scale, rarely change after recording)

Tar+gz = re-uploading GBs of audio every session. Instead: sync per-file by comparing R2 ETag
(MD5 of the object) against local content hash. Only changed files go up/down. Common case
(edit the `.rpp`, no new audio) is very fast.

**Prerequisite for users**: configure Reaper to use relative paths for all media files.
Absolute paths break on a collaborator's machine. This is a one-time Reaper setup.

---

## R2 Bucket Layout

```
your-bucket/
  projects/
    episode-47/
      episode-47.rpp
      audio/
        intro.wav
        interview.wav
  locks/
    episode-47.lock          ← only exists when locked
  metadata.json              ← project inventory + version history
```

`metadata.json` schema:
```json
{
  "projects": {
    "episode-47": {
      "last_pushed_by": "alice",
      "last_pushed_at": "2026-03-28T10:00:00Z",
      "object_count": 3,
      "total_bytes": 847392810
    }
  }
}
```

---

## Authentication

One shared R2 API key (scoped to the specific bucket), stored in
`~/.config/whirlwind/config.toml` on each machine. The bucket is private (no public access).
IP restrictions are an optional additional layer via Cloudflare WAF.

---

## CLI Commands

```bash
whirlwind init                    # write config.toml, test R2 connection
whirlwind list                    # list projects, show lock status
whirlwind status [project]        # is it locked? by whom? last pushed when?
whirlwind pull <project>          # download without launching Reaper
whirlwind push <project>          # upload without the session wrapper
whirlwind session <project>       # MAIN: pull → Reaper → push
whirlwind unlock <project>        # emergency: break a stale lock
whirlwind new <name>              # starts a new reaper project from a template with audio files
```

`session` is the 95% case. The others are escape hatches.

---

## Build Phases

**Phase 1** — Manual push/pull, no Reaper integration:
- R2 client (aws-sdk-s3 with endpoint override for R2)
- `list`, `pull`, `push` with lock file concurrency
- `config.toml` with R2 credentials + local working directory + Reaper path

**Phase 2** — The magic command:
- `session`: process spawning, wait for Reaper exit, auto-push
- Progress display for uploads/downloads

**Phase 3** — Polish:
- `status` shows rich lock info
- Skip-unchanged file sync (compare ETags)
- `unlock` with confirmation prompt

**Phase 4** — Next Steps:
- `new` creates a new reaper episode-project
- Uses a Reaper template in the root level of the bucket `whirlwind/template`.
- SUPER NICE TO HAVE! Inserts all audio files in the directory into the template and automatically moves outro out to the end!

---

## Key Rust Crates

| Concern | Crate |
|---|---|
| S3/R2 client | `aws-sdk-s3` (with endpoint override for R2) |
| Async runtime | `tokio` |
| CLI | `clap` |
| Serialization | `serde` + `serde_json` |
| Config file | `toml` + `serde` |
| Progress bars | `indicatif` |
| Process spawning | `std::process::Command` |

---

## Explicit Non-Goals

- No CRDTs or real-time collaborative editing
- No web UI
- No database — `metadata.json` only
- No file watcher daemon
- No simultaneous editing support (lock model prevents it)
- No syncing of files outside the project directory (shared samples libraries are out of scope)
