# abcc, on one page

You put a job on a board. A local model tries it in a scratch copy of your repo.
A gate runs your real checks on the result. If everything passes, you read the
diff and commit it. That is the whole tool.

**You never have to remember any of this.** Type `abcc board`; every row tells
you the command it is waiting for.

```powershell
abcc board
```
```
t14978 STANDING BY            STARTER 2: AttemptOutcome::why    abcc run --task t14978
t13055 MISSION ACCOMPLISHED   Seq::forward saturating           abcc land t13055
t14977 INTERVENTION REQUIRED  STARTER 1: Seq::distance          abcc replay t14977
```

Do what the right-hand column says. Then run `abcc board` again. That is the loop.

## Working on it together: `abcc chat`

The daily way in. You talk, the model edits, in a scratch copy of your repo.

```powershell
abcc chat "add a --port flag to serve"      # a new task, worked on as a conversation
abcc chat --task t42                        # pick one up where it was left
```

| while it works | at the `you>` prompt |
|---|---|
| **Esc** or **Ctrl-C** stops the turn; then say what to do instead | anything you type goes to the model |
| | `/diff` what it has changed so far |
| | `/done` run your checks; if they pass you see the diff and `y` lands it |
| | `/quit` stop and keep the work (`abcc chat --task t42` resumes) |

If a check refuses the change, say *yes* to keep going: the model sees what the
check said and carries on in the same conversation. The time between the diff
appearing and your `y` is recorded as your review; you do not type it.

## The three words the board answers with

| | what it means | what you do |
|---|---|---|
| **STANDING BY** | queued, nobody has tried it | `abcc run --task t42` |
| **MISSION ACCOMPLISHED** | every check passed | `abcc land t42`, then `abcc review t42 <minutes>` |
| **INTERVENTION REQUIRED** | it stopped and wants you | `abcc replay t42` to see why |

Anything else is finished and the board hides it. `abcc board --all` if you want
the graveyard.

## Starting work

```powershell
abcc task "<what to change>" --title "<short name>"
```

Then `abcc run --task t42`. It prints as it goes and takes a few minutes.

## When it says INTERVENTION REQUIRED

`abcc replay t42` prints one screen: what it tried, which tools it called, and
the line that stopped it. The last line, `ended`, is the answer.

Then pick one:

```powershell
abcc take t42 ; abcc release t42 ; abcc run --task t42   # try again
abcc take t42                                            # do it yourself
abcc reject t42 --note "<why>"                           # drop it
```

⚠ `abcc run` alone will not restart it. A run starts from STANDING BY, and
`take` then `release` is what puts it back there.

## Landing

`abcc land t42` applies the diff to your branch and commits it. It refuses if
your checkout is dirty, so commit your own work first.

`abcc review t42 <minutes>` afterwards records how long **you** spent reading
it. That number is the only thing this project is scored on, and it is the one
command nobody but you may type.

## Two commands for looking

* `abcc replay t42` — one task, after the fact. Start here, always.
* `abcc watch` — the live console, every event as it happens. It is a
  firehose on purpose. `q` quits.

## Before the first run of the day

```powershell
lms load qwen3.6-35b-a3b-mtp@iq3_s -c 40960 --parallel 1
abcc check
```

If `abcc check` says *model confirmed*, you are good. If it refuses, the model
is not loaded and nothing else will work.

## The only trap worth knowing

**Land one change before you run the next task that touches the same file.** Two
runs against the same starting point collide when the second one lands.
