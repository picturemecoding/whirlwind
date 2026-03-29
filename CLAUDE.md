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

## Validating Source Code Changes

**Important**: all source code changes must go through the following steps!

1. `just fmt`
2. `just check`
3. `just test`

## Architecture

`whirlwind` is a small Rust CLI with a library-style core in `src/`.

- `main.rs` / `cli.rs`: parse command-line args and dispatch commands.
- `config.rs`: load and validate local config.
- `session.rs`: lock an episode directory (using R2 object storage) and start Reaper session (unlock on exit).
- `sync.rs`: orchestrate sync behavior and conflict handling.
- `r2.rs`: Cloudflare R2 interactions (upload/download/list, conditional ops).

## Key design patterns

Use these patterns consistently when adding features:

...

## Integration tests

Integration tests live in `tests/` and should exercise real command/core behavior.

- Keep fast tests default: use temp dirs and local fixtures for most coverage.
- No external calls: use mocks.
- Prefer deterministic fixtures over random data.
- Keep assertions user-meaningful (files, metadata, exit/result states).
