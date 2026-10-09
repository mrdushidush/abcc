# Security posture

**Last reviewed 2026-09-11.** Every row of the threat model below has either a shipped control or a
dated admission that it does not. That is the Posture milestone's exit criterion (ADR-0014), and the
admissions are as much of the deliverable as the controls.

## The one sentence

🚨 **The posture is blast radius, not a sandbox.** Model-written code runs with your privileges, on
your machine, with your files. What limits the damage is that each attempt works in a disposable git
worktree, that a role which does not need a capability does not have it, and that every child process
is on a durable log. None of that is a boundary. On Windows there is no boundary underneath any of
it, and this document says so rather than implying otherwise.

**If you would not run a stranger's pull request on this machine without reading it, do not run this
unattended on this machine.**

## Why the control is "deny the class" and not a check

Ten of the eleven rows below close on one idea: **a role that does not need a capability does not get
it.** That is not a preference. W7 measured the alternatives:

- **An argument check binds only the tool that has an argument.** Proved four times in the donor
  family, by four mechanisms across three authors — a path validator that `bash` never calls, a
  destructive-git guard keyed on the first word being `git`, so `sh -c "git reset --hard"` walks past
  it. A shell is a hole in every argument check that is not in the shell.
- **A prompt binds only as far as the model complies.** The best-written untrusted-content wrapper in
  the family shipped at **39 of 50**; the best candidate replacement reached **0 of 50**. The effect
  is real and it is still a prompt, so it ships as defence in depth and never as the control.
- **There is no OS-level confinement to fall back to.** See row 3.

So the control is `Head::max_tier` — a ceiling per *role*, derived from `ToolSpec::reach` rather than
from a second list of tool names, and enforced before a tool layer is reached at all.

## The threat model

The eleven rows are W7's, unchanged. "State" is as of 2026-09-11.

| # | asset | how it is reached | state | what actually stops it |
|---|---|---|---|---|
| 1 | the working tree | a destructive git op, or an edit the model chose | ✅ shipped | the worktree is the boundary; one merged guard as a backstop |
| 2 | the rest of the filesystem | `write_file` on a new path, or `bash` | ✅ shipped | `max_tier` — a role without the exec class cannot reach it |
| 3 | the OS beneath it | any of the above, on Windows | ⚠ **admitted** | nothing. Stated, not claimed |
| 4 | credentials on disk | a hostile file asks; one `bash cat` answers | ✅ shipped (backstop) | deny the shell class; redaction limits the damage |
| 5 | the tool child's environment | inherited wholesale at spawn | ✅ shipped | `env_clear()` + a named allowlist, with a CI test |
| 6 | the model's context | hostile text in any file a worker reads | ⚠ **admitted** | nothing that is a control |
| 7 | the task itself | injection displaces it, and persists into the rewritten file | ⚠ **admitted** | the Judge can see it and may not refuse |
| 8 | egress / exfiltration | an injected instruction, then a network call | ⚠ **partly** | no networked tool exists; `bash` is still a shell |
| 9 | console and session on disk | any secret in any tool result | ✅ shipped (backstop) | redaction at one seam, enforced by a type |
| 10 | the binary's dependencies | a malicious or yanked crate among 297 | ✅ shipped | `cargo deny` + `cargo audit`, in CI, behind one gate |
| 11 | the model weights | a substituted model at first pull | ✅ shipped | a digest pinned at first sight and checked on every start |

---

## What is shipped

### Row 1 — the working tree

The boundary is **a disposable git worktree per task** (ADR-0007), cut at the task's last checkpoint.
An attempt that destroys its tree destroys a copy.

Beside it, and explicitly a **backstop rather than the control**, is one table of seven destructive
git operations — `reset --hard`, `checkout --force`, `switch --force`, `push --force`, `branch -D`,
`clean -f`, `stash drop` — in `abcc-engine/src/tools.rs`. It is read by **both** paths, the `git` tool
and `bash`, from the whole command line rather than the first word, so `sh -c "git reset --hard"` does
not launder the command. The donor's two guards recognised three operations between them and neither
copy covered the other (F422); this closes both holes, and it fails closed where the donor's failed
open.

### Row 2 — the rest of the filesystem

`Head::max_tier` (`abcc-engine/src/head.rs`): Engineering and Recon are capped at `Tier::Read`,
Builders at `Tier::Exec`, Commandos at `Tier::NoTools`. A slot carries its own ceiling and the
effective one is the narrower of the two, so a role declares what it needs and an operator declares
what a slot will allow.

The exec class is **derived** from each tool's `ToolSpec::reach` rather than declared in a second
column, so the list of "tools that can execute code" cannot drift from the list of tools that can.
`tests/policy.rs` fails the moment a `Reach::SpawnsChild` tool is admitted below `Tier::Exec`.

### Row 5 — the tool child's environment

`env_clear()` followed by a named allowlist, applied in that order so a variable can only be present
by being named (`abcc-engine/src/child.rs`). The list has no prefix rules and no wildcards. Nothing
resembling a credential is on it, and `CARGO_TARGET_DIR` is deliberately absent — a shared build cache
makes the gate certify the wrong tree (F356).

ADR-0014 §5 asked for this "as data in one const **with a CI test**". The const and the test have
existed since the Skeleton milestone; **until 2026-09-11 there was no CI to run them.** There is now.

### Rows 4 and 9 — credentials, and what lands on disk

A tool's output reaches three sinks: the durable log, the console that projects that log, and the
model's own context. The donor redacted the two disk sinks and neither of the others (F418).

`abcc-core/src/redact.rs` scrubs at **one seam** — the tool-call round in `TurnLoop` — so all three
read from one string. The enforcement is a type: `Scrubbed` has a private field and one constructor,
and both sinks (`Event::ToolCallEnded::arguments`, `Message::tool_result`) take one, so a second path
to either does not compile. The donor's denylist was a good one hung off a function `bash` never
called; a check that is not on the path is the defect this shape exists to prevent.

The denylist has two unequal halves. The **literals** this process holds (the model API key) are
matched exactly and are the only half with no false-positive story. The **shapes** — private key
blocks, `Authorization:` values, vendor-prefixed tokens, secret-shaped assignments — are heuristics.

⚠ **This is a backstop and not the control**, and it may not be described as one. A denylist over text
is precisely the shape W7 measured at 39 of 50 and rejected. What actually stops row 4 is that a role
without `Tier::Exec` cannot run `cat ~/.ssh/id_rsa` at all. Redaction limits the damage of results a
role *is* allowed to ask for.

⚠ **What it does not cover**: an operator who types a secret into `abcc task` or into an answer, a
secret in a file the model then rewrites into a diff that is applied (the tree is the tree), and any
credential whose shape is not in the table and whose value this process does not hold.

### Row 10 — dependencies

`deny.toml` and `.github/workflows/ci.yml`, copied from the donor per ADR-0014 §6 with the update
procedure included — the procedure is what stops the policy rotting into a pile of `ignore` entries.
`cargo deny check` enforces a license allow-list, a duplicate-version ban, a source allow-list and a
second pass over the RustSec database; `cargo audit` runs beside it. Both are behind a single `gate`
job so branch protection has one derived check rather than a hand-maintained list.

Measured 2026-09-11 against 297 crates: advisories, bans, licenses and sources all clean, zero
warnings. ⚠ **Before that date this repository had no CI at all**, which W7 (F419) called a
contradiction for a security-positioned project. It was right.

### Row 11 — the model weights

The one control no donor in the family has. `abcc-core`'s `WeightsChecked` event and `abcc`'s
`weights` module: a SHA-256 of the model file is pinned at first sight, compared on every run start,
and a mismatch is a run-visible event and a line on the console.

Two measurements shaped it, both 2026-09-11:

- 🚨 **The serving stack offers no digest.** `/v1/models` returns id, object and owner;
  `/api/v0/models` adds type, publisher, arch, quantization, state, context length and capabilities.
  Neither carries a digest, a length or a path — the API identifies a model by the name, which is the
  thing under suspicion. So the digest is of the file.
- 🚨 **A full digest of the champion is 54 s** (12.67 GiB at ~250 MB/s). A check costing most of a
  minute on every run is a check that gets turned off, so a start compares length and modification
  time and only re-reads when one has moved.

`WeightsOutcome` therefore has **two passing arms that are not interchangeable**: `Verified` read the
bytes; `Unchanged` read the directory entry. ⚠ A mismatch **does not re-pin** — that would make the
alarm fire once and then treat the substitute as the reference. `abcc weights --repin` is deliberate.

⚠ **What it does not cover**: an adversary who rewrites the file preserving both its length and its
modification time is invisible to a start and caught only by `abcc weights --verify`. And a **first
pin trusts what is there** — it records the bytes, it cannot vouch for them. Where the weights came
from is your assertion, not this program's.

---

## The admissions

Each of these is a row with no shipped control. They are here because a threat model with silent rows
is worse than one with honest gaps.

### Row 3 — the OS beneath all of it — *admitted 2026-09-11*

**There is no OS-level confinement, and abcc does not claim one.** `Confinement::Cwd` is what every
tool child reports on this platform, and it is a value the console can show rather than a boolean
somebody assumed.

What was considered and rejected (F408–F411): a native Windows sandbox needs Win32 token code nobody
in this family has shipped; WSL2 is not a boundary because `binfmt_misc` hands PE files back to the
host and the workspace crossing costs ~290×; a container per attempt is ruled out for the runtime on
independent grounds and would still not be a boundary against a tool child sharing the workspace.

**This is a platform fact, not a hypothesis, and the milestone does not retire it.** It makes the
stated posture honest. If a supported confinement primitive appears on this platform,
`Confinement::OsSandbox` stops being an arm that never occurs and this section is rewritten.

### Row 6 — the model's context — *admitted 2026-09-11*

**Nothing marks a file's contents as untrusted, and nothing could be relied on if it did.** A worker
that reads a file with hostile text in it has that text in its context, indistinguishable from the
operator's own instructions.

The donor's envelope is aimed at network responses and not at file contents. The measurement is the
reason this is an admission rather than a feature: the best-written wrapper of this kind in the
family holds at **39 of 50**, and the best candidate replacement at **0 of 50** (F412–F415). A
control that fails one time in five is not a control; shipping it and calling the row closed would be
worse than this paragraph.

**What limits it instead is rows 2 and 8**: the role that reads most files is capped at `Tier::Read`,
and nothing in the tool set reaches the network. Injected text can mislead the model. It cannot, on
its own, reach past what the role was given.

### Row 7 — the task itself — *admitted 2026-09-11, and verified before it was written*

**A displaced task is visible and is not a gate failure.**

ADR-0014 §5 says a displaced task should be "a verification failure, not a style issue". It is not one
today, and the check was made rather than assumed:

- The gate's `Veto` rung has **exactly one rule** — a tree whose checker could not get as far as
  running is broken rather than unmeasured — and its own source says the security rule "arrives with
  the Posture milestone". It has not; declaring it now would be a name standing in for a
  specification, and a veto that cannot fire is one an operator trusts for the wrong reason.
- The **Judge does see it**. `judge::Dossier` carries the task's title and prompt beside the patch, so
  a change that does something other than the task is within what the reviewer is shown. ⚠ The
  nearest thing to evidence is F536: across 15 calls the best answer was the one that checked the
  change against *the scope the ticket states* — but that was an **incomplete** change, not a
  displaced one, so it is a reason to think the Judge could notice this and not a measurement that
  it does.
- 🚨 **And the Judge may never refuse.** Only deterministic rungs may (`AttemptPhase::may_refuse` is
  `!uses_model()`); a model verdict is a `Claim` and there is no function in the workspace that turns
  one into an `Outcome`. That is a standing ruling and it is not being revisited for this row.

So: an operator reading the Judge's report will usually see a displaced task called out. Nothing
stops the attempt, and **`abcc accept` is a person's verb** — a working task reaches a terminal state
only when somebody says so. Closing this row properly needs a deterministic detector for *this diff
does not address this task*, and nobody has one.

### Row 8 — egress — *partly shipped, admitted 2026-09-11*

**No tool in the set reaches the network**, and that is worth stating exactly. The nine tools are
`read_file`, `list_files`, `search`, `write_file`, `apply_patch`, `bash`, `run_tests`, `diagnostics`
and `git`. There is no `web_fetch`, and the donor's SSRF guard has nothing here to guard — which is
just as well, because an SSRF guard is not an exfiltration control.

⚠ **But `bash` is a shell and `git` pushes.** Three of the nine spawn children, and a child with
`PATH` can reach `curl`. So egress is closed for every role capped below `Tier::Exec` — the Planner,
Recon and the Judge — and **open for Builders**, which is the role that needs to run tests.

⚠ **`Mode::SinglePlayer` is a policy about providers, not about tools**, and
`ProviderClass::leaves_the_machine` currently has no caller: it describes where the *model* runs, and
it does not and cannot constrain what a tool child does. Do not read it as an air gap. ADR-0014's
`--offline` for unattended roles is the leg that would close this, and it is not built.

---

## Reporting something

This is a personal research project with no release and nothing published to crates.io. If you find
something, open an issue — or, if it is the kind of thing that should not be public, report it
privately through **Report a vulnerability** on the repository's Security tab, or mail the address in
`Cargo.toml`. There is no bounty and no response-time commitment.

## Things that are deliberately not here

- **A list of forbidden commands.** A sentence binds only as far as the model complies. If an action
  should not be available, the tool is absent from the role's set or the tier refuses it.
- **A `--force` on the model check.** A check that can be waived by a flag is a check that will be
  waived, and a run against an unconfirmed model produces numbers that look exactly like good ones.
- **A security scanner rung.** See row 7.
