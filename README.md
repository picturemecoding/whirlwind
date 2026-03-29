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
  new      Create a new episode project from a Reaper template
  unlock   Break a stale lock on a project
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version

```

## Creating a New Episode Project

`whirlwind new` automates Reaper project setup for a new episode. It downloads your team's shared template from R2, wires in the recorded audio files, sets the project end marker, and pushes the result back to R2 — ready to open in Reaper.

### Workflow

1. Copy the recorded WAV files into a new episode directory:
   ```sh
   mkdir -p ~/podcast/episodes/ep96-database-history
   cp /path/to/recordings/*.wav ~/podcast/episodes/ep96-database-history/
   ```

2. Run `whirlwind new`:
   ```sh
   whirlwind new ep96-database-history
   ```

3. Open the session in Reaper:
   ```sh
   whirlwind session ep96-database-history
   ```

### How it works

- Downloads `templates/default.rpp` (or a named template) from R2
- Reads `[[new.tracks]]` entries from `~/.config/whirlwind/config.toml` — maps filename glob patterns to named tracks in the template (e.g. `*_erik_*.wav` → `erik-mic`)
- Matched audio files are inserted into the corresponding template track, preserving its EQ and plugin chain
- Unmatched files (guests) are appended as plain tracks
- The outro track position is set to `project_end - 3s`
- The project end marker is set to `max(track_durations) - trim_seconds`
- The resulting `.rpp` is pushed to R2

### Options

```
whirlwind new <episode-name> [OPTIONS]

Arguments:
  <episode-name>  Name of the episode directory under your working_dir

Options:
  --template <name>         Template to use (default: from config, else "default")
  --trim-seconds <secs>     Seconds to trim from project end (default: from config, else 0)
  --assign <TRACK=FILE>     Assign a file to a named track (repeatable)
  --dry-run                 Show what would happen without writing or pushing anything
```

Use `--assign` to handle filenames that don't match your configured patterns:

```sh
whirlwind new ep96-database-history \
  --assign "erik=riverside_ERIKLONGNAME_raw-audio_ep96.wav" \
  --assign "mike=riverside_MIKELONGNAME_raw-audio_ep96.wav"
```

### Uploading your template

Before using `whirlwind new`, upload your Reaper template to R2:

```sh
aws s3 cp episode-base-template.rpp \
  s3://<bucket>/templates/default.rpp \
  --endpoint-url https://<account_id>.r2.cloudflarestorage.com
```

The template should have empty `<TRACK>` blocks (no `<ITEM>`) for host mic tracks, and fully configured items for intro/outro tracks.

### Config

Add an optional `[new]` section to `~/.config/whirlwind/config.toml` to set defaults and configure track matching:

```toml
[new]
default_template = "default"   # template name in R2
trim_seconds = 2.0             # trim this many seconds from project end

[[new.tracks]]
track = "erik-mic"             # track name in the Reaper template
pattern = "*_erik_*.wav"       # glob pattern matched against the audio filename

[[new.tracks]]
track = "mike-mic"
pattern = "*_mike_*.wav"
```

`[[new.tracks]]` patterns are matched in order — the first match wins. Use `--assign` to override per-run when a filename doesn't match your usual patterns.

## Purpose

- Keep Reaper project state aligned across collaborators.
- Provide a reliable sync workflow for podcast editing sessions.
- Reduce manual file handoffs between co-editors.

## Common Workflows

This project uses just as the task runner for everyday development commands:

- `just test`: run all tests
- `just test <filter>`: run a filtered subset of tests
- `just fmt`: auto-format code
- `just check`: run formatting checks, clippy, and linting
- `just build`: create a release build

## Stack

- Rust for the CLI and core sync logic
- Cloudflare R2 for remote object storage
- Reaper project files as the collaboration target
