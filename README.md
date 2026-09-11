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
  those words because Windows does not offer the other thing. Model-written code
  runs with your privileges. [`SECURITY.md`](SECURITY.md) is the whole of it —
  eleven rows, seven with a shipped control and **four carrying a dated
  admission**, one of which is only half closed.

## Using it, as far as it goes

```
abcc where                       where this repository's log and worktrees live
abcc task "make one() return two"   put a task on the board
abcc run --task t3               one attempt, Localize then Change
abcc watch                       the reader, over the event log alone
abcc accept t3 / abcc reject t3  the operator's two endings
abcc take t3 / abcc release t3   take the keyboard, and hand it back
abcc review <sha> <minutes>      the measurement W13's ladder is defined in
abcc weights [--verify]          which bytes are behind the model you asked for
```

`abcc take t3` is *take over manually*, and it hands over a directory as well as
a state: the task goes `UNDER MANUAL CONTROL` and you get a worktree cut at its
last checkpoint — the work as the fleet left it. `abcc release t3` snapshots
what you did, takes the tree down and puts the task back on the board;
`abcc accept` and `abcc reject` end it and close the workspace on the way out.
It is refused while an attempt is flying, because that worktree belongs to a
driver that is still writing in it.

The log and the attempt worktrees live **outside** the repository, in a
per-checkout directory under the platform data directory; `abcc where` prints it
and `--home` moves it. `abcc run` needs an OpenAI-compatible server on
`ABCC_MODEL_BASE_URL` (default `http://127.0.0.1:1234`) and a model named by
`--model` or `ABCC_MODEL`.

🚨 **Two things it will not do**, and both are the design rather than a gap.
It **refuses to run against a model it cannot confirm the server is holding** —
LM Studio answers a request naming a model it does not have by using whichever
model *is* loaded, so set `ABCC_MODEL_FINGERPRINT` to a substring of the id the
server reports if the two do not match exactly. And it **never says
`Accomplished`**: there is no gate before the Gate milestone, so a working
attempt ends `Uncertain` and asks a person, which is what `abcc accept` answers.

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
