//! The tool layer's working half: the five tools that touch files and the four
//! that start a child, over one real directory.
//!
//! [`crate::tools`] decides *what a role may reach for*; this is what happens
//! when it may. The policy check has already run when [`Tools::run`] is called —
//! the `spec` is the proof of it — so nothing here re-decides it. **Two places
//! deciding one thing is how the donor ended up with a path check that is not in
//! the path.**
//!
//! # The argument check, and why its wording matters
//!
//! 🚨 Every path a model hands a file tool is resolved against the workspace root
//! and refused if it lands outside. **That is an argument check and never the
//! sandbox** (ADR-0014): it binds the tool that has an argument, and `bash` walks
//! straight past it.
//!
//! What it is worth is measured. In the 40-task battery the subject put its
//! artifact where the verifier grades it **in exactly half of 80 attempts**, and
//! the other half is one defect wearing three hats: the subject hands
//! `write_file` an absolute path with the work-dir component missing, the check
//! correctly refuses, **and the subject routes around the refusal with `bash`,
//! which has no path gate at all** (F400–F403). Placement was a per-attempt coin
//! flip rather than a property of the task.
//!
//! So a refusal here is written to be *acted on*: it names the workspace root, it
//! says paths are relative to it, and when the rejected path has a tail that does
//! exist inside the workspace it says which one — because the cheapest available
//! fix for *refuse, then reroute* is a refusal that carries the answer.
//!
//! # What confines a tool child
//!
//! The worktree, and nothing else. Every child reports [`Confinement::Cwd`], the
//! environment is `env_clear()` plus a named allowlist, and the posture is blast
//! radius rather than a boundary (F408–F411, ADR-0014 §3).
//!
//! # Why the file tools do not shell out
//!
//! `list_files` reads ignore rules in-process and `apply_patch` applies diffs in
//! this process, rather than either of them calling git. The exec class is
//! **derived** from [`ToolSpec::reach`], so a `Reach::Edits` tool that spawns a
//! child would be an exec-class tool admitted at the write tier — the donor
//! defect ADR-0014 §2 exists to prevent, reintroduced by an implementation
//! detail rather than by a column.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use abcc_core::outcome::{Reading, Why};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::RegexBuilder;
use serde::Deserialize;

use crate::child::{Finished, Spawn, ToolChild};
use crate::control::Watch;
use crate::patch::{self, Patch};
use crate::provider::ToolCall;
use crate::tools::{Confinement, Denied, ToolSpec, destructive_git};
use crate::turn::{ToolResult, Tools};

// ---------------------------------------------------------------------------
// What the model is allowed to be handed
// ---------------------------------------------------------------------------

/// The most of a file `read_file` hands back.
///
/// The context champion holds 64k tokens resident and the other two hold less; at
/// roughly four bytes a token that whole window is about 256 KiB, so this is a
/// quarter of everything the model has for one read. It is a cap rather than a
/// convenience, and exceeding it is **said** rather than silently trimmed.
pub const MAX_READ_BYTES: usize = 64 * 1024;

/// The most of one captured stream a tool result shows the model.
///
/// [`crate::child::MAX_CAPTURE_BYTES`] is 4 MiB and is what the *host* keeps for
/// the operator; this is what a transcript can afford to carry forward for the
/// rest of the phase.
pub const MAX_STREAM_BYTES: usize = 16 * 1024;

/// Entries one `list_files` returns before it stops walking.
pub const MAX_LISTED: usize = 400;

/// Matches one `search` returns before it stops walking.
pub const MAX_MATCHES: usize = 200;

/// The most of one matching line a search result shows. A minified bundle is one
/// line and it is not worth the window.
pub const MAX_MATCH_LINE: usize = 400;

/// Depth `list_files` walks when the model does not say.
pub const DEFAULT_DEPTH: usize = 2;

/// How long a tool child gets when nothing says otherwise.
///
/// Five minutes, which is the donor's own global task timeout of 300 s — the
/// number the 40-task battery was run under, so a task that fitted there fits
/// here.
pub const DEFAULT_EXEC_BUDGET: Duration = Duration::from_mins(5);

/// The most a model may ask for. A budget the model chooses is a budget the model
/// can choose to be infinite.
pub const MAX_EXEC_BUDGET: Duration = Duration::from_mins(15);

// ---------------------------------------------------------------------------
// The toolchain profile
// ---------------------------------------------------------------------------

/// How `run_tests` and `diagnostics` are spelled on this workspace.
///
/// ADR-0008 names the thing that is actually language-specific — **a toolchain
/// profile: extensions, build command, test command, binary path** — against a
/// per-language rung ladder, because the expensive part of the donors'
/// abstraction is output parsers keyed to a *tool* rather than to a language.
///
/// ⚠ **Nothing here parses anything**, and that is still true now that the Gate
/// reads these commands: the exit status is the verdict (ADR-0009 §5) and the
/// text is only what an operator reads. Turning output into counts is
/// `abcc_core::outcome`'s work and it lives there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toolchain {
    pub name: &'static str,
    /// The file whose presence is the evidence for this profile.
    pub witnesses: &'static [&'static str],
    pub test: &'static [&'static str],
    pub diagnostics: &'static [&'static str],
    /// How this profile reads its test runner's ending (ADR-0009 §5). It is a
    /// field rather than a match on [`Toolchain::name`] because a name match is
    /// a second table keyed to the first.
    pub reading: Reading,
    /// 🚨 **The standard the repository declares for itself, or `None`.**
    ///
    /// This is the Gate's third rung and it is the one ADR-0008 argued against:
    /// over 728 real agent attempts the linter was *strictly dominated by the
    /// test suite in both languages*, so it was rejected as a rung. **F512 is
    /// the counter-evidence, and it is from this repository.** Six working
    /// `--version` implementations by the champion were checked out and put to
    /// the compiler; `clippy -D warnings` refuses **6 of 6** on genuine
    /// violations, and on two of them (runs 10 and 23) every test passes, so the
    /// linter is the *only* rung that refuses them.
    ///
    /// The two claims are not in conflict, because they are about different
    /// questions. ADR-0008 measured a linter as a **bug catcher** and it is a
    /// poor one. This measures it as a **mergeability check**: a tree that fails
    /// the repository's own declared standard cannot land there, whether or not
    /// it is correct. See [`Standard`] for why it is declared by the repository
    /// rather than by us.
    pub standard: Option<Standard>,
}

/// A standard the repository declares, and the file that is the declaration.
///
/// 🚨 **The gate enforces what a repository asks for and nothing it does not.**
/// A `cargo clippy -- -D warnings` imposed on a project that never opted into
/// clippy is this tool having an opinion about somebody else's code, which is
/// the shape of every donor gate that scores a language it has never heard of.
/// So the rung is **declared** only when the witness is in the tree, and a
/// workspace with no witness has three rungs rather than a failed fourth —
/// an undeclared rung is not a missing measurement.
///
/// ⚠ **The witness is a file and not a manifest key.** A project that declares
/// clippy only through `[lints.clippy]` in its `Cargo.toml` is not detected
/// here, because detecting it needs a TOML parser this crate does not have, and
/// grepping a manifest for a section header is a keyword count rather than a
/// declaration. That is a stated gap, not an oversight: an operator whose
/// project is in that shape sets the profile explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standard {
    /// Any one of these in the workspace root is the repository declaring it.
    pub witnesses: &'static [&'static str],
    /// 🚨 **A conjunction of commands, in the order they run, and the first
    /// refusal ends the rung.** It is a list rather than one command because
    /// **F555** found the hole a single command leaves: the gate reported
    /// MISSION ACCOMPLISHED about a tree `cargo fmt --check` refuses — all four
    /// rungs green, the Judge with no findings, and one function whose
    /// single-line body rustfmt reformats. The standard rung was spelled
    /// `cargo clippy` and rustfmt was in **none of the four rungs**, so
    /// ADR-0017's criterion — *a correct tree is one that could land* — was
    /// being checked with part of the repository's own standard missing.
    ///
    /// ⚠ Cheapest first. Formatting is a parse and a print; clippy is a
    /// compile. An operator waiting on a red wants the fast one to say so.
    pub commands: &'static [&'static [&'static str]],
}

impl Standard {
    /// Whether this repository declares this standard.
    #[must_use]
    pub fn declared_at(&self, root: &Path) -> bool {
        self.witnesses.iter().any(|w| root.join(w).exists())
    }
}

/// The profiles, in the order they are looked for.
pub const TOOLCHAINS: &[Toolchain] = &[
    Toolchain {
        name: "cargo",
        witnesses: &["Cargo.toml"],
        test: &["cargo", "test"],
        diagnostics: &["cargo", "check", "--all-targets"],
        reading: Reading::Cargo,
        standard: Some(Standard {
            witnesses: &["clippy.toml", ".clippy.toml"],
            commands: &[
                // 🚨 F555's rung. `--check` exits 1 and prints the diff it
                // wanted, so the evidence an operator needs is the rung output
                // ADR-0019 already keeps.
                //
                // ⚠ `--color=never`, and it is **measured rather than tidy**
                // (F557): rustfmt colours its diff even when stdout is a plain
                // file rather than a terminal — 5 lines carrying ESC on the
                // one-line-body fixture — and this detail goes into a durable
                // SQLite log and is re-rendered in the TUI. Clippy is left
                // alone; changing what it prints is not what was ruled.
                &["cargo", "fmt", "--check", "--", "--color=never"],
                // ⚠ `--all-targets` matters: without it clippy does not lint
                // the test targets, and four of the six F512 implementations
                // broke a test.
                &["cargo", "clippy", "--all-targets", "--", "-D", "warnings"],
            ],
        }),
    },
    // ⚠ `python` rather than `python3`, and that is measured rather than
    // stylistic: on this platform `python3` resolves to a Microsoft Store stub
    // that prints an installation notice instead of running anything. The same
    // class as F312 — a name that resolves is not a working interpreter.
    Toolchain {
        name: "pytest",
        witnesses: &["pyproject.toml", "setup.py", "pytest.ini", "tox.ini"],
        test: &["python", "-m", "pytest", "-q"],
        diagnostics: &["python", "-m", "compileall", "-q", "."],
        reading: Reading::Python,
        // 🚨 None, deliberately. Python has no single declared standard the way
        // a cargo project has clippy: ruff, flake8, pylint and mypy are four
        // different opinions, and picking one here would be this tool choosing a
        // linter for somebody else's repository. A project that wants one names
        // it in an operator-configured profile.
        standard: None,
    },
];

impl Toolchain {
    /// The first profile whose witness is in this tree.
    ///
    /// ⚠ Detection is the witness file and nothing more. Whether the command it
    /// names is on this host is answered by executing it: a name that resolves is
    /// not a working interpreter (F312), so the probe is the run.
    #[must_use]
    pub fn detect(root: &Path) -> Option<Toolchain> {
        TOOLCHAINS
            .iter()
            .find(|t| t.witnesses.iter().any(|w| root.join(w).exists()))
            .copied()
    }
}

// ---------------------------------------------------------------------------
// The workspace
// ---------------------------------------------------------------------------

/// One attempt's working directory, and the nine tools over it.
pub struct Workspace {
    root: PathBuf,
    toolchain: Option<Toolchain>,
    watch: Watch,
    budget: Duration,
}

impl Workspace {
    /// Open a workspace at `root`.
    ///
    /// 🚨 The root is canonical from here on. Both sides of every path comparison
    /// are canonical, which is what keeps `D:\dev\x` and `\\?\D:\dev\x` from
    /// being two different directories to an argument check.
    ///
    /// # Errors
    ///
    /// Whatever the OS says about a directory that is not there.
    pub fn open(root: impl AsRef<Path>) -> std::io::Result<Workspace> {
        let root = root.as_ref().canonicalize()?;
        let toolchain = Toolchain::detect(&root);
        Ok(Workspace {
            root,
            toolchain,
            watch: Watch::detached(),
            budget: DEFAULT_EXEC_BUDGET,
        })
    }

    /// Watch this control point, so an operator's urgent verb ends a running tool
    /// child instead of waiting for it.
    #[must_use]
    pub fn watching(mut self, watch: Watch) -> Workspace {
        self.watch = watch;
        self
    }

    /// Override the detected profile — a workspace whose witness file is missing,
    /// or one whose commands the operator has chosen.
    #[must_use]
    pub fn with_toolchain(mut self, toolchain: Toolchain) -> Workspace {
        self.toolchain = Some(toolchain);
        self
    }

    /// The default tool-child budget.
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Workspace {
        self.budget = budget.min(MAX_EXEC_BUDGET);
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn toolchain(&self) -> Option<Toolchain> {
        self.toolchain
    }

    /// What this workspace's tool children actually get, which the console shows
    /// rather than a footnote (ADR-0014 §3).
    #[must_use]
    pub fn confinement(&self) -> Confinement {
        crate::child::confinement()
    }
}

impl Tools for Workspace {
    fn run(&self, spec: &'static ToolSpec, call: &ToolCall) -> ToolResult {
        let started = Instant::now();
        let raw = call.arguments.as_str();
        let mut result = match spec.name {
            "read_file" => told(self.read_file(raw)),
            "list_files" => told(self.list_files(raw)),
            "search" => told(self.search(raw)),
            "write_file" => told(self.write_file(raw)),
            "apply_patch" => told(self.apply_patch(raw)),
            "bash" => self.bash(raw).unwrap_or_else(refused),
            "run_tests" => self.runner(raw, Runner::Test).unwrap_or_else(refused),
            "diagnostics" => self
                .runner(raw, Runner::Diagnostics)
                .unwrap_or_else(refused),
            "git" => self.git(raw).unwrap_or_else(refused),
            other => unimplemented(other),
        };
        // One clock for the record, around everything the operator waited for:
        // the child's own elapsed time is in the text, where its scope is clear.
        result.elapsed_ms = elapsed_ms(started);
        result
    }
}

// ---------------------------------------------------------------------------
// The five that touch files
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    from_line: Option<usize>,
    to_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default = "here")]
    path: String,
    depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    pattern: String,
    #[serde(default = "here")]
    path: String,
    glob: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct PatchArgs {
    diff: String,
}

fn here() -> String {
    ".".to_owned()
}

impl Workspace {
    fn read_file(&self, raw: &str) -> Result<String, String> {
        let args: ReadArgs = parse(raw, "read_file")?;
        let path = self.resolve("read_file", &args.path)?;
        let bytes = fs::read(&path)
            .map_err(|e| format!("read_file: {} could not be read: {e}", args.path))?;
        if let Some(at) = zero_byte(&bytes) {
            return Err(format!(
                "read_file: {} is binary — a zero byte at offset {at}, {} bytes in total",
                args.path,
                bytes.len()
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let from = args.from_line.unwrap_or(1).max(1);
        if from > total {
            return Err(format!(
                "read_file: {} has {total} lines and from_line is {from}",
                args.path
            ));
        }
        let to = args.to_line.unwrap_or(total).clamp(from, total);
        let (body, elided) = clamp(&lines[from - 1..to].join("\n"), MAX_READ_BYTES);
        let note = elided.map_or_else(String::new, |n| format!(", {n} bytes elided"));
        Ok(format!(
            "{} lines {from}-{to} of {total}{note}\n{body}",
            self.relative(&path)
        ))
    }

    fn list_files(&self, raw: &str) -> Result<String, String> {
        let args: ListArgs = parse(raw, "list_files")?;
        let dir = self.resolve("list_files", &args.path)?;
        let depth = args.depth.unwrap_or(DEFAULT_DEPTH).clamp(1, 64);

        let mut entries = Vec::new();
        let mut stopped = false;
        for found in walker(&dir, None)?.max_depth(Some(depth)).build() {
            let found = found.map_err(|e| format!("list_files: {e}"))?;
            if found.path() == dir {
                continue;
            }
            if entries.len() >= MAX_LISTED {
                stopped = true;
                break;
            }
            let slash = if found.file_type().is_some_and(|t| t.is_dir()) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{}{slash}", self.relative(found.path())));
        }

        let note = if stopped {
            format!(" (stopped at {MAX_LISTED}; ask for a subdirectory)")
        } else {
            String::new()
        };
        Ok(format!(
            "{} entries under {} to depth {depth}{note}\n{}",
            entries.len(),
            self.relative(&dir),
            entries.join("\n")
        ))
    }

    fn search(&self, raw: &str) -> Result<String, String> {
        let args: SearchArgs = parse(raw, "search")?;
        let scope = self.resolve("search", &args.path)?;
        let re = RegexBuilder::new(&args.pattern).build().map_err(|e| {
            format!(
                "search: {} is not a pattern this engine accepts: {e}",
                args.pattern
            )
        })?;

        let mut hits: Vec<String> = Vec::new();
        let mut searched = 0usize;
        let mut in_files = 0usize;
        for found in walker(&scope, args.glob.as_deref())?.build() {
            if hits.len() >= MAX_MATCHES {
                break;
            }
            let found = found.map_err(|e| format!("search: {e}"))?;
            if !found.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(bytes) = fs::read(found.path()) else {
                continue;
            };
            if zero_byte(&bytes).is_some() {
                continue;
            }
            searched += 1;
            let text = String::from_utf8_lossy(&bytes);
            let before = hits.len();
            for (n, line) in text.lines().enumerate() {
                if hits.len() >= MAX_MATCHES {
                    break;
                }
                if re.is_match(line) {
                    let (shown, _) = clamp(line.trim_end(), MAX_MATCH_LINE);
                    hits.push(format!(
                        "{}:{}: {shown}",
                        self.relative(found.path()),
                        n + 1
                    ));
                }
            }
            if hits.len() > before {
                in_files += 1;
            }
        }

        // 🚨 The file count is part of the answer. A bare "no matches" from an
        // instrument nobody validated is not a measurement — it reads the same
        // whether the pattern is absent or the walk visited nothing.
        Ok(format!(
            "{} match{} in {in_files} of {searched} files for /{}/ under {}\n{}",
            hits.len(),
            if hits.len() == 1 { "" } else { "es" },
            args.pattern,
            self.relative(&scope),
            hits.join("\n")
        ))
    }

    fn write_file(&self, raw: &str) -> Result<String, String> {
        let args: WriteArgs = parse(raw, "write_file")?;
        let path = self.resolve("write_file", &args.path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("write_file: {} has no parent directory: {e}", args.path))?;
        }
        let existed = path.exists();
        fs::write(&path, args.content.as_bytes())
            .map_err(|e| format!("write_file: {} could not be written: {e}", args.path))?;
        Ok(format!(
            "{} {} ({} bytes, {} lines)",
            if existed { "replaced" } else { "created" },
            self.relative(&path),
            args.content.len(),
            args.content.lines().count()
        ))
    }

    /// ⚠ **All or nothing.** Every file is applied in memory first and nothing is
    /// written until all of them succeed, because a half-applied diff leaves a
    /// tree nobody chose and the model cannot see that it happened.
    fn apply_patch(&self, raw: &str) -> Result<String, String> {
        let args: PatchArgs = parse(raw, "apply_patch")?;
        let patch = Patch::parse(&args.diff).map_err(|e| format!("apply_patch: {e}"))?;

        let mut staged: Vec<(PathBuf, Option<String>)> = Vec::new();
        let mut applied: Vec<(String, usize)> = Vec::new();
        for file in &patch.files {
            let target = self.resolve("apply_patch", &file.path)?;
            let original = match fs::read(&target) {
                Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| {
                    format!(
                        "apply_patch: {} is not UTF-8 and cannot be patched",
                        file.path
                    )
                })?),
                Err(_) => None,
            };
            let new = file
                .apply(original.as_deref())
                .map_err(|e| format!("apply_patch: {e}"))?;
            staged.push((target, new));
            applied.push((file.path.clone(), file.hunk_count()));
        }

        for (path, new) in &staged {
            match new {
                Some(content) => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("apply_patch: {}: {e}", path.display()))?;
                    }
                    fs::write(path, content)
                        .map_err(|e| format!("apply_patch: {}: {e}", path.display()))?;
                }
                None => fs::remove_file(path)
                    .map_err(|e| format!("apply_patch: {}: {e}", path.display()))?,
            }
        }
        Ok(patch::summarise(&applied))
    }
}

/// One walker, configured the same way for both tools that use it.
///
/// `.git` is skipped by name: it is never what a model is looking for and it is
/// most of the entries in a repository. Ignore rules are read from the tree
/// itself with `require_git(false)`, so a worktree and a bare directory behave
/// the same.
fn walker(at: &Path, glob: Option<&str>) -> Result<WalkBuilder, String> {
    let mut builder = WalkBuilder::new(at);
    builder
        .hidden(false)
        .parents(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_path(Path::cmp);
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(at);
        overrides
            .add(glob)
            .map_err(|e| format!("search: {glob} is not a usable glob: {e}"))?;
        builder.overrides(
            overrides
                .build()
                .map_err(|e| format!("search: {glob} is not a usable glob: {e}"))?,
        );
    }
    Ok(builder)
}

// ---------------------------------------------------------------------------
// The four that start a child
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct SelectorArgs {
    selector: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitArgs {
    args: Vec<String>,
}

/// Which half of the toolchain profile a call wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    Test,
    Diagnostics,
}

impl Runner {
    const fn tool(self) -> &'static str {
        match self {
            Runner::Test => "run_tests",
            Runner::Diagnostics => "diagnostics",
        }
    }

    const fn command(self, toolchain: Toolchain) -> &'static [&'static str] {
        match self {
            Runner::Test => toolchain.test,
            Runner::Diagnostics => toolchain.diagnostics,
        }
    }
}

impl Workspace {
    fn bash(&self, raw: &str) -> Result<ToolResult, String> {
        let args: BashArgs = parse(raw, "bash")?;
        not_destructive(&args.command)?;
        let budget = args.timeout_ms.map_or(self.budget, |ms| {
            Duration::from_millis(ms).min(MAX_EXEC_BUDGET)
        });
        let Some(shell) = shell() else {
            return Ok(ToolResult {
                text: "bash: there is no bash on this host".to_owned(),
                exit: None,
                elapsed_ms: 0,
                unmeasured: Some(Why::CheckerNotOnHost {
                    binary: "bash".to_owned(),
                }),
            });
        };
        Ok(self.exec(
            &shell,
            &["-c".to_owned(), args.command.clone()],
            &args.command,
            budget,
        ))
    }

    fn git(&self, raw: &str) -> Result<ToolResult, String> {
        let args: GitArgs = parse(raw, "git")?;
        // 🚨 The backstop scans for a token that *is* git, and an argv does not
        // carry one. Handing it the bare arguments would be a guard that matches
        // nothing — which is how the donor's ends up covering `git reset --hard`
        // through one tool and not through the other (F422).
        let line = format!("git {}", args.args.join(" "));
        not_destructive(&line)?;
        Ok(self.exec("git", &args.args, &line, self.budget))
    }

    fn runner(&self, raw: &str, which: Runner) -> Result<ToolResult, String> {
        let asked: SelectorArgs = parse(raw, which.tool())?;
        let Some(toolchain) = self.toolchain else {
            let sentence = format!(
                "{}: there is no toolchain profile for this workspace — none of {} is here",
                which.tool(),
                TOOLCHAINS
                    .iter()
                    .flat_map(|t| t.witnesses)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(ToolResult {
                text: sentence,
                exit: None,
                elapsed_ms: 0,
                unmeasured: Some(Why::NoCheckerForArtifact {
                    artifact: self.relative(&self.root),
                }),
            });
        };
        let command = which.command(toolchain);
        let mut argv: Vec<String> = command[1..].iter().map(|a| (*a).to_owned()).collect();
        argv.extend(
            asked
                .selector
                .iter()
                .flat_map(|s| s.split_whitespace())
                .map(ToOwned::to_owned),
        );
        let display = format!("{} {}", command[0], argv.join(" "));
        Ok(self.exec(command[0], &argv, &display, self.budget))
    }

    fn exec(
        &self,
        program: impl AsRef<std::ffi::OsStr>,
        argv: &[String],
        display: &str,
        budget: Duration,
    ) -> ToolResult {
        let spawn = Spawn::new(program.as_ref(), &self.root)
            .args(argv.iter().map(OsString::from))
            .budget(budget);
        match ToolChild::spawn(&spawn) {
            Err(why) => ToolResult {
                text: format!("$ {display}\n{why}"),
                exit: None,
                elapsed_ms: 0,
                unmeasured: Some(why),
            },
            Ok(child) => {
                let finished = child.finish_watching(&self.watch);
                ToolResult {
                    text: render(display, &finished),
                    exit: finished.exit,
                    elapsed_ms: finished.elapsed_ms,
                    unmeasured: finished.unmeasured,
                }
            }
        }
    }
}

/// The interpreter the `bash` tool runs, looked for rather than spawned by name.
///
/// 🚨 **F492: on this box the first `bash` on PATH is the WSL relay in the system
/// directory, and it does not work.** With no distribution installed it answers
/// `execvpe(/bin/bash) failed: No such file or directory` **at exit 1** — so a
/// tool layer that spawns `bash` by name records *the command failed* about a
/// shell that never ran, which is the same class of lie as a timeout folded into
/// "clean" (F220). It is not even a wrong host: WSL2 is installed here, it
/// answers, and it has nothing to run.
///
/// So the search is explicit, and every step of it is a path that exists rather
/// than a name that resolves (F312):
///
/// 1. `ABCC_SHELL`, because an operator with a different shell is configuration
///    and not a special case.
/// 2. A `bash` on PATH that is **not** inside the system directory — which is the
///    WSL relay's only home.
/// 3. Git for Windows, where this platform's working bash actually lives.
///
/// On every other platform the name is the answer and the OS resolves it.
fn shell() -> Option<OsString> {
    if let Some(chosen) = std::env::var_os("ABCC_SHELL") {
        return Some(chosen);
    }
    if !cfg!(windows) {
        return Some(OsString::from("bash"));
    }
    let system = std::env::var_os("SystemRoot").map(PathBuf::from);
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("bash.exe"))
                .collect::<Vec<_>>()
        })
        .find(|candidate| {
            candidate.is_file()
                && system
                    .as_ref()
                    .is_none_or(|root| !candidate.starts_with(root))
        });
    if let Some(found) = on_path {
        return Some(found.into_os_string());
    }
    ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|dir| {
            let mut path = PathBuf::from(dir);
            path.push("Git");
            path.push("bin");
            path.push("bash.exe");
            path
        })
        .find(|path| path.is_file())
        .map(PathBuf::into_os_string)
}

/// The backstop, on the whole command line. It is never the control — the
/// boundary is the worktree — and it exists because the donor's guard exists
/// twice and neither copy covers the other (F422).
fn not_destructive(line: &str) -> Result<(), String> {
    match destructive_git(line) {
        None => Ok(()),
        Some(op) => {
            let (shown, _) = clamp(line, MAX_MATCH_LINE);
            Err(Denied::DestructiveGit { op, line: shown }.to_string())
        }
    }
}

/// What the host watched, in the order an operator reads it.
fn render(display: &str, finished: &Finished) -> String {
    let mut out = format!("$ {display}\n");
    match (&finished.unmeasured, finished.exit) {
        (Some(why), _) => {
            let _ = writeln!(out, "no exit status: {why}");
        }
        (None, Some(code)) => {
            let _ = writeln!(out, "exit {code} in {} ms", finished.elapsed_ms);
        }
        (None, None) => {
            let _ = writeln!(out, "no exit status and no reason: a bug in the tool layer");
        }
    }
    for (name, stream) in [("stdout", &finished.stdout), ("stderr", &finished.stderr)] {
        if stream.trim().is_empty() {
            continue;
        }
        let (body, elided) = clamp(stream, MAX_STREAM_BYTES);
        let note = elided.map_or_else(String::new, |n| format!(", {n} bytes elided"));
        let _ = writeln!(out, "--- {name} ({} bytes{note}) ---", stream.len());
        out.push_str(&body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 🚨 The argument check
// ---------------------------------------------------------------------------

impl Workspace {
    /// Resolve a model-supplied path against the workspace root.
    ///
    /// 🚨 **An argument check, and never the sandbox.** It binds the tool that has
    /// an argument; `bash` has none of it. See the module note for what that is
    /// measured to be worth and for why the refusal reads the way it does.
    fn resolve(&self, tool: &str, raw: &str) -> Result<PathBuf, String> {
        if raw.trim().is_empty() {
            return Err(format!("{tool}: the path is empty"));
        }
        let asked = Path::new(raw);
        let joined = if asked.is_absolute() {
            asked.to_path_buf()
        } else {
            self.root.join(asked)
        };
        // Lexically first, so `..` is gone before anything touches the disk, then
        // canonically, so a symlink out of the tree is caught by the same check.
        let real = canonical(&lexical(&joined));
        if real.starts_with(&self.root) {
            return Ok(real);
        }
        Err(self.outside(tool, raw))
    }

    /// The refusal, written to be acted on rather than merely to be correct.
    fn outside(&self, tool: &str, raw: &str) -> String {
        let mut sentence = format!(
            "{tool}: the argument check refused {raw} — it is outside the workspace. \
             The workspace is {} and every path is relative to it.",
            self.root.display()
        );
        if let Some(meant) = self.nearest_inside(raw) {
            let _ = write!(sentence, " Did you mean {meant}?");
        }
        sentence
    }

    /// The longest tail of a rejected path whose parent directory does exist
    /// inside the workspace.
    ///
    /// 🚨 This is the whole of F400–F403's lesson in one function: the measured
    /// failure is *the work-dir component is missing, the check refuses, and the
    /// model reroutes through `bash`* — so the refusal carries the answer.
    fn nearest_inside(&self, raw: &str) -> Option<String> {
        let parts: Vec<&std::ffi::OsStr> = Path::new(raw)
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect();
        for take in (1..=parts.len()).rev() {
            let tail: PathBuf = parts[parts.len() - take..].iter().collect();
            let candidate = self.root.join(&tail);
            let usable = candidate.exists()
                || candidate
                    .parent()
                    .is_some_and(|p| p.is_dir() && p != self.root);
            if usable {
                return Some(tail.display().to_string().replace('\\', "/"));
            }
        }
        None
    }

    /// A path as the model should write it back: relative to the root, with
    /// forward slashes, because that is what it wrote in the first place.
    fn relative(&self, path: &Path) -> String {
        let shown = path.strip_prefix(&self.root).unwrap_or(path);
        let shown = shown.display().to_string().replace('\\', "/");
        if shown.is_empty() {
            ".".to_owned()
        } else {
            shown
        }
    }
}

/// Remove `.` and `..` textually. `..` above the top is left in place, so the
/// caller's `starts_with` refuses it rather than this quietly clamping.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Canonicalise as much of a path as exists and re-attach the rest.
///
/// A file being written for the first time does not exist yet, so canonicalising
/// the whole path is not available; canonicalising its deepest existing ancestor
/// is, and it is what makes `\\?\D:\x` and `D:\x` the same directory to the
/// check.
fn canonical(path: &Path) -> PathBuf {
    let mut tail: Vec<OsString> = Vec::new();
    let mut here = path.to_path_buf();
    loop {
        if let Ok(real) = here.canonicalize() {
            let mut full = real;
            for part in tail.iter().rev() {
                full.push(part);
            }
            return full;
        }
        let Some(name) = here.file_name().map(ToOwned::to_owned) else {
            return path.to_path_buf();
        };
        tail.push(name);
        if !here.pop() {
            return path.to_path_buf();
        }
    }
}

// ---------------------------------------------------------------------------
// The small shared shapes
// ---------------------------------------------------------------------------

/// A file tool's answer. There is no exit status because there was no process:
/// inventing `Some(0)` would be the record claiming a measurement nothing took.
fn told(answer: Result<String, String>) -> ToolResult {
    match answer {
        Ok(text) => ToolResult {
            text,
            exit: None,
            elapsed_ms: 0,
            unmeasured: None,
        },
        Err(sentence) => refused(sentence),
    }
}

/// 🚨 The argument check refusing, an argument that will not parse, or a backstop
/// firing. Nothing ran, so the class is [`Why::FailedBeforeRunning`] — and **the
/// sentence the model reads is the same string the log records**, so a transcript
/// and a record cannot disagree about why a tool did nothing.
fn refused(sentence: String) -> ToolResult {
    ToolResult {
        text: sentence.clone(),
        exit: None,
        elapsed_ms: 0,
        unmeasured: Some(Why::FailedBeforeRunning { detail: sentence }),
    }
}

/// The registry advertises a tool this layer cannot run. That is not the model's
/// mistake and not a denial: it is the engine, and it says so.
fn unimplemented(tool: &str) -> ToolResult {
    let sentence = format!("{tool} is in the registry and this workspace cannot run it");
    ToolResult {
        text: sentence.clone(),
        exit: None,
        elapsed_ms: 0,
        unmeasured: Some(Why::EngineError { detail: sentence }),
    }
}

/// Arguments, leniently.
///
/// ⚠ The schema the head advertises says `additionalProperties: false` and this
/// parser ignores unknown fields anyway. A refusal for a field nobody read costs
/// a round of the budget and buys nothing; a missing *required* field is a
/// different thing and is refused, because there is no work to do without it.
fn parse<T: for<'de> Deserialize<'de>>(raw: &str, tool: &str) -> Result<T, String> {
    serde_json::from_str(raw).map_err(|e| {
        let (shown, _) = clamp(raw, MAX_MATCH_LINE);
        format!("{tool}: the arguments are not what its schema describes: {e}. Got {shown}")
    })
}

/// Keep the head and the tail, and say how much went.
///
/// 🚨 Both ends. A compiler puts the error first and a test runner puts the
/// summary last, so a tool layer that keeps one end is blind to whichever tool it
/// did not have in mind.
fn clamp(text: &str, budget: usize) -> (String, Option<usize>) {
    if text.len() <= budget {
        return (text.to_owned(), None);
    }
    let head = floor_boundary(text, budget * 2 / 3);
    let tail = ceil_boundary(text, text.len().saturating_sub(budget / 3));
    let elided = tail.saturating_sub(head);
    (
        format!(
            "{}\n… {elided} bytes elided …\n{}",
            &text[..head],
            &text[tail..]
        ),
        Some(elided),
    )
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// Where the first zero byte is, if there is one in the first block.
///
/// The same heuristic every diff tool uses, and it is a heuristic: a file with no
/// zero byte in its first 8 KiB is treated as text, because the alternative is
/// spending the model's window on a decoded binary.
fn zero_byte(bytes: &[u8]) -> Option<usize> {
    bytes.iter().take(8 * 1024).position(|b| *b == 0)
}

fn elapsed_ms(from: Instant) -> u64 {
    u64::try_from(from.elapsed().as_millis()).unwrap_or(u64::MAX)
}
