# Driving it

The operator's runbook. `README.md` says what this is and `CLAUDE.md` says how to
work *in* the code; this says how to *use* the tool on a day when you are not
studying it.

Everything below is typed from the repository you want worked on. 🚨 **Run it
from that checkout**, not from a parent directory — the log is keyed on the
checkout path, so the wrong directory gets you a different, empty log and no
error.

The shell on this box is **PowerShell**, and every line below is written for it.
`export VAR=value` is bash and PowerShell answers *the term 'export' is not
recognized*; the spelling here is `$env:VAR = "value"`.

## Once, ever

Put `abcc` on PATH, so the loop below is five words rather than five paths:

```powershell
cargo install --path crates\abcc --locked
```

It lands in `~\.cargo\bin`, which is already on PATH. ⚠ **It is a copy, not
a link**: after you land a change to abcc itself, re-run that line or you are
driving with the binary from before the change. `abcc --version` does not tell
you — the version string only moves when `Cargo.toml` does.

Then name the model once, in the profile, so every shell you open already has it:

```powershell
if (-not (Test-Path $PROFILE)) { New-Item -ItemType File -Force $PROFILE }
Add-Content $PROFILE '$env:ABCC_MODEL = "qwen3.6-35b-a3b-mtp@iq3_s"'
```

## Once, at the start of a session

```powershell
lms load qwen3.6-35b-a3b-mtp@iq3_s -c 40960 --parallel 1
abcc check
```

`abcc check` asks the server which model it is actually holding and stops if the
answer is not the one you asked for. It is the whole of the pre-flight: if it
passes, `run` will not refuse on the model. What a pass looks like:

```
server      http://127.0.0.1:1234
loaded      [qwen3.6-35b-a3b-mtp@iq3_s]
model confirmed: asked qwen3.6-35b-a3b-mtp@iq3_s, the server reports it has
loaded qwen3.6-35b-a3b-mtp@iq3_s, matched exactly
pulse       1 token(s) in 260 ms — "Thinking", reasoning only
```

⚠ **`lms ps` is the truth about whether a model is loaded**, and the answer
changes without warning — the port and the API key move on every `lms load`. A
recorded load state is not evidence. Check it, do not remember it.

## The loop

Five words, in this order. Nothing else is needed on an ordinary day.

```powershell
abcc task "<what to change>" --title "<short name>"    # put it on the board
abcc run --task t42                                     # one attempt
abcc board                                              # what happened
abcc land t42                                           # it becomes a commit
abcc review t42 <minutes>                               # what it cost you to read
```

`abcc run` prints as it goes and takes a few minutes. When it ends, `board` shows
the task at one of three words:

| board says | what it means | what you do |
|---|---|---|
| `MISSION ACCOMPLISHED` | the gate measured every rung green | `abcc land t42` |
| `INTERVENTION REQUIRED` | it stopped and is asking you | read `abcc replay t42`, then retry, `take` or `reject` |
| `ABORT` | it failed and another attempt would too | `abcc reject t42 --note "<why>"` |

🚨 **`MISSION ACCOMPLISHED` is a measurement, never the model's opinion.** It
means the gate ran `cargo check`, the test suite, `cargo fmt --check` and
`cargo clippy -- -D warnings` over the before/after pair and all of them exited
0. What the model said about its own work is a `Claim` and cannot reach that
word.

### land

`abcc land t42` applies the attempt's diff to your branch and commits it. It
refuses a dirty checkout, so **commit or stash your own work first**. It prints
the sha it made.

### review

`abcc review t42 <minutes>` records how long *you* spent reading the change. That
is the one number this project is judged on — human review minutes per merged
change — and it is the one number no agent may type, because an agent typing it
fabricates the measurement.

Name the task, not the sha: `review` resolves `t42` against the log's own
landings and refuses anything it did not land. A sha or an unambiguous
abbreviation work too. Add `--boundary` when the change touched more than one
module.

## When it stalls

* `abcc replay t42` — the after-action read: every attempt under that task and how
  each one ended. This is the first thing to look at, always.
* `abcc take t42` — take the keyboard. You get a worktree cut at the attempt's
  last checkpoint, with the work as the fleet left it. `abcc release t42`
  snapshots what you did and puts it back on the board.
* `abcc run --task t42` again — a retry is cheap and the budget is 2.
* `abcc reject t42 --note "<why>"` — stop it. The note is the record of your
  judgment, so write a sentence rather than a word.

Two failures that are the environment rather than the work:

* **the model said nothing.** The champion sometimes reasons to the end of its
  budget and emits an empty answer. The phase nudges twice on its own; a retry
  recovers it most of the time. It is a sample, not a verdict.
* **`abcc check` refuses.** The model unloaded, or `lms load` moved the port.
  Re-load and re-export.

## Writing a task that lands

This is where the leverage is. Five changes have landed through `abcc land` and
they share a shape; the tasks that failed share a different one.

**A task that lands is one function, in one file, with its test beside it.** Add
a method to an `impl` block that is already there. Name the file and the exact
signature. Paste the surrounding block into the prompt so the model does not have
to go find it.

**A task that does not land spans two files.** `--version` failed five attempts
on five different walls because it needed an arm added in `cli.rs` *and* a
matching arm in `lib.rs`, and nothing in the prompt said the second one existed.
If a change needs two files, either say both, or do it yourself.

The five things worth putting in every prompt:

1. **The file and the signature**, exactly:

   > In `crates/abcc-core/src/seq.rs`, add a method
   > `Seq::distance(self, other: Seq) -> u64` to the existing `impl Seq` block,
   > directly after `forward`.

2. **The surrounding code, pasted in**, followed by *so you do not need to read
   the file to place the method*. This is worth more than any other sentence in
   the prompt.
3. **The specific lint that will bite**, by name. `-D warnings` promotes clippy's
   pedantic set to hard errors, so a style lint refuses the work exactly like a
   type error. `clippy::match_same_arms`, `clippy::manual_string_new`,
   `clippy::manual_let_else` and `clippy::trivially_copy_pass_by_ref` have each
   already cost an attempt here. Name the one you can see coming.
4. **Where the test goes**, and that it must not invent a second home:

   > Add a test to the `#[cfg(test)] mod tests` that is ALREADY at the bottom of
   > this file. It exists — do not add a second module.

   Only `seq.rs`, `attempt.rs` and `outcome.rs` in `abcc-core` have one.
5. **The grading, and an order to self-check.** Tell it the three commands it is
   graded on and tell it to call the `diagnostics` tool **with no arguments**
   before finishing. Told to call it, 5 of 6 attempts did; untold, 0 of 44 —
   asking is the whole difference.

And one line that saves a round:

> Make the edit with `apply_patch` early rather than reading more of the file
> first. The placement above is all you need.

## Land one at a time

Two attempts generated against the same base that touch the same file conflict
when the second one lands. The attempt's worktree is forked from your checkout at
the moment it runs, so **land each change before running the next task that
touches the same file** and the question never comes up. `abcc fleet` runs until
the board is quiet and is for tasks in different files.
