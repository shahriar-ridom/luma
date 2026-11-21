//! Running helper programs. Always an argument list, never a shell string.

use std::{
    io::{self, Read},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdSpec {
    pub argv: Vec<String>,
    /// Needs administrator rights. [`StdRunner`] refuses these; they only run
    /// through the interactive pkexec runner after the user confirms.
    pub privileged: bool,
}

impl CmdSpec {
    pub fn user<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { argv: argv.into_iter().map(Into::into).collect(), privileged: false }
    }

    /// Display form for previews and confirm dialogs.
    pub fn display(&self) -> String {
        shell_words::join(&self.argv)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait Runner: Send + Sync {
    fn run(&self, cmd: &CmdSpec, cancel: &AtomicBool) -> io::Result<CmdOutput>;
}

/// Runs unprivileged commands as the current user, with a timeout and cancel.
pub struct StdRunner {
    pub timeout: Duration,
}

impl Default for StdRunner {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(120) }
    }
}

impl Runner for StdRunner {
    fn run(&self, cmd: &CmdSpec, cancel: &AtomicBool) -> io::Result<CmdOutput> {
        if cmd.privileged {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "privileged commands need the pkexec runner"));
        }
        let (prog, args) =
            cmd.argv.split_first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let mut child =
            Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let out = child.stdout.take().map(reader);
        let err = child.stderr.take().map(reader);
        let start = Instant::now();
        let code = loop {
            if let Some(st) = child.try_wait()? {
                break st.code();
            }
            let stop = if cancel.load(Ordering::Relaxed) {
                Some(io::ErrorKind::Interrupted)
            } else if start.elapsed() > self.timeout {
                Some(io::ErrorKind::TimedOut)
            } else {
                None
            };
            if let Some(kind) = stop {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(kind, "command stopped"));
            }
            thread::sleep(Duration::from_millis(20));
        };
        let join = |h: Option<thread::JoinHandle<String>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
        Ok(CmdOutput { code, stdout: join(out), stderr: join(err) })
    }
}

fn reader<R: Read + Send + 'static>(mut r: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Test double: records every command and answers from a script.
#[cfg(test)]
pub struct FakeRunner {
    pub calls: std::sync::Mutex<Vec<CmdSpec>>,
    pub reply: Box<dyn Fn(&CmdSpec) -> io::Result<CmdOutput> + Send + Sync>,
}

#[cfg(test)]
impl FakeRunner {
    pub fn ok() -> Self {
        Self { calls: Default::default(), reply: Box::new(|_| Ok(CmdOutput { code: Some(0), ..Default::default() })) }
    }
    pub fn calls(&self) -> Vec<CmdSpec> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Runner for FakeRunner {
    fn run(&self, cmd: &CmdSpec, _cancel: &AtomicBool) -> io::Result<CmdOutput> {
        self.calls.lock().unwrap().push(cmd.clone());
        (self.reply)(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_status() {
        let r = StdRunner::default();
        let o = r.run(&CmdSpec::user(["sh", "-c", "echo hi; echo oops >&2; exit 3"]), &AtomicBool::new(false)).unwrap();
        assert_eq!(o.code, Some(3));
        assert_eq!(o.stdout.trim(), "hi");
        assert_eq!(o.stderr.trim(), "oops");
        assert!(!o.success());
    }

    #[test]
    fn refuses_privileged_and_empty() {
        let r = StdRunner::default();
        let cancel = AtomicBool::new(false);
        let mut c = CmdSpec::user(["true"]);
        c.privileged = true;
        assert_eq!(r.run(&c, &cancel).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            r.run(&CmdSpec::user(Vec::<String>::new()), &cancel).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn timeout_and_cancel_kill_the_child() {
        let slow = CmdSpec::user(["sleep", "5"]);
        let r = StdRunner { timeout: Duration::from_millis(100) };
        assert_eq!(r.run(&slow, &AtomicBool::new(false)).unwrap_err().kind(), io::ErrorKind::TimedOut);
        let r = StdRunner::default();
        assert_eq!(r.run(&slow, &AtomicBool::new(true)).unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn display_quotes_arguments() {
        assert_eq!(CmdSpec::user(["rm", "a b"]).display(), "rm 'a b'");
    }
}
