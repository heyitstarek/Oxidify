//! Boot and frame diagnostics: mirrors key milestones to stderr and to a
//! log file so a blank window can be diagnosed without a debugger.
//!
//! Log location: `%TEMP%\oxidify-debug.log` (appended, UTF-8).

use std::io::Write;

fn log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("oxidify-debug.log")
}

/// Write one timestamped line to stderr and the debug log. Never panics.
pub fn diag(stage: &str) {
    let line = format!("[oxidify-diag] {stage}");
    eprintln!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{ms} {line}");
    }
}
