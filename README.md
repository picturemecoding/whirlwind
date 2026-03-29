# whirlwind

whirlwind is a collaborative Reaper project sync tool for podcast co-editors. It helps a small team keep project files in sync through Cloudflare R2-backed storage.

## Reaper Users

This project uses local paths for media. To make sure you are using local media paths in Reaper, see this recommendation from the docs:

> In **Options > Preferences > Project** and check **"Save project file references with relative pathnames"**. This ensures all media files are stored within the project folder.

## Command Reference

```sh
$ whirlwind help
Collaborative Reaper project sync for podcasters

Usage: whirlwind <COMMAND>

Commands:
  init     Initialize whirlwind config and test R2 connection
  list     List all projects and their lock/push status
  status   Show status of a project (lock info, last push)
  pull     Download a project from R2 to local working directory
  push     Upload local project changes to R2
  session  Pull project, launch Reaper, push on exit
  unlock   Break a stale lock on a project
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version

```

## Purpose

- Keep Reaper project state aligned across collaborators.
- Provide a reliable sync workflow for podcast editing sessions.
- Reduce manual file handoffs between co-editors.

## Common Workflows

This project uses just as the task runner for everyday development commands:

- just test: run all tests
- just test <filter>: run a filtered subset of tests
- just fmt: auto-format code
- just check: run formatting checks, clippy, and linting
- just build: create a release build

## Stack

- Rust for the CLI and core sync logic
- Cloudflare R2 for remote object storage
- Reaper project files as the collaboration target
