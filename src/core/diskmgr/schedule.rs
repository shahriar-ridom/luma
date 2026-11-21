//! Settings for the recurring cleanup, and the systemd unit files that run it.
//!
//! Unattended runs only ever do user-level cleanup. The config is rejected if
//! it names anything else, so a hand-edited file cannot turn a timer into an
//! administrator task.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::Thresholds;

pub const SERVICE_NAME: &str = "luma-cleanup.service";
pub const TIMER_NAME: &str = "luma-cleanup.timer";
/// First line of every unit we write. A unit without it is never overwritten.
pub const MANAGED_MARK: &str = "# Managed by Luma. Changes made here are overwritten.";

/// What a scheduled run may clean, and the name used in the config file.
pub const SCHEDULABLE: &[(&str, &str)] = &[
    ("user-cache", "Application caches"),
    ("thumbnails", "Thumbnails"),
    ("x11-logs", "Old logs in your home folder"),
    ("trash", "The Trash"),
];

const DAYS: std::ops::RangeInclusive<u32> = 1..=3650;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Daily,
    Weekly,
    Monthly,
}

impl Interval {
    pub const ALL: [Interval; 3] = [Self::Daily, Self::Weekly, Self::Monthly];

    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "Every day",
            Self::Weekly => "Every week",
            Self::Monthly => "Every month",
        }
    }

    /// systemd's name for it.
    fn calendar(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CleanupConfig {
    pub enabled: bool,
    pub interval: Interval,
    /// Ids from [`SCHEDULABLE`].
    pub providers: Vec<String>,
    pub trash_days: u32,
    pub cache_days: u32,
    pub thumbnail_days: u32,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        let t = Thresholds::default();
        Self {
            enabled: false,
            interval: Interval::Weekly,
            // The Trash holds things people deleted on purpose and may still
            // want, so it is opt-in.
            providers: ["user-cache", "thumbnails", "x11-logs"].map(String::from).to_vec(),
            trash_days: t.trash_days,
            cache_days: t.cache_days,
            thumbnail_days: t.thumbnail_days,
        }
    }
}

impl CleanupConfig {
    /// Read and check a config file's text. An empty file is the default.
    pub fn parse(text: &str) -> Result<Self, String> {
        let c: Self =
            toml::from_str(text).map_err(|e| format!("The cleanup settings are not valid: {}", e.message()))?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        for id in &self.providers {
            if !SCHEDULABLE.iter().any(|(s, _)| s == id) {
                return Err(format!("“{id}” cannot be cleaned automatically."));
            }
        }
        for (name, d) in
            [("trash_days", self.trash_days), ("cache_days", self.cache_days), ("thumbnail_days", self.thumbnail_days)]
        {
            if !DAYS.contains(&d) {
                return Err(format!("{name} must be between {} and {}.", DAYS.start(), DAYS.end()));
            }
        }
        Ok(())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn thresholds(&self) -> Thresholds {
        Thresholds { trash_days: self.trash_days, cache_days: self.cache_days, thumbnail_days: self.thumbnail_days }
    }

    pub fn wants(&self, id: &str) -> bool {
        self.providers.iter().any(|p| p == id)
    }
}

/// Quote one argument for an `ExecStart=` line.
fn quote(arg: &str) -> String {
    let plain = !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c));
    if plain {
        return arg.to_string();
    }
    let escaped: String = arg
        .chars()
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            '"' => vec!['\\', '"'],
            // systemd expands these in unit files.
            '%' => vec!['%', '%'],
            '$' => vec!['$', '$'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

pub fn service_text(exe: &Path) -> String {
    format!(
        "{MANAGED_MARK}\n\
         [Unit]\n\
         Description=Luma scheduled cleanup\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} --cleanup-run\n\
         Nice=19\n\
         IOSchedulingClass=idle\n",
        quote(&exe.to_string_lossy())
    )
}

pub fn timer_text(interval: Interval) -> String {
    format!(
        "{MANAGED_MARK}\n\
         [Unit]\n\
         Description=Run the Luma cleanup {}\n\
         \n\
         [Timer]\n\
         OnCalendar={}\n\
         Persistent=true\n\
         RandomizedDelaySec=1h\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        interval.calendar(),
        interval.calendar()
    )
}

pub fn is_managed(unit_text: &str) -> bool {
    unit_text.lines().next().map(str::trim) == Some(MANAGED_MARK)
}

/// The program a managed service unit runs, so a moved or reinstalled Luma
/// can be noticed.
pub fn service_exe(unit_text: &str) -> Option<String> {
    let line = unit_text.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
    let words = shell_words::split(line).ok()?;
    // Undo systemd's `%%` and `$$` escapes, which quoting adds.
    words.into_iter().next().map(|w| w.replace("%%", "%").replace("$$", "$"))
}

/// What a timer unit says about its schedule.
pub fn timer_interval(unit_text: &str) -> Option<Interval> {
    let v = unit_text.lines().find_map(|l| l.strip_prefix("OnCalendar="))?.trim();
    Interval::ALL.into_iter().find(|i| i.calendar() == v)
}

/// What `systemctl --user show luma-cleanup.timer --property=...` said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimerInfo {
    /// The timer is loaded and waiting.
    pub active: bool,
    /// When it fires next, as systemd prints it.
    pub next: Option<String>,
}

pub const SHOW_PROPERTIES: &str = "ActiveState,NextElapseUSecRealtime";

pub fn parse_show(out: &str) -> TimerInfo {
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(str::trim);
    TimerInfo {
        active: get("ActiveState") == Some("active"),
        next: get("NextElapseUSecRealtime").filter(|v| !v.is_empty() && *v != "n/a").map(String::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_config_is_the_safe_default() {
        let c = CleanupConfig::parse("").unwrap();
        assert!(!c.enabled);
        assert_eq!(c.interval, Interval::Weekly);
        assert!(!c.wants("trash"), "the Trash is opt-in");
        assert!(c.wants("user-cache") && c.wants("thumbnails") && c.wants("x11-logs"));
    }

    #[test]
    fn round_trips_through_toml() {
        let c = CleanupConfig {
            enabled: true,
            interval: Interval::Monthly,
            providers: vec!["trash".into(), "thumbnails".into()],
            trash_days: 60,
            cache_days: 14,
            thumbnail_days: 120,
        };
        assert_eq!(CleanupConfig::parse(&c.to_toml()).unwrap(), c);
        assert_eq!(c.thresholds(), Thresholds { trash_days: 60, cache_days: 14, thumbnail_days: 120 });
    }

    #[test]
    fn rejects_anything_that_is_not_user_level() {
        for id in ["pkg-cache", "journal", "coredumps", "orphans", "large-files", "duplicates", "nope", ""] {
            let text = format!("providers = [\"{id}\"]");
            assert!(CleanupConfig::parse(&text).is_err(), "{id}");
        }
        assert!(CleanupConfig::parse("providers = [\"user-cache\", \"journal\"]").is_err());
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        assert!(CleanupConfig::parse("run_as_root = true").is_err());
        assert!(CleanupConfig::parse("interval = \"hourly\"").is_err());
        assert!(CleanupConfig::parse("cache_days = 0").is_err());
        assert!(CleanupConfig::parse("trash_days = 99999").is_err());
        assert!(CleanupConfig::parse("cache_days = -1").is_err());
        assert!(CleanupConfig::parse("enabled = [").is_err());
    }

    #[test]
    fn service_unit_is_gentle_and_marked() {
        let t = service_text(&PathBuf::from("/usr/bin/luma"));
        assert!(is_managed(&t));
        assert!(t.contains("ExecStart=/usr/bin/luma --cleanup-run\n"));
        assert!(t.contains("Type=oneshot") && t.contains("Nice=19") && t.contains("IOSchedulingClass=idle"));
        assert_eq!(service_exe(&t).as_deref(), Some("/usr/bin/luma"));
    }

    #[test]
    fn unusual_install_paths_are_quoted() {
        let t = service_text(&PathBuf::from("/opt/My Apps/lu\"ma%/bin"));
        assert!(t.contains("ExecStart=\"/opt/My Apps/lu\\\"ma%%/bin\" --cleanup-run\n"), "{t}");
        assert_eq!(service_exe(&t).as_deref(), Some("/opt/My Apps/lu\"ma%/bin"));
    }

    #[test]
    fn timer_unit_matches_the_interval() {
        for i in Interval::ALL {
            let t = timer_text(i);
            assert!(is_managed(&t));
            assert!(t.contains("Persistent=true") && t.contains("RandomizedDelaySec=1h"));
            assert!(t.contains("WantedBy=timers.target"));
            assert_eq!(timer_interval(&t), Some(i));
        }
        assert!(timer_text(Interval::Daily).contains("OnCalendar=daily\n"));
    }

    #[test]
    fn units_we_did_not_write_are_not_ours() {
        assert!(!is_managed("[Unit]\nDescription=mine\n"));
        assert!(!is_managed(""));
        // The mark has to be the first line, not just somewhere in the file.
        assert!(!is_managed(&format!("[Unit]\n{MANAGED_MARK}\n")));
    }

    #[test]
    fn reads_the_timer_state() {
        let t = parse_show("ActiveState=active\nNextElapseUSecRealtime=Sun 2026-11-22 03:12:00 +06\n");
        assert_eq!(t, TimerInfo { active: true, next: Some("Sun 2026-11-22 03:12:00 +06".into()) });
        assert_eq!(parse_show("ActiveState=inactive\nNextElapseUSecRealtime=\n"), TimerInfo::default());
        assert_eq!(parse_show("ActiveState=active\nNextElapseUSecRealtime=n/a\n").next, None);
        assert_eq!(parse_show(""), TimerInfo::default());
    }

    #[test]
    fn snapshot_of_the_weekly_timer() {
        assert_eq!(
            timer_text(Interval::Weekly),
            "# Managed by Luma. Changes made here are overwritten.\n[Unit]\nDescription=Run the Luma cleanup weekly\n\n[Timer]\nOnCalendar=weekly\nPersistent=true\nRandomizedDelaySec=1h\n\n[Install]\nWantedBy=timers.target\n"
        );
    }
}
