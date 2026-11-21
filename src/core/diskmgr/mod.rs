//! Disk manager: cleanup suggestions, safe execution, health checks.
//!
//! Pure logic only (std + serde). Nothing here touches widgets, so the same
//! code serves the dialog and the headless `--cleanup-run` mode. Anything that
//! deletes goes through [`safety::PathPolicy`]; anything that runs a program
//! goes through a [`exec::Runner`] as an argument list, never a shell string.

pub mod devices;
pub mod distro;
pub mod exec;
pub mod fscheck;
pub mod headless;
pub mod privileged;
pub mod providers;
pub mod safety;
pub mod schedule;
pub mod smart;
pub mod tools;

use std::{path::PathBuf, sync::atomic::AtomicBool};

pub use distro::Distro;
pub use exec::{CmdSpec, Runner};
pub use tools::ToolSet;

/// How much care a suggestion needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    /// Regenerated automatically (caches, thumbnails).
    Safe,
    /// The user should look at the items first.
    Review,
    /// Never executed by Luma; only a command to copy is offered.
    SuggestOnly,
}

/// Who may run it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    /// Needs administrator rights, so only interactive and only via pkexec.
    Root,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub path: Option<PathBuf>,
    pub bytes: u64,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    /// Stable id such as `pkg-cache`; also the name used in the schedule config.
    pub id: String,
    pub title: String,
    pub detail: String,
    pub est_bytes: Option<u64>,
    pub risk: Risk,
    pub scope: Scope,
    pub items: Vec<Item>,
    /// For `SuggestOnly`: the command to show the user.
    pub hint: Option<Vec<String>>,
}

/// One step of a plan. A preview is just this list rendered, and a run
/// executes the same list, so the two cannot diverge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Permanent delete (links are removed, never followed).
    RemovePath(PathBuf),
    /// Move to the Trash: reversible, so used for files the user's own data.
    TrashPath(PathBuf),
    /// A command; privileged ones run only through the pkexec runner.
    Exec(CmdSpec),
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Bytes deleted for good.
    pub freed: u64,
    /// Bytes moved to the Trash; the space comes back when it is emptied.
    pub trashed: u64,
    /// Commands that ran and succeeded.
    pub commands_ok: u32,
    pub errors: Vec<String>,
    pub skipped: Vec<String>,
}

impl Outcome {
    pub fn merge(&mut self, other: Outcome) {
        self.freed += other.freed;
        self.trashed += other.trashed;
        self.commands_ok += other.commands_ok;
        self.errors.extend(other.errors);
        self.skipped.extend(other.skipped);
    }
}

/// How old things must be before they are offered, in days. The 24 hour
/// floor in [`safety::MIN_AGE_FLOOR`] applies on top of every value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub trash_days: u32,
    pub cache_days: u32,
    pub thumbnail_days: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { trash_days: 30, cache_days: 30, thumbnail_days: 90 }
    }
}

/// What a provider may look at.
pub struct Env<'a> {
    pub home: PathBuf,
    /// `$XDG_DATA_HOME` (holds the Trash).
    pub data_dir: PathBuf,
    /// `$XDG_CACHE_HOME`.
    pub cache_dir: PathBuf,
    /// Where system folders (`var/cache`, `var/log`) are looked up: `/`.
    pub sys: PathBuf,
    pub distro: Distro,
    pub tools: ToolSet,
    pub thresholds: Thresholds,
    pub runner: &'a dyn Runner,
}

impl<'a> Env<'a> {
    /// The real environment of the current user.
    pub fn detect(runner: &'a dyn Runner) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let xdg = |var: &str, rel: &str| {
            std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(rel))
        };
        Self {
            data_dir: xdg("XDG_DATA_HOME", ".local/share"),
            cache_dir: xdg("XDG_CACHE_HOME", ".cache"),
            sys: PathBuf::from("/"),
            distro: Distro::detect(),
            tools: ToolSet::detect(),
            thresholds: Thresholds::default(),
            home,
            runner,
        }
    }

    /// A self-contained environment rooted at `home`, for tests.
    #[cfg(test)]
    pub fn rooted(home: PathBuf, runner: &'a dyn Runner) -> Self {
        Self {
            data_dir: home.join(".local/share"),
            cache_dir: home.join(".cache"),
            sys: home.join("sysroot"),
            distro: distro::parse_os_release(""),
            tools: ToolSet::default(),
            thresholds: Thresholds::default(),
            home,
            runner,
        }
    }
}

pub trait CleanupProvider: Send + Sync {
    fn id(&self) -> &'static str;
    /// Gate on distro and installed tools.
    fn available(&self, env: &Env) -> bool;
    fn scan(&self, env: &Env, cancel: &AtomicBool) -> Vec<Suggestion>;
    /// Dry run: no side effects.
    fn plan(&self, selection: &[Item], env: &Env) -> Plan;
    fn run(&self, plan: &Plan, env: &Env, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Outcome;
}
