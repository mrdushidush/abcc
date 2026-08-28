//! The tool child: drained from the first byte, scrubbed of the operator's
//! environment, killable from the control thread, and honest about what confined
//! it.
//!
//! # Why the pipes are drained by threads and not by `wait()`
//!
//! F202's donor defect, in one sentence: **a child that writes past the 64 KiB
//! pipe buffer blocks on `write`, and is recorded as a *timeout* for a tool that
//! had finished its work.** A test suite printing its own output is exactly that
//! shape. So both pipes are drained from the moment the child starts, by threads
//! that keep reading **even after the capture cap is reached** — stopping the
//! read to save memory would recreate the defect the cap was not the point of.
//!
//! ⚠ Two drainer threads per running tool and no more. ADR-0006 counts the
//! process's threads rather than hand-waving them, and its inventory says *≤4,
//! two per running tool, only while one runs*. A waiter thread would make it
//! three; `wait_timeout` on the calling thread is why it does not have to.
//!
//! # What actually confines it
//!
//! Nothing but the worktree. A `runas /trustlevel:0x20000` child stays at Medium
//! integrity, writes the user's home directory and opens TCP; WSL2's
//! `binfmt_misc` hands any PE file back to the Windows host to execute; the
//! workspace crossing a VM boundary costs ~290× and the process spawn itself is
//! cheap (F408–F411). **There is no containment on this platform without Win32
//! token code**, and this workspace forbids `unsafe`.
//!
//! So every child reports [`Confinement::Cwd`], and ADR-0014 §3 requires that to
//! be a value the console shows rather than a footnote: **the posture is blast
//! radius, not a boundary.**

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use abcc_core::outcome::Why;
use shared_child::SharedChild;

use crate::tools::Confinement;

/// The most output kept per stream. Beyond it the drainer keeps reading and
/// stops storing: the point of the cap is memory, and the point of the read is
/// that the child never blocks. Those are different problems and conflating them
/// is F202.
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// How long to wait for a drainer to finish after the child has exited.
///
/// It is not a timeout on the tool; the tool has already ended. It bounds the
/// case where a grandchild inherited the pipe and holds it open after its parent
/// is gone — which no signal on this platform reliably resolves, so the honest
/// move is to bound the wait and report what was captured.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// 🚨 **The environment a tool child gets, as data in one const.**
///
/// ADR-0014 §5: adopt `env_clear()` plus an allowlist — **one donor of three
/// strips anything.** Everything not named here is absent from the child,
/// including every credential the operator's shell happens to be carrying.
///
/// The toolchain entries are here because the workload is *build and test an
/// existing repository*: without `CARGO_HOME` and `RUSTUP_*` a cargo invocation
/// re-resolves against a home directory it cannot find. They are named
/// individually rather than admitted by prefix, because a prefix rule is how an
/// allowlist stops being one.
pub const ENV_ALLOWLIST: &[&str] = &[
    // Both platforms.
    "PATH",
    "LANG",
    "LC_ALL",
    "TZ",
    // Windows needs these to start a process at all.
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    // Unix equivalents.
    "HOME",
    "TMPDIR",
    "USER",
    // The toolchain, named one at a time.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// What to run, where, and for how long.
#[derive(Debug, Clone)]
pub struct Spawn {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    budget: Duration,
}

impl Spawn {
    /// 🚨 `cwd` is the attempt's worktree and never the operator's checkout. It
    /// is required rather than defaulted, because inheriting the caller's working
    /// directory is the one mistake that turns the blast radius into the whole
    /// machine.
    #[must_use]
    pub fn new(program: impl Into<OsString>, cwd: impl Into<PathBuf>) -> Spawn {
        Spawn {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            budget: Duration::from_mins(2),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Spawn {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Spawn
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// How long the host waits before it stops waiting. ⚠ A timeout folded into
    /// "clean" is the worst available lie, because the process may still be alive
    /// (F220) — so exceeding this produces [`Why::Timeout`] and never an exit
    /// status.
    #[must_use]
    pub fn budget(mut self, budget: Duration) -> Spawn {
        self.budget = budget;
        self
    }

    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

/// A running tool child.
pub struct ToolChild {
    child: Arc<SharedChild>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    drained: mpsc::Receiver<()>,
    started: Instant,
    budget: Duration,
    killed_by: Arc<OnceLock<String>>,
}

/// A handle that can end the child from another thread.
///
/// ADR-0006 names one dependency for this and one reason: **kill a tool child
/// from the control thread.** [`crate::control::ControlPoint`] is what holds it
/// while the worker is inside a tool call.
#[derive(Clone)]
pub struct Killer {
    child: Arc<SharedChild>,
    killed_by: Arc<OnceLock<String>>,
}

impl Killer {
    /// End the child, on the record.
    ///
    /// 🚨 `by` is required because a killed child must not be reported as a
    /// failed one. `TerminateProcess` gives the child exit code 1 and a run that
    /// recorded that would be saying *the tests failed* about work an operator
    /// stopped — so [`ToolChild::finish`] reads this instead and answers
    /// [`Why::Cancelled`], which is neither a pass nor a failure. The first
    /// killer wins; a second call does not overwrite the reason.
    ///
    /// ⚠ It ends *this* child. A grandchild the tool started is not reached by
    /// it, and this platform offers no reliable way to reach one without Win32
    /// job-object code — the same fact that makes the posture blast radius rather
    /// than a boundary.
    ///
    /// # Errors
    ///
    /// Whatever the OS says. A child that has already exited is not an error.
    pub fn kill(&self, by: impl Into<String>) -> std::io::Result<()> {
        let _ = self.killed_by.set(by.into());
        self.child.kill()
    }

    /// The process id, for the console and for an operator with a task manager.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

/// What the host watched the child do.
///
/// The fields line up with `ToolCallEnded` one for one, so recording a tool call
/// is a move rather than a translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// `None` when the process ended without one — killed, or stopped by a
    /// signal.
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed_ms: u64,
    pub confinement: Confinement,
    /// 🚨 Set when the child produced **no measurable ending** — the class, never
    /// a generic failure. A `Finished` with `unmeasured: Some(..)` is not a
    /// result with a bad exit code; it is an absence, and the report type has a
    /// place for exactly that.
    pub unmeasured: Option<Why>,
}

impl ToolChild {
    /// Start the child with both pipes already being drained.
    ///
    /// # Errors
    ///
    /// [`Why::CheckerNotOnHost`] when the program is not there, and
    /// [`Why::SpawnFailed`] for anything else the OS refused. These are two
    /// values because they are two different things to tell an operator, and
    /// because a name that resolves is not a working interpreter (F312) — the
    /// only honest probe is to execute.
    pub fn spawn(spec: &Spawn) -> Result<ToolChild, Why> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        scrub_env(&mut command);

        let child = SharedChild::spawn(&mut command).map_err(|e| {
            let binary = spec.program.to_string_lossy().into_owned();
            if e.kind() == std::io::ErrorKind::NotFound {
                Why::CheckerNotOnHost { binary }
            } else {
                Why::SpawnFailed {
                    binary,
                    os_error: e.to_string(),
                }
            }
        })?;
        let child = Arc::new(child);

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let (done_tx, drained) = mpsc::channel();

        // Both drainers start before anything else happens, which is the whole
        // point: the window in which the child could fill a 64 KiB buffer is
        // never open.
        if let Some(pipe) = child.take_stdout() {
            spawn_drainer(pipe, Arc::clone(&stdout), done_tx.clone());
        }
        if let Some(pipe) = child.take_stderr() {
            spawn_drainer(pipe, Arc::clone(&stderr), done_tx);
        }

        Ok(ToolChild {
            child,
            stdout,
            stderr,
            drained,
            started: Instant::now(),
            budget: spec.budget,
            killed_by: Arc::new(OnceLock::new()),
        })
    }

    /// A handle for the control thread.
    #[must_use]
    pub fn killer(&self) -> Killer {
        Killer {
            child: Arc::clone(&self.child),
            killed_by: Arc::clone(&self.killed_by),
        }
    }

    /// Wait for the child, within its budget, and collect what it said.
    ///
    /// ⚠ On a timeout the child is killed and the output captured **so far** is
    /// returned. That partial output is usually the most useful thing an operator
    /// can be shown about a hang, and discarding it to keep the return type tidy
    /// would be throwing away the evidence.
    #[must_use]
    pub fn finish(self) -> Finished {
        let waited = self.child.wait_timeout(self.budget);
        let (exit, unmeasured) = match waited {
            Ok(Some(status)) => {
                self.await_drainers();
                // 🚨 A stopped child is not a failed one. The operator's abort
                // arrives here as an exit code of 1 from `TerminateProcess`, and
                // recording that would be the report saying *the tests failed*
                // about work nobody ran.
                match self.killed_by.get() {
                    Some(by) => (None, Some(Why::Cancelled { by: by.clone() })),
                    None => (status.code(), None),
                }
            }
            Ok(None) => {
                // The budget is spent. Kill first, then reap, so the pipes close
                // and the drainers can finish.
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.await_drainers();
                let after_ms = elapsed_ms(self.started);
                (None, Some(Why::Timeout { after_ms }))
            }
            Err(e) => {
                let _ = self.child.kill();
                self.await_drainers();
                (
                    None,
                    Some(Why::EngineError {
                        detail: format!("waiting on the tool child: {e}"),
                    }),
                )
            }
        };

        Finished {
            exit,
            stdout: take_text(&self.stdout),
            stderr: take_text(&self.stderr),
            elapsed_ms: elapsed_ms(self.started),
            confinement: achieved_confinement(),
            unmeasured,
        }
    }

    /// Give the drainers a bounded moment to reach EOF now that the child is
    /// gone. Bounded rather than joined, because a grandchild holding the pipe
    /// would otherwise hang the worker on cleanup.
    fn await_drainers(&self) {
        let deadline = Instant::now() + DRAIN_GRACE;
        for _ in 0..2 {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.drained.recv_timeout(left) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

/// 🚨 What actually confined the child, on this platform, today.
///
/// [`Confinement::Cwd`] and nothing else. Reporting anything stronger would be a
/// claim rather than a measurement, and ADR-0014's falsifier is *a supported
/// confinement primitive appears* — which is why the stronger arm exists and why
/// nothing returns it yet.
const fn achieved_confinement() -> Confinement {
    Confinement::Cwd
}

/// `env_clear()` plus the allowlist, applied in that order so a variable can only
/// be present by being named.
fn scrub_env(command: &mut Command) {
    command.env_clear();
    for key in ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn spawn_drainer<R>(mut pipe: R, into: Arc<Mutex<Vec<u8>>>, done: mpsc::Sender<()>)
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut held) = into.lock() {
                        // Keep reading past the cap; only stop *storing*. The
                        // read is what keeps the child unblocked, and that is
                        // not the thing the cap is for.
                        let room = MAX_CAPTURE_BYTES.saturating_sub(held.len());
                        if room > 0 {
                            held.extend_from_slice(&buf[..n.min(room)]);
                        }
                    }
                }
            }
        }
        let _ = done.send(());
    });
}

fn take_text(held: &Arc<Mutex<Vec<u8>>>) -> String {
    match held.lock() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // A poisoned lock means a drainer panicked. There is nothing to recover
        // and nothing worth pretending: say so where the operator will see it.
        Err(_) => String::from("<output lost: the drainer thread panicked>"),
    }
}

fn elapsed_ms(from: Instant) -> u64 {
    u64::try_from(from.elapsed().as_millis()).unwrap_or(u64::MAX)
}
