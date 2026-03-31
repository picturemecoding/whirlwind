use uuid::Uuid;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Returns true when a line opens a new block (`<KEYWORD ...`).
fn opens_block(line: &str) -> bool {
    line.trim_start().starts_with('<')
}

/// Returns true when a line is a bare closing tag (`>`), ending a block.
fn closes_block(line: &str) -> bool {
    line.trim() == ">"
}

/// Build the `<ITEM>` text to insert into a named track.
fn item_block(file_path: &str, duration_secs: f64, position_secs: f64) -> String {
    let iguid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let guid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file_path);
    format!(
        "    <ITEM\n      POSITION {position_secs}\n      SNAPOFFS 0\n      LENGTH {duration_secs}\n      LOOP 0\n      ALLTAKES 0\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      MUTE 0 0\n      SEL 0\n      IGUID {iguid}\n      IID 1\n      NAME {name}\n      VOLPAN 1 0 1 -1\n      SOFFS 0\n      PLAYRATE 1 1 0 -1 0 0.0025\n      CHANMODE 0\n      GUID {guid}\n      <SOURCE WAVE\n        FILE \"{file_path}\"\n      >\n    >"
    )
}

/// Scan forward from `start` (inclusive) and return the index of the line
/// that closes the block opened at `start` (depth returns to 0).
///
/// `<FXCHAIN` blocks are treated as opaque — depth is still tracked so we
/// find the correct closing `>`, but we do not look for named sub-blocks
/// inside them.
fn find_block_end(lines: &[&str], start: usize) -> usize {
    let mut depth: usize = 1; // we are already inside the block at `start`
    let mut i = start + 1;
    let mut inside_fxchain = false;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        if !inside_fxchain && trimmed.starts_with("<FXCHAIN") {
            inside_fxchain = true;
            depth += 1;
        } else if inside_fxchain {
            if trimmed == ">" {
                depth -= 1;
                if depth == 1 {
                    inside_fxchain = false;
                } else if depth == 0 {
                    return i;
                }
            } else if opens_block(lines[i]) {
                depth += 1;
            }
        } else if opens_block(lines[i]) {
            depth += 1;
        } else if closes_block(lines[i]) {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    // Malformed input — return last line index.
    lines.len().saturating_sub(1)
}

/// Find the first direct-child `<ITEM` block within a track, returning its
/// `(start_line, end_line)` range (inclusive). Returns `None` if absent.
fn find_direct_item(
    lines: &[&str],
    track_start: usize,
    track_end: usize,
) -> Option<(usize, usize)> {
    let mut depth: usize = 1;
    let mut inside_fxchain = false;

    for i in (track_start + 1)..=track_end {
        if i >= lines.len() {
            break;
        }
        let trimmed = lines[i].trim();

        if !inside_fxchain && trimmed.starts_with("<FXCHAIN") {
            inside_fxchain = true;
            depth += 1;
            continue;
        }

        if inside_fxchain {
            if trimmed == ">" {
                depth -= 1;
                if depth == 1 {
                    inside_fxchain = false;
                }
            } else if opens_block(lines[i]) {
                depth += 1;
            }
            continue;
        }

        if depth == 1 && trimmed.starts_with("<ITEM") {
            let item_end = find_block_end(lines, i);
            return Some((i, item_end));
        }

        if opens_block(lines[i]) {
            depth += 1;
        } else if closes_block(lines[i]) {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
    }
    None
}

/// Extract the track name value from a `NAME` attribute line (trimmed).
///
/// Handles both Reaper's quoted form (`NAME "track name"`) and unquoted form
/// (`NAME trackname`). Returns `None` if the line is not a NAME attribute.
fn parse_name_value(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("NAME ")?;
    if let Some(inner) = rest.strip_prefix('"') {
        inner.strip_suffix('"')
    } else {
        Some(rest)
    }
}

/// Return true if the TRACK block from `track_start` to `track_end` (inclusive)
/// contains a `NAME` line (quoted or unquoted) matching `track_name` at depth 1.
fn track_has_name(lines: &[&str], track_start: usize, track_end: usize, track_name: &str) -> bool {
    let mut depth: usize = 1;
    let mut inside_fxchain = false;

    for i in (track_start + 1)..=track_end {
        if i >= lines.len() {
            break;
        }
        let trimmed = lines[i].trim();

        if !inside_fxchain && trimmed.starts_with("<FXCHAIN") {
            inside_fxchain = true;
            depth += 1;
            continue;
        }

        if inside_fxchain {
            if trimmed == ">" {
                depth -= 1;
                if depth == 1 {
                    inside_fxchain = false;
                }
            } else if opens_block(lines[i]) {
                depth += 1;
            }
            continue;
        }

        if opens_block(lines[i]) {
            depth += 1;
        } else if closes_block(lines[i]) {
            depth -= 1;
            if depth == 0 {
                break;
            }
        } else if depth == 1
            && let Some(name) = parse_name_value(trimmed)
            && name == track_name
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Insert or replace the `<ITEM>` block in an existing named track.
///
/// - If the track has no existing `<ITEM>`, inserts one before the track's closing `>`.
/// - If the track already has an `<ITEM>` (e.g. a placeholder), replaces it.
/// - Returns the string unchanged if the named track is not found.
pub fn set_track_item(
    rpp: &str,
    track_name: &str,
    file_path: &str,
    duration_secs: f64,
    position_secs: f64,
) -> String {
    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();
    let mut out: Vec<String> = Vec::with_capacity(n + 20);
    let mut i = 0;

    while i < n {
        let line = lines[i];

        if line.trim_start().starts_with("<TRACK") {
            let track_end = find_block_end(&lines, i);

            if track_has_name(&lines, i, track_end, track_name) {
                let existing_item = find_direct_item(&lines, i, track_end);
                // Emit track lines, skipping any existing <ITEM> block.
                for (k, line) in lines.iter().enumerate().take(track_end).skip(i) {
                    if let Some((s, e)) = existing_item
                        && k >= s
                        && k <= e
                    {
                        continue;
                    }
                    out.push((*line).to_string());
                }
                // Insert new <ITEM> block before the closing >.
                let item = item_block(file_path, duration_secs, position_secs);
                for item_line in item.lines() {
                    out.push(item_line.to_string());
                }
                out.push(lines[track_end].to_string()); // closing >
            } else {
                for line in lines.iter().take(track_end + 1).skip(i) {
                    out.push(line.to_string());
                }
            }
            i = track_end + 1;
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }

    let mut result = out.join("\n");
    if rpp.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Update the `POSITION` of the `<ITEM>` in a named track (used for outro
/// placement). Returns the string unchanged if the track or item is not found.
pub fn set_item_position(rpp: &str, track_name: &str, position_secs: f64) -> String {
    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let mut i = 0;

    while i < n {
        let line = lines[i];

        if line.trim_start().starts_with("<TRACK") {
            let track_end = find_block_end(&lines, i);

            if track_has_name(&lines, i, track_end, track_name) {
                // Find the first <ITEM inside this track and update its POSITION.
                let mut depth: usize = 1;
                let mut j = i + 1;
                let mut inside_fxchain = false;
                let mut updated = false;
                let mut in_item = false;
                let mut item_depth: usize = 0;

                while j <= track_end && !updated {
                    let sl = lines[j];
                    let trimmed = sl.trim();

                    if !inside_fxchain && trimmed.starts_with("<FXCHAIN") {
                        inside_fxchain = true;
                        depth += 1;
                    } else if inside_fxchain {
                        if trimmed == ">" {
                            depth -= 1;
                            if depth == 1 {
                                inside_fxchain = false;
                            }
                        } else if opens_block(sl) {
                            depth += 1;
                        }
                    } else if !in_item && trimmed.starts_with("<ITEM") {
                        in_item = true;
                        item_depth = depth + 1;
                        depth += 1;
                    } else if in_item && depth == item_depth && trimmed.starts_with("POSITION") {
                        let indent = &sl[..sl.len() - sl.trim_start().len()];
                        out[j] = format!("{indent}POSITION {position_secs}");
                        updated = true;
                    } else if opens_block(sl) {
                        depth += 1;
                    } else if closes_block(sl) {
                        depth -= 1;
                        if in_item && depth < item_depth {
                            in_item = false;
                        }
                    }
                    j += 1;
                }
            }
            i = track_end + 1;
        } else {
            i += 1;
        }
    }

    let mut result = out.join("\n");
    if rpp.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Append new plain TRACK blocks (no FX chain) before the closing `>` of the
/// project root.
pub fn insert_tracks(rpp: &str, tracks: &[String]) -> String {
    if tracks.is_empty() {
        return rpp.to_string();
    }

    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();

    // Find the last bare `>` — the project root closing tag.
    let close_idx = match (0..n).rev().find(|&k| lines[k].trim() == ">") {
        Some(idx) => idx,
        None => return rpp.to_string(),
    };

    let mut out: Vec<String> = Vec::with_capacity(n + tracks.len() * 20);
    for line in lines.iter().take(close_idx) {
        out.push(line.to_string());
    }
    for track in tracks {
        for tl in track.lines() {
            out.push(tl.to_string());
        }
    }
    out.push(lines[close_idx].to_string());

    let mut result = out.join("\n");
    if rpp.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Set or replace the project end marker. Inserts if absent.
///
/// Marker line format: `MARKER 1 {end_secs} "End" 0 0 1 R {GUID} 0`
pub fn set_end_marker(rpp: &str, end_secs: f64) -> String {
    let guid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let new_marker = format!("MARKER 1 {end_secs} \"End\" 0 0 1 R {guid} 0");

    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();
    let mut replaced = false;
    let mut out: Vec<String> = Vec::with_capacity(n + 1);

    for line in &lines {
        if !replaced && line.trim().starts_with("MARKER 1 ") {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(format!("{indent}{new_marker}"));
            replaced = true;
        } else {
            out.push(line.to_string());
        }
    }

    if !replaced {
        // Insert before the final `>`.
        let close_idx = (0..out.len())
            .rev()
            .find(|&k| out[k].trim() == ">")
            .unwrap_or(out.len());
        out.insert(close_idx, new_marker);
    }

    let mut result = out.join("\n");
    if rpp.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Replace the `FILE "..."` path inside the `<SOURCE WAVE>` block of the first
/// `<ITEM>` in a named track.
///
/// Returns the string unchanged if the named track is not found or has no ITEM.
pub fn set_source_file(rpp: &str, track_name: &str, file_path: &str) -> String {
    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let mut i = 0;

    while i < n {
        let line = lines[i];
        if line.trim_start().starts_with("<TRACK") {
            let track_end = find_block_end(&lines, i);
            if track_has_name(&lines, i, track_end, track_name) {
                if let Some((item_start, item_end)) = find_direct_item(&lines, i, track_end) {
                    // Find <SOURCE block inside the ITEM.
                    let mut source_found = false;
                    let mut j = item_start + 1;
                    while j < item_end {
                        if lines[j].trim().starts_with("<SOURCE") {
                            source_found = true;
                            let source_end = find_block_end(&lines, j);
                            // Replace FILE line inside <SOURCE>.
                            let mut file_found = false;
                            for k in (j + 1)..source_end {
                                if k >= n {
                                    break;
                                }
                                let sl = lines[k];
                                if sl.trim().starts_with("FILE ") {
                                    let indent = &sl[..sl.len() - sl.trim_start().len()];
                                    out[k] = format!("{indent}FILE \"{file_path}\"");
                                    file_found = true;
                                    break;
                                }
                            }
                            if !file_found {
                                eprintln!(
                                    "Warning: set_source_file: no FILE line found in <SOURCE> for track '{track_name}'"
                                );
                            }
                            break;
                        }
                        j += 1;
                    }
                    if !source_found {
                        eprintln!(
                            "Warning: set_source_file: no <SOURCE> block found in <ITEM> for track '{track_name}'"
                        );
                    }
                }
                i = track_end + 1;
            } else {
                i = track_end + 1;
            }
        } else {
            i += 1;
        }
    }

    let mut result = out.join("\n");
    if rpp.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Return the LENGTH (in seconds) of the first `<ITEM>` in the named track.
///
/// Returns `0.0` if the track or item is not found or the LENGTH cannot be parsed.
pub fn get_track_item_length(rpp: &str, track_name: &str) -> f64 {
    let lines: Vec<&str> = rpp.lines().collect();
    let n = lines.len();
    let mut i = 0;

    while i < n {
        let line = lines[i];
        if line.trim_start().starts_with("<TRACK") {
            let track_end = find_block_end(&lines, i);
            if track_has_name(&lines, i, track_end, track_name) {
                if let Some((item_start, item_end)) = find_direct_item(&lines, i, track_end) {
                    for line in lines.iter().take(item_end + 1).skip(item_start + 1) {
                        if let Some(rest) = line.trim().strip_prefix("LENGTH ") {
                            return rest.parse::<f64>().unwrap_or(0.0);
                        }
                    }
                }
                eprintln!(
                    "Warning: get_track_item_length: no LENGTH found for track '{track_name}'"
                );
                return 0.0;
            }
            i = track_end + 1;
        } else {
            i += 1;
        }
    }
    0.0
}

/// Build a plain `<TRACK>` block (no FX chain) for an unmatched audio file.
///
/// The track name is derived from the filename stem. The GUID is a fresh UUID v4.
pub fn build_plain_track(file_path: &str, duration_secs: f64) -> String {
    let track_guid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let iguid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let item_guid = format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase());
    let stem = std::path::Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_path);
    let name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file_path);
    format!(
        "<TRACK {track_guid}\n  NAME {stem}\n  <ITEM\n    POSITION 0\n    SNAPOFFS 0\n    LENGTH {duration_secs}\n    LOOP 0\n    ALLTAKES 0\n    FADEIN 1 0 0 1 0 0 0\n    FADEOUT 1 0 0 1 0 0 0\n    MUTE 0 0\n    SEL 0\n    IGUID {iguid}\n    IID 1\n    NAME {name}\n    VOLPAN 1 0 1 -1\n    SOFFS 0\n    PLAYRATE 1 1 0 -1 0 0.0025\n    CHANMODE 0\n    GUID {item_guid}\n    <SOURCE WAVE\n      FILE \"{file_path}\"\n    >\n  >\n>"
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // set_track_item
    // -----------------------------------------------------------------------

    #[test]
    fn set_track_item_inserts_into_named_track() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "erik"
  <FXCHAIN
    SHOW 0
    <VST "VST3: ReaEQ" reaEQ.vst3 0 "" >
      BASE64BLOB==
    >
  >
>
>"#;
        let result = set_track_item(rpp, "erik", "audio/erik-ep42.wav", 3612.5, 0.0);
        assert!(
            result.contains(r#"FILE "audio/erik-ep42.wav""#),
            "FILE path missing:\n{result}"
        );
        assert!(
            result.contains("LENGTH 3612.5"),
            "LENGTH missing:\n{result}"
        );
        assert!(
            result.contains("<FXCHAIN"),
            "FXCHAIN was dropped:\n{result}"
        );
    }

    #[test]
    fn set_track_item_no_op_when_track_not_found() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "other-track"
>
>"#;
        let result = set_track_item(rpp, "erik", "audio/erik-ep42.wav", 3612.5, 0.0);
        assert_eq!(result, rpp, "Should return input unchanged");
    }

    #[test]
    fn set_track_item_replaces_existing_item() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "mike"
  <FXCHAIN
    SHOW 0
  >
  <ITEM
    POSITION 0
    LENGTH 30
    <SOURCE WAVE
      FILE "audio/placeholder.wav"
    >
  >
>
>"#;
        // Track has a placeholder <ITEM> — set_track_item must replace it.
        let result = set_track_item(rpp, "mike", "audio/ep42-mike.wav", 3600.0, 0.0);
        assert!(
            result.contains(r#"FILE "audio/ep42-mike.wav""#),
            "New FILE must appear:\n{result}"
        );
        assert!(
            !result.contains(r#"FILE "audio/placeholder.wav""#),
            "Old FILE must be gone:\n{result}"
        );
        assert!(
            result.contains("LENGTH 3600"),
            "New LENGTH must appear:\n{result}"
        );
        assert!(
            result.contains("<FXCHAIN"),
            "FXCHAIN must be preserved:\n{result}"
        );
        assert_eq!(
            result.matches("<ITEM").count(),
            1,
            "Exactly one <ITEM> expected:\n{result}"
        );
    }

    #[test]
    fn set_track_item_exact_name_match_does_not_match_prefix() {
        // looking for "erik" must not insert into a track named "erik-mic"
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "erik-mic"
>
>"#;
        let result = set_track_item(rpp, "erik", "audio/erik.wav", 100.0, 0.0);
        assert_eq!(
            result, rpp,
            "Should not match 'erik-mic' when searching for 'erik'"
        );
    }

    #[test]
    fn set_track_item_handles_unquoted_track_name() {
        // Reaper saves simple names without quotes: NAME erik (not NAME "erik")
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME erik
>
>"#;
        let result = set_track_item(rpp, "erik", "audio/ep42-erik.wav", 3600.0, 0.0);
        assert!(
            result.contains(r#"FILE "audio/ep42-erik.wav""#),
            "Should insert into unquoted-name track:\n{result}"
        );
    }

    // -----------------------------------------------------------------------
    // set_item_position
    // -----------------------------------------------------------------------

    #[test]
    fn set_item_position_updates_correct_track() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "outro"
  <ITEM
    POSITION 0
    LENGTH 30
  >
>
<TRACK {BBB}
  NAME "intro"
  <ITEM
    POSITION 0
    LENGTH 10
  >
>
>"#;
        let result = set_item_position(rpp, "outro", 55.5);
        assert!(
            result.contains("POSITION 55.5"),
            "Expected POSITION 55.5:\n{result}"
        );
        let count_55 = result.matches("POSITION 55.5").count();
        let count_0 = result.matches("POSITION 0").count();
        assert_eq!(count_55, 1, "Should only update one track:\n{result}");
        assert_eq!(count_0, 1, "Intro POSITION should remain 0:\n{result}");
    }

    // -----------------------------------------------------------------------
    // insert_tracks
    // -----------------------------------------------------------------------

    #[test]
    fn insert_tracks_appends_before_root_close() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "host"
>
>"#;
        let track = build_plain_track("audio/guest.wav", 1800.0);
        let tracks = vec![track];
        let result = insert_tracks(rpp, &tracks);

        let lines: Vec<&str> = result.lines().collect();
        let last = lines.last().copied().unwrap_or("");
        assert_eq!(last, ">", "Final line should be project root close");
        assert!(
            result.contains(r#"FILE "audio/guest.wav""#),
            "Inserted track missing:\n{result}"
        );
        assert!(
            !result.contains("<FXCHAIN"),
            "Plain tracks must not contain FXCHAIN:\n{result}"
        );
    }

    // -----------------------------------------------------------------------
    // set_end_marker
    // -----------------------------------------------------------------------

    #[test]
    fn set_end_marker_inserts_when_absent() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
<TRACK {AAA}
  NAME "host"
>
>"#;
        let result = set_end_marker(rpp, 3610.0);
        assert!(
            result.contains("MARKER 1 3610"),
            "Marker missing:\n{result}"
        );
    }

    #[test]
    fn set_end_marker_replaces_existing() {
        let rpp = r#"<REAPER_PROJECT 0.1 "6.0" 1234567890
MARKER 1 100 "End" 0 0 1 R {OLD-GUID} 0
<TRACK {AAA}
  NAME "host"
>
>"#;
        let result = set_end_marker(rpp, 3610.0);
        let marker_count = result.matches("MARKER 1 ").count();
        assert_eq!(
            marker_count, 1,
            "Should only have one MARKER 1 line:\n{result}"
        );
        assert!(
            result.contains("MARKER 1 3610"),
            "Updated marker missing:\n{result}"
        );
        assert!(
            !result.contains("MARKER 1 100"),
            "Old marker should be gone:\n{result}"
        );
    }
}
