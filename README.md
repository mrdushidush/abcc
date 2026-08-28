# ABCC 2.0 — Agent Battle Command Center

An RTS-framed command center for running local model workers against real
repository tasks. One Rust binary: a fleet of two local slots, a durable SQLite
event log, a gate built out of deterministic refusals, and a terminal console
with an isometric battlefield.

> **Status: pre-alpha, and not yet open.** The first milestone is being built.
> Nothing here is released, nothing is packaged, and the repository is private
> until the succession notice on v1 is up.

## What it is

The operator gives the fleet repository work. A **slot** — a resident model plus
a tool policy — takes one **attempt** at a **task**, running Localize → Change →
Measure → Judge inside an isolated git worktree. Everything that happens is an
append-only event, the task board is a projection of that log, and restarting the
process reconstructs the board by replaying it.

Three commitments distinguish it from the thing it replaces, and each one came
out of measurement rather than taste:

- **A model verdict is a report and never a gate.** Only deterministic rungs may
  refuse. The Judge is shown the measurements and its opinion is written down for
  a human — it does not vote.
- **There is no `bool` in the outcome data.** A rung either produced a
  measurement the host watched, or it did not and says why. "The tests passed",
  "the tests all failed" and "there were no tests" are three different records,
  which is two more than the predecessor could tell apart.
- **The security posture is blast radius, not a sandbox**, and it is written in
  those words because Windows does not offer the other thing. See
  [`SECURITY.md`](SECURITY.md) when it lands with the Posture milestone.

## Relationship to ABCC v1

This is the successor to
[`agent-battle-command-center`](https://github.com/mrdushidush/agent-battle-command-center),
which keeps its own repository, its final Docker tag and its history. It is a
**new repository at a new slug**, not a branch or a major version, because a Rust
rewrite sharing a repository with a TypeScript/Python monorepo inherits a CI
configuration and a 575-line CONTRIBUTING that do not apply to it.

**None of v1's code is here.** 2.0 is a clean-room rewrite: the predecessors were
read as a specification and as a test corpus, their catalogued defects became
test cases, and the code was written fresh. That is why it ships
`MIT OR Apache-2.0` with no inherited-code caveat. The art, the audio and the
identity are David's own and are carried across unchanged. See
[`CREDITS.md`](CREDITS.md).

**There is no migration path from v1's database, and that is deliberate**: its
task rows carry no tokens, no cost and no labels. The corpora that were worth
keeping were extracted and are already running in 2.0's measurement harness.

## Where the reasoning lives

The architecture was not designed in this repository. It came out of a research
phase that produced thirteen workstream documents, two acceptance sweeps, a
signed-off summary and fourteen ADRs, against 488 numbered findings — all of it
in the ABCC 2.0 research repository, which is where every claim in the source
comments can be checked.

Source comments cite that work by finding id (`F146`), by workstream (`W3`) and
by decision record (`ADR-0004`). **A number in a comment is quoted from a named
finding**; if you cannot find the finding, treat the number as wrong.

## Licence

`MIT OR Apache-2.0`, at your option.
