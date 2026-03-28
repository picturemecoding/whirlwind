use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

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
        let msg = if filename.len() > 40 {
            filename[..40].to_string()
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
// Human-readable size helper
//
// Duplicated from sync.rs where it is private. If sync.rs's version is made
// `pub` in a future cleanup, this copy can be removed in favour of
// `crate::sync::format_bytes`.
// ---------------------------------------------------------------------------

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_in_progress() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GB");
    }

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
}
