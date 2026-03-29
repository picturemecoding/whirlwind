# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

**whirlwind** is a collaborative Reaper project file sync tool for podcast co-editors, backed by Cloudflare R2.

## Commands

This project uses [`just`](https://github.com/casey/just) as a task runner and `cargo nextest` for tests.

```bash
just check                # fmt check + clippy + lint (always runs before merging)
just test                 # run all tests
just test <filter>        # run a single test or filtered subset
just fmt                  # auto-format code
just build                # release build
```

### Auxiliary Tooling

This project uses `bmo` for issue tracking and the terminal environment has the following tools available also: `rg` and `jq`. Thus, we can pipe `bmo` command with `--json` option outputs to `jq` to parse values we need to proceed.

## Code Quality Requirements

**Important**: all source code changes must go through the following steps!

1. `just fmt`
2. `just check`
3. `just test`

### Code Quality Rules

- Always place imports (`use` statements) at the top of a module, NEVER INSIDE FUNCTION BODIES.
- Perfer simple, readable code, no tornado code, no overlong functions.
- Prefer functional patterns (map, filter) and pattern-matching.

## Architecture

`whirlwind` is a CLI tool for collaborative Reaper DAW project sync backed by Cloudflare R2.

Two users share a project directory in R2 and coordinate edits through a distributed lock protocol built on `If-None-Match: *` conditional PUT. The tool is a locally-installed binary; there is no server-side component.

- `main.rs` / `cli.rs`: parse command-line args and dispatch commands.
- `config.rs`: load and validate local config with Cloudflare R2 connection info and other identifying info.
- `session.rs` / `lock.rs`: lock an episode directory (on the R2 object storage) and start Reaper session (unlock R2 object paths on exit).
- `sync.rs`: orchestrate sync behavior and conflict handling.
- `r2.rs`: Cloudflare R2 interactions (upload/download/list, conditional ops).

## Integration tests

Integration tests live in `tests/` and should exercise real command/core behavior.

- Keep fast tests default: use temp dirs and local fixtures for most coverage.
- No external calls: use mocks.
- Prefer deterministic fixtures over random data.
- Keep assertions user-meaningful (files, metadata, exit/result states).
