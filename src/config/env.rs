//! Environment configuration (feature 32). Reads debugging options from the process environment:
//!
//! - `QS_MEMORY_QUARANTINE`: number of freed blocks to hold back (default 0).
//! - `QS_MEMORY_POISON`: `1` or `true` to poison freed memory (default off).
//!
//! `Settings::parse` takes the lookup as a parameter, so the parsing is testable without touching the
//! real environment.

/// Debugging options read from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub quarantine_blocks: usize,
    pub poison_on_free: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { quarantine_blocks: 0, poison_on_free: false }
    }
}

impl Settings {
    /// Reads the settings from the process environment.
    pub fn from_env() -> Self {
        Self::parse(|key| std::env::var(key).ok())
    }

    /// Parses the settings using `lookup` for each variable. Unset or malformed values keep the default.
    pub fn parse(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut settings = Settings::default();
        if let Some(n) = lookup("QS_MEMORY_QUARANTINE").and_then(|v| v.trim().parse::<usize>().ok()) {
            settings.quarantine_blocks = n;
        }
        if let Some(v) = lookup("QS_MEMORY_POISON") {
            settings.poison_on_free = matches!(v.trim(), "1") || v.trim().eq_ignore_ascii_case("true");
        }
        settings
    }
}
