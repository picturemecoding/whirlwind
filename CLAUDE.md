# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

**whirlwind** is a collaborative Reaper project file sync tool for podcast co-editors, backed by Cloudflare R2.

## Commands

This project uses [`just`](https://github.com/casey/just) as a task runner and `cargo nextest` for tests.

```bash
just test                     # run all tests
just test <filter>            # run a single test or filtered subset
just fmt                      # auto-format code
just check                    # fmt check + clippy + lint (always runs before merging)
just build                    # release build
```

## Validating Source Code Changes

**Important**: all source code changes must go through the following steps!

1. `just fmt`
2. `just check`
3. `just test`

## Architecture

...TO DO...

## Key design patterns

...TO DO...

## Integration tests

...TO DO...
