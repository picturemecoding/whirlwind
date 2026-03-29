use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::sync::format_bytes;

// ---------------------------------------------------------------------------
// ProgressReporter
// ---------------------------------------------------------------------------

/// A container for multiple file-level progress bars rendered together.
pub struct ProgressReporter {
    multi: MultiProgress,
}

/// A progress bar scoped to a single file transfer.
pub struct FileProgressBar {
    bar: ProgressBar,
}

impl ProgressReporter {
    pub fn new() -> Self {
        Self {
            multi: MultiProgress::new(),
        }
    }

    /// Attach a new file-level progress bar to the multi-bar.
    ///
    /// `total_bytes` sets the bar length. `filename` is shown as the message,
    /// truncated to 40 characters.
    pub fn add_file_bar(&self, filename: &str, total_bytes: u64) -> FileProgressBar {
        let pb = self.multi.add(ProgressBar::new(total_bytes));

        let style = ProgressStyle::with_template(
            "{spinner:.green} {msg:40!} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec})",
        )
        // with_template returns a Result; fall back to default if the template is
        // ever somehow invalid (it won't be in practice, but unwrap_or avoids panic).
        .unwrap_or_else(|_| ProgressStyle::default_bar());

        pb.set_style(style);

        // Truncate the filename to 40 chars so the message column is stable.
        // Use char-count (not byte length) to avoid panicking on multi-byte UTF-8.
        let msg: String = if filename.chars().count() > 40 {
            filename.chars().take(40).collect()
        } else {
            filename.to_string()
        };
        pb.set_message(msg);

        FileProgressBar { bar: pb }
    }
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// FileProgressBar
// ---------------------------------------------------------------------------

impl FileProgressBar {
    /// Advance the bar to `bytes_transferred`.
    ///
    /// For Phase 1 (whole-file in-memory transfers) this is called once with
    /// the full file size after the transfer completes. Phase 2 can call it
    /// incrementally if streaming is added.
    pub fn update(&self, bytes_transferred: u64) {
        self.bar.set_position(bytes_transferred);
    }

    /// Replace the spinning bar with a static completion line and clear the bar.
    ///
    /// Renders: `  ✓ <filename> (<human_readable_size>)`
    pub fn finish(&self, filename: &str, total_bytes: u64) {
        let size_str = format_bytes(total_bytes);
        self.bar
            .finish_with_message(format!("✓ {:<40} ({})", filename, size_str));
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_progress_bar_finish_does_not_panic() {
        // Use a hidden MultiProgress so indicatif does not try to render to a
        // terminal during CI / test runs.
        let reporter = ProgressReporter::new();
        let bar = reporter.add_file_bar("test-file.wav", 1_048_576);
        bar.update(512_000);
        bar.update(1_048_576);
        // Must not panic.
        bar.finish("test-file.wav", 1_048_576);
    }

    #[test]
    fn add_file_bar_truncates_long_filename() {
        let reporter = ProgressReporter::new();
        // A filename longer than 40 characters; must not panic.
        let long_name = "a".repeat(80);
        let bar = reporter.add_file_bar(&long_name, 1024);
        bar.finish(&long_name, 1024);
    }

    #[test]
    fn add_file_bar_truncates_multibyte_utf8_filename() {
        // Each '日' is 3 bytes; 15 repetitions = 45 chars = 135 bytes.
        // Slicing by byte offset would panic; slicing by char count must not.
        let reporter = ProgressReporter::new();
        let long_name = "日".repeat(45);
        assert!(long_name.len() > 40, "sanity: byte length exceeds 40");
        assert!(
            long_name.chars().count() > 40,
            "sanity: char count exceeds 40"
        );
        let bar = reporter.add_file_bar(&long_name, 2048);
        bar.update(2048);
        bar.finish(&long_name, 2048);
    }
}
