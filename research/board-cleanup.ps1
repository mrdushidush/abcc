# Clear the board of work that is already in the tree.
#
# Seventeen tasks, every one superseded by a commit the note names. The
# classification was made by grepping HEAD for the code each task asked for,
# not by reading titles.
#
# Run it from D:\dev\abcc:   .\research\board-cleanup.ps1
#
# WAIT until no attempt is running. `abcc run` writes to the same SQLite log
# continuously and there is no reason to race it. `Get-Process abcc` tells you.
#
# NOT IN HERE, deliberately:
#   t2598  budget::retries()  -- the one stale green that is REAL. `fn retries`
#          is not in crates/abcc-fleet/src/budget.rs, so the work never landed.
#          Try `abcc land t2598` instead; if the patch will not apply to today's
#          tree, git says so and you can reject it then.
#   t14977..t14980  the four starter tasks. Those are the work.

$ErrorActionPreference = 'Stop'

abcc reject t2691 --note "superseded: Seq::is_origin landed as a0054e7"
abcc reject t2692 --note "superseded: Seq::back landed as fb15aa1"
abcc reject t13055 --note "superseded: Seq::forward landed as c741d1f under t14581"
abcc reject t8319 --note "superseded: --version landed as 253d3c3, applied by hand"
abcc reject t11312 --note "superseded: the per-call seed landed as 223ae3a"
abcc reject t11726 --note "superseded: --version landed as 253d3c3"
abcc reject t11727 --note "superseded: --version landed as 253d3c3"
abcc reject t11728 --note "superseded: --version landed as 253d3c3"
abcc reject t11729 --note "superseded: --version landed as 253d3c3"
abcc reject t11730 --note "superseded: --version landed as 253d3c3"
abcc reject t11731 --note "superseded: --version landed as 253d3c3"
abcc reject t13054 --note "superseded: --version landed as 253d3c3"
abcc reject t14017 --note "superseded: --version landed as 253d3c3"
abcc reject t14411 --note "superseded: --version landed as 253d3c3"
abcc reject t14580 --note "superseded: the review guard landed as 498b963"
abcc reject t14787 --note "superseded: the review guard landed as 498b963"
abcc reject t14854 --note "superseded: the review guard landed as 498b963"

Write-Output ""
abcc board
