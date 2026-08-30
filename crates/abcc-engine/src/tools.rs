//! The tool layer's policy half: what a role may reach for, and the class that
//! is denied rather than argued with.
//!
//! ADR-0014. The brief asked *a path check per tool, or a process the tool child
//! runs inside*, and both halves moved.
//!
//! 🚨 **The path check is not the weak part of a good design — it is a well-built
//! control that is simply not in the path of the tools that execute code**, and
//! all three donors have that property by three different mechanisms.
//! `write_file` is path-sandboxed and `bash` is not, so *`write_file` on a new
//! path plus `run_tests`* is arbitrary code execution at the tier that never
//! prompts (F404–F406).
//!
//! And there is nothing underneath to reach for. A `runas /trustlevel:0x20000`
//! child stays at Medium integrity, writes the user's home directory and opens
//! TCP; WSL2's `binfmt_misc` hands any PE file back to the Windows host to
//! execute (F408–F411). **There is no containment on this platform without Win32
//! token code**, the state of the art does not attempt it, and this workspace
//! forbids `unsafe`.
//!
//! So the only enforcement that is real here is policy: **a role that does not
//! need a capability does not get it.** Ten of the threat model's eleven rows
//! close on that one control, and it is not a check —
//!
//! * an argument check binds one tool and a shell walks past it, demonstrated
//!   four times by four mechanisms across three authors;
//! * a prompt binds only as far as the model complies — **39 of 50** for the
//!   shipped wrapper, and **0 of 50** for the best re-aimed one, which is a large
//!   effect in the right direction and still a prompt;
//! * underneath either of them there is no OS boundary.
//!
//! ⚠ Argument checks are kept and **demoted**. They stop honest mistakes, and
//! everywhere they appear — in this code and in the text the model is shown —
//! they are called *an argument check*, never *the sandbox*.

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// The ceiling
// ---------------------------------------------------------------------------

/// What a role may reach for.
///
/// Ordered from the bottom: a policy admits a tool whose required tier is at or
/// below its ceiling. 🚨 The ceiling belongs to the **role**, never to the tool
/// name — rewriting the README sentence is not the fix, `max_tier` per role is
/// (ADR-0014 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// No tools at all. The Judge's ceiling: A4 is one model call with no tools,
    /// and a phase that cannot call a tool cannot be talked into calling one.
    NoTools,
    /// Reads the workspace and nothing else.
    Read,
    /// Writes files inside the workspace.
    Write,
    /// 🚨 Starts child processes. This is the class, and it is the whole class.
    Exec,
}

impl Tier {
    /// Every ceiling, lowest first. The order is the enum's own, which is also
    /// the `Ord` the comparisons use.
    pub const ALL: [Tier; 4] = [Tier::NoTools, Tier::Read, Tier::Write, Tier::Exec];

    /// The lower of two ceilings — **the whole of what a slot cap does.**
    ///
    /// A role declares what it needs and a slot declares what it will allow, and
    /// the effective ceiling is the narrower of the two. It is spelled `narrower`
    /// rather than `min` because a slot cap reads as *narrowing* a role, and
    /// because shadowing [`Ord::min`] with something that had to agree with it
    /// would be a second definition of the same order.
    ///
    /// ⚠ It is `const` — which is why it is a discriminant comparison rather
    /// than `Ord::min`, and why `tests/policy.rs` checks the two against each
    /// other over all sixteen pairs rather than trusting the cast.
    #[must_use]
    pub const fn narrower(self, other: Tier) -> Tier {
        if (self as u8) <= (other as u8) {
            self
        } else {
            other
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Tier::NoTools => "no-tools",
            Tier::Read => "read",
            Tier::Write => "write",
            Tier::Exec => "exec",
        })
    }
}

impl std::str::FromStr for Tier {
    type Err = UnknownTier;

    /// Parses the [`fmt::Display`] spelling, so the word an operator types is the
    /// word the log and the refusal print back.
    fn from_str(s: &str) -> std::result::Result<Tier, UnknownTier> {
        match s {
            "no-tools" => Ok(Tier::NoTools),
            "read" => Ok(Tier::Read),
            "write" => Ok(Tier::Write),
            "exec" => Ok(Tier::Exec),
            other => Err(UnknownTier {
                given: other.to_owned(),
            }),
        }
    }
}

/// A word that is not one of the four ceilings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{given} is not a ceiling; the four are no-tools, read, write and exec")]
pub struct UnknownTier {
    pub given: String,
}

/// What a tool does to the machine — the fact the exec class is *defined* by.
///
/// 🚨 Deliberately not a name list. The donor put four tools in one class for the
/// egress control and then gated them separately for this one; here the class is
/// a property every entry declares, so a tool cannot join the registry without
/// answering the question, and [`ToolSpec::required_tier`] reads the answer
/// rather than trusting a second column to agree with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// Reads the workspace.
    Inspects,
    /// Writes files inside the workspace.
    Edits,
    /// Starts a child process — any shell, any toolchain runner, any package
    /// manager. **The isolation boundary is the tool child, not the agent**, so
    /// everything in this arm is one class regardless of how narrow its argument
    /// surface looks.
    SpawnsChild,
}

// ---------------------------------------------------------------------------
// The registry — the one const
// ---------------------------------------------------------------------------

/// One tool, as the registry holds it and as the head advertises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub reach: Reach,
    /// One line, shown to the model in the head and to the operator in the
    /// console.
    pub summary: &'static str,
    /// The JSON Schema for this tool's arguments, exactly as it goes into the
    /// head.
    ///
    /// `&'static str` rather than something assembled at call time, because
    /// ADR-0011 §2 makes the system prefix immutable within an attempt: a schema
    /// built per call is a prompt head that varies, and one token changed at the
    /// *front* annihilates the 79.7% TTFT saving the prefix cache is worth (F81).
    pub schema: &'static str,
}

impl ToolSpec {
    /// The tier this tool's reach forces.
    ///
    /// 🚨 Derived, never declared. A registry with both a `reach` and a `tier`
    /// column is a registry where the two can disagree, and that disagreement is
    /// exactly the donor defect: four tools identified as one class for one
    /// control and gated separately for another.
    #[must_use]
    pub const fn required_tier(&self) -> Tier {
        match self.reach {
            Reach::Inspects => Tier::Read,
            Reach::Edits => Tier::Write,
            Reach::SpawnsChild => Tier::Exec,
        }
    }

    /// Whether this tool starts a child process — the predicate the exec class
    /// *is*, spelled out for the console and for the maintenance test.
    #[must_use]
    pub const fn in_exec_class(&self) -> bool {
        matches!(self.reach, Reach::SpawnsChild)
    }
}

/// 🚨 **Every tool, in one const.** ADR-0014 §2 copies the donor's egress
/// registry shape exactly — one const, a maintenance contract, and an
/// integration test that drives every entry through the real policy and asserts
/// the refusal — because the donor already proved the pattern for the *other*
/// control and already put these four tools in one class for it.
///
/// The maintenance contract in one sentence: **an entry whose `reach` is
/// [`Reach::SpawnsChild`] is in the exec class**, and `tests/policy.rs` fails the
/// moment such an entry is admitted below [`Tier::Exec`] by any role's ceiling.
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "read_file",
        reach: Reach::Inspects,
        summary: "Read a file from the workspace, optionally a line range.",
        schema: concat!(
            r#"{"type":"object","properties":{"path":{"type":"string"},"#,
            r#""from_line":{"type":"integer","minimum":1},"#,
            r#""to_line":{"type":"integer","minimum":1}},"#,
            r#""required":["path"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "list_files",
        reach: Reach::Inspects,
        summary: "List paths under a directory, honouring the repository's ignore rules.",
        schema: concat!(
            r#"{"type":"object","properties":{"path":{"type":"string"},"#,
            r#""depth":{"type":"integer","minimum":1}},"#,
            r#""required":["path"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "search",
        reach: Reach::Inspects,
        summary: "Search the workspace for a regular expression and return matching lines.",
        schema: concat!(
            r#"{"type":"object","properties":{"pattern":{"type":"string"},"#,
            r#""path":{"type":"string"},"glob":{"type":"string"}},"#,
            r#""required":["pattern"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "write_file",
        reach: Reach::Edits,
        summary: "Write a file in the workspace, creating it if it does not exist.",
        schema: concat!(
            r#"{"type":"object","properties":{"path":{"type":"string"},"#,
            r#""content":{"type":"string"}},"#,
            r#""required":["path","content"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "apply_patch",
        reach: Reach::Edits,
        summary: "Apply a unified diff to the workspace.",
        schema: concat!(
            r#"{"type":"object","properties":{"diff":{"type":"string"}},"#,
            r#""required":["diff"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "bash",
        reach: Reach::SpawnsChild,
        summary: "Run a shell command in the workspace.",
        schema: concat!(
            r#"{"type":"object","properties":{"command":{"type":"string"},"#,
            r#""timeout_ms":{"type":"integer","minimum":1}},"#,
            r#""required":["command"],"additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "run_tests",
        reach: Reach::SpawnsChild,
        summary: "Run the workspace's test suite and return what the host watched it do.",
        schema: concat!(
            r#"{"type":"object","properties":{"selector":{"type":"string"}},"#,
            r#""additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "diagnostics",
        reach: Reach::SpawnsChild,
        summary: "Build or typecheck the workspace and return the compiler's own output.",
        schema: concat!(
            r#"{"type":"object","properties":{"selector":{"type":"string"}},"#,
            r#""additionalProperties":false}"#
        ),
    },
    ToolSpec {
        name: "git",
        reach: Reach::SpawnsChild,
        summary: "Run a git command in the workspace.",
        schema: concat!(
            r#"{"type":"object","properties":{"args":{"type":"array","#,
            r#""items":{"type":"string"}}},"#,
            r#""required":["args"],"additionalProperties":false}"#
        ),
    },
];

/// The registry entry for `name`, if there is one.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

// ---------------------------------------------------------------------------
// The control: deny the class
// ---------------------------------------------------------------------------

/// A role's ceiling. This is the enforcement, and there is no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The call-sign the ceiling belongs to, so a refusal names a role rather
    /// than a rule number.
    pub role: &'static str,
    pub max_tier: Tier,
}

impl Policy {
    #[must_use]
    pub const fn new(role: &'static str, max_tier: Tier) -> Policy {
        Policy { role, max_tier }
    }

    /// The tools this policy admits, in registry order.
    ///
    /// **This is the set that goes into the head**, which is what makes the
    /// advertised surface and the enforced surface one list rather than two that
    /// happen to agree today.
    #[must_use]
    pub fn admitted(&self) -> Vec<&'static ToolSpec> {
        TOOLS
            .iter()
            .filter(|t| t.required_tier() <= self.max_tier)
            .collect()
    }

    /// Resolve a tool name the model asked for.
    ///
    /// # Errors
    ///
    /// [`Denied::NoSuchTool`] when the name is not in the registry — a model that
    /// invents a tool is refused down the same path as one that reaches above its
    /// ceiling — and [`Denied::AboveCeiling`] when the tool exists and this role
    /// may not have it.
    pub fn admits(&self, tool: &str) -> Result<&'static ToolSpec, Denied> {
        let Some(spec) = lookup(tool) else {
            return Err(Denied::NoSuchTool {
                role: self.role,
                tool: tool.to_owned(),
            });
        };
        let needs = spec.required_tier();
        if needs <= self.max_tier {
            Ok(spec)
        } else {
            Err(Denied::AboveCeiling {
                role: self.role,
                tool: spec.name,
                needs,
                ceiling: self.max_tier,
            })
        }
    }
}

/// Why a tool call did not happen.
///
/// A denial is a normal outcome the console shows, not an error path — the same
/// shape as [`abcc_core::task::Refused`], for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("{role} has no tool named {tool}")]
    NoSuchTool { role: &'static str, tool: String },
    #[error("{tool} is {needs}; {role} is capped at {ceiling}")]
    AboveCeiling {
        role: &'static str,
        tool: &'static str,
        needs: Tier,
        ceiling: Tier,
    },
    #[error("the argument check refused {op}: {line}")]
    DestructiveGit { op: Destructive, line: String },
}

// ---------------------------------------------------------------------------
// The posture, as a value the operator can see
// ---------------------------------------------------------------------------

/// What actually confined a tool child.
///
/// 🚨 Not a Boolean, and not an argument silently ignored on the wrong platform
/// (ADR-0014 §3). **A run whose every tool child reports [`Confinement::Cwd`] is
/// a fact the console must show**, because that is the true state of 2.0 on
/// Windows today: the posture is *blast radius*, not a boundary, and the
/// disposable worktree is the whole of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "confinement", rename_all = "snake_case")]
pub enum Confinement {
    /// Nothing at all — the child inherited the operator's working directory and
    /// environment. Reaching this arm is a bug in the caller, not a setting.
    Unconfined,
    /// The child ran in the attempt's worktree with a scrubbed environment, and
    /// nothing stronger. **This is what every tool child reports on Windows.**
    Cwd,
    /// A real OS-level confinement primitive. ⚠ This arm does not occur on this
    /// platform today; it exists because ADR-0014's falsifier is *a supported
    /// confinement primitive appears*, and an enum that cannot express the answer
    /// hides the question.
    OsSandbox { kind: String },
}

// ---------------------------------------------------------------------------
// The backstop — never the control
// ---------------------------------------------------------------------------

/// A git operation that destroys work without asking.
///
/// ⚠ **This is a backstop and never the control.** The boundary is the worktree
/// (ADR-0007). The table exists because the donor's guard exists *twice* and
/// neither copy covers the other (F422).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destructive {
    ResetHard,
    CheckoutForce,
    SwitchForce,
    PushForce,
    BranchForceDelete,
    CleanForce,
    StashDrop,
}

impl fmt::Display for Destructive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Destructive::ResetHard => "git reset --hard",
            Destructive::CheckoutForce => "git checkout --force",
            Destructive::SwitchForce => "git switch --force",
            Destructive::PushForce => "git push --force",
            Destructive::BranchForceDelete => "git branch -D",
            Destructive::CleanForce => "git clean -f",
            Destructive::StashDrop => "git stash drop",
        })
    }
}

/// One row of the table both paths read.
struct Rule {
    op: Destructive,
    sub: &'static str,
    /// Long forms, matched whole.
    long: &'static [&'static str],
    /// A short letter, matched *inside a cluster*, so `-fd` triggers on `f`.
    short: Option<char>,
    /// A bare word rather than a flag — `git stash drop` has no flag at all.
    word: Option<&'static str>,
}

/// 🚨 **One table, read by both paths, with both holes closed.**
///
/// The donor's careful copy recognises three operations — `reset --hard`,
/// `checkout -f`, `switch -f` — so **`git push --force` and `git branch -D` are
/// blocked through the git tool and unrecognised through `bash`**, while
/// `git clean -fd` and `git stash drop` are caught by neither (F422). All seven
/// are here, and one function reads them.
const RULES: &[Rule] = &[
    Rule {
        op: Destructive::ResetHard,
        sub: "reset",
        long: &["--hard"],
        short: None,
        word: None,
    },
    Rule {
        op: Destructive::CheckoutForce,
        sub: "checkout",
        long: &["--force"],
        short: Some('f'),
        word: None,
    },
    Rule {
        op: Destructive::SwitchForce,
        sub: "switch",
        long: &["--force", "--discard-changes"],
        short: Some('f'),
        word: None,
    },
    Rule {
        op: Destructive::PushForce,
        sub: "push",
        long: &["--force"],
        short: Some('f'),
        word: None,
    },
    Rule {
        op: Destructive::BranchForceDelete,
        sub: "branch",
        long: &[],
        short: Some('D'),
        word: None,
    },
    Rule {
        op: Destructive::CleanForce,
        sub: "clean",
        long: &["--force"],
        short: Some('f'),
        word: None,
    },
    Rule {
        op: Destructive::StashDrop,
        sub: "stash",
        long: &[],
        short: None,
        word: Some("drop"),
    },
];

/// Scan a whole command line for a destructive git operation.
///
/// 🚨 **It takes the line, not the first word.** The donor's guard keys on
/// `cmd_word != "git"` and therefore returns nothing for `sh -c "git reset
/// --hard"` (F422) — the hole that matters, because the tool a model reaches for
/// when the git tool refuses is the shell. This scans every token, so a wrapper
/// does not launder the command.
///
/// ⚠ **It fails closed.** The donor's fails open when git itself errors; nothing
/// here can error, and an over-eager match costs a refusal the operator can see,
/// which is the right direction for a backstop.
///
/// Callers holding an argv rather than a line join it with spaces first: the
/// tokenizer strips quote characters from token edges, so either shape scans the
/// same.
#[must_use]
pub fn destructive_git(command_line: &str) -> Option<Destructive> {
    let tokens: Vec<&str> = command_line
        .split([' ', '\t', '\n', '\r'])
        .map(|t| t.trim_matches(['"', '\'']))
        .filter(|t| !t.is_empty())
        .collect();

    for (i, token) in tokens.iter().enumerate() {
        if !is_git(token) {
            continue;
        }
        // Everything up to the next shell separator belongs to this invocation.
        let invocation: Vec<&str> = tokens[i + 1..]
            .iter()
            .take_while(|t| !matches!(**t, ";" | "&&" | "||" | "|" | "&"))
            .copied()
            .collect();
        if let Some(op) = match_rules(&drop_flag_values(&invocation)) {
            return Some(op);
        }
    }
    None
}

/// Flags whose *next* token is a value rather than an argument of git's.
///
/// Not git's option table and not trying to be: it is here so that
/// `git commit -m "reset --hard"` is not refused for the words in its message.
/// ⚠ The residual behaviour is to over-match — a message that also contains the
/// word `git` can still trip a rule — and that is the direction a backstop
/// should err in.
const TAKES_VALUE: &[&str] = &[
    "-m",
    "--message",
    "-C",
    "-c",
    "-F",
    "--file",
    "--git-dir",
    "--work-tree",
];

fn drop_flag_values<'a>(invocation: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(invocation.len());
    let mut skip = false;
    for token in invocation {
        if skip {
            skip = false;
            continue;
        }
        if TAKES_VALUE.contains(token) {
            skip = true;
        }
        out.push(*token);
    }
    out
}

/// Whether a token invokes git. A path prefix and a `.exe` suffix are both
/// ordinary on this platform, and neither may launder the command.
fn is_git(token: &str) -> bool {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .trim_end_matches(".exe");
    base.eq_ignore_ascii_case("git")
}

/// Match one git invocation's arguments against the table.
///
/// The subcommand is found by name anywhere in the argument list rather than at
/// a fixed position, so `git -C elsewhere reset --hard` is recognised exactly as
/// `git reset --hard` is. Skipping flags *and their values* precisely would take
/// the whole of git's option table; a backstop looks past both instead.
fn match_rules(rest: &[&str]) -> Option<Destructive> {
    for rule in RULES {
        let Some(at) = rest.iter().position(|t| *t == rule.sub) else {
            continue;
        };
        let args = &rest[at + 1..];
        let hit = args.iter().any(|a| {
            if rule.long.contains(a) {
                return true;
            }
            if let Some(word) = rule.word
                && *a == word
            {
                return true;
            }
            match rule.short {
                // A short cluster: `-fd` and `-df` both carry `f`. A long option
                // is never a cluster, so `--dry-run` cannot trigger on a letter.
                Some(c) => a.starts_with('-') && !a.starts_with("--") && a.contains(c),
                None => false,
            }
        });
        if hit {
            return Some(rule.op);
        }
    }
    None
}
