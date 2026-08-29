//! 🚨 **The Judge over the corpora — ADR-0008's own falsifier, on a population
//! with an answer key.**
//!
//! `tests/corpus.rs` produces one dossier per measured tree: the title, the
//! prompt, the patch and the measurements, which is exactly
//! [`judge::Dossier`]'s field list. This asks the champion for a review of each
//! one and writes down what came back. **It runs second and it reads that
//! directory** — nothing here builds a tree or runs a rung.
//!
//! # What this is for, and what it is not for
//!
//! It is not a scoreboard. `Measured::headline` was computed by the ladder before
//! any of this ran, and there is no function in this workspace that turns a
//! `Claim` into an `Outcome`. What is being counted is **report volume and what
//! the volume is made of**, which is the one number ADR-0008 says would falsify
//! the phase: *a reviewer that produces findings nobody can act on costs the
//! operator the minutes W13 grades this project on.*
//!
//! **121 trees in three populations**, and they ask three different questions —
//! see [`population`]:
//!
//! * **59 `wrong`.** The 35 the ladder called `Green` are the case the Judge
//!   exists for: the donor's visible tests are happy-path and a wrong answer
//!   passes them on purpose, so the deterministic gate accepts them and the
//!   hidden reviewer tests reject them. **A finding there is the phase earning
//!   its place.** The 13 it refused are **F521**'s population — on the first
//!   live review of a refused tree the champion spent finding 1 of 2 restating
//!   the rung it had just been shown, which is volume with no information in it.
//! * **3 `sham`.** The corpus's own tempting local fix. Every rung passes and the
//!   answer key still fails it, so here the Judge is not a second opinion — it is
//!   the only opinion there is.
//! * **59 `correct`.** 🚨 **A finding here is a candidate false positive.** The
//!   falsifier is whether these reports are worth an operator's minutes, and a
//!   reviewer that finds something on a tree that is fine spends them for
//!   nothing. It cannot fail one — the headline was computed before this ran.
//!
//! ⚠ **Recall is not scored automatically and neither is precision.**
//! `verify.sh` grades a *tree* and a reviewer's prose is not a tree, so what is
//! written down is the finding; reading it against the answer key is a person's
//! job. The restatement flag below is a **candidate** rule for the same reason,
//! and every candidate is printed verbatim. A keyword count is not a finding
//! count.
//!
//! # It is restartable, on purpose
//!
//! One call is 25 s and 122 s measured, so 121 of them is a couple of hours. A
//! dossier whose review file already exists is skipped, so an interrupted run
//! resumes rather than starting over, a single task can be re-asked by deleting
//! one file, and a population added later costs only its own calls. ⚠
//! **Sequential, and `--parallel 1` on the server**: two turns in flight push
//! each other past the 90 s idle gap.
//!
//! # Running it
//!
//! ```text
//! ABCC_CORPUS_OUT=D:\dev\ABCC_20_powerd_by_claudette\research\corpus-run \
//! ABCC_MODEL="qwen3.6-35b-a3b-mtp@iq3_s" \
//! ABCC_URL=http://localhost:1234 \
//!   cargo test -p abcc-gate --test corpus_review -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{env, fs};

use abcc_core::event::Event;
use abcc_core::outcome::{Headline, Report};
use abcc_core::seq::{AttemptId, Seq};
use abcc_engine::control::ControlPoint;
use abcc_engine::openai::OpenAiCompat;
use abcc_engine::provider::Body;
use abcc_engine::turn::{NoTools, PhaseEnded, TurnLoop};
use abcc_engine::{Head, Provider};
use abcc_gate::Measured;
use abcc_gate::judge::{self, Dossier, Finding, Review};
use serde::Deserialize;

/// One tree, as `tests/corpus.rs` wrote it down.
#[derive(Debug, Deserialize)]
struct Written {
    id: String,
    lang: String,
    title: String,
    prompt: String,
    patch: String,
    report: Report,
    headline: Headline,
}

fn out_dir() -> PathBuf {
    let raw = env::var_os("ABCC_CORPUS_OUT")
        .expect("set ABCC_CORPUS_OUT to the directory `tests/corpus` wrote — see the module docs");
    PathBuf::from(raw)
}

fn dossiers(dir: &Path) -> Vec<(String, Written)> {
    let only = env::var("ABCC_CORPUS_ONLY").unwrap_or_default();
    let dossiers = dir.join("dossiers");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dossiers)
        .unwrap_or_else(|e| {
            panic!(
                "{} is not readable: {e} — run `--test corpus` first",
                dossiers.display()
            )
        })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let slug = path.file_stem()?.to_string_lossy().into_owned();
            if !only.is_empty() && !only.split(',').any(|w| slug.ends_with(w.trim())) {
                return None;
            }
            let text = fs::read_to_string(&path).ok()?;
            let written: Written = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} is not a dossier: {e}", path.display()));
            Some((slug, written))
        })
        .collect()
}

/// Whether a finding is **a candidate** for restating a rung the brief showed it.
///
/// 🚨 Deliberately generous and deliberately not trusted. The rule is: the thing
/// the reviewer says to run is one of the rung commands, or the thing it expects
/// is an exit status. That will catch a finding whose runnable evidence happens
/// to be a test command legitimately — which is why every hit is printed in full
/// and the summary calls them candidates. **The number is a pointer to reading,
/// not a measurement.**
fn restates_a_rung(f: &Finding) -> bool {
    let call = f.call.to_lowercase();
    let commands = [
        "cargo test",
        "cargo clippy",
        "cargo check",
        "python -m pytest",
        "pytest",
        "node test_basic",
    ];
    let expects_a_status = |s: &str| {
        let s = s.to_lowercase();
        s.contains("exit 0") || s.contains("exit code 0") || s.contains("exit status 0")
    };
    commands.iter().any(|c| call.contains(c))
        || expects_a_status(&f.expected)
        || expects_a_status(&f.actual)
}

fn verdict(headline: &Headline) -> &'static str {
    match headline {
        Headline::Green { .. } => "GREEN",
        Headline::Red { .. } => "RED",
        Headline::Unverified { .. } => "UNVERIFIED",
    }
}

/// 🚨 **Which tier of the corpus this tree is, taken from the dossier's own
/// filename.**
///
/// The three are different questions and averaging them would answer none:
///
/// * **`wrong`** — a first attempt that is wrong. *Did the reviewer see it?*
/// * **`sham`** — the corpus's own tempting local fix, which fixes the symptom
///   the ticket named. Only the K suite ships one, so there are 3. *Did the
///   reviewer see past it?*
/// * **`correct`** — the answer key's right answer. 🚨 **A finding here is a
///   candidate false positive**, and that is the number the exit criterion's
///   second clause actually needs: the falsifier is whether these reports are
///   worth an operator's minutes, and a reviewer that finds something on a tree
///   that is fine spends them for nothing. ADR-0008's **1 of 34** is borrowed
///   from Phase 1; 48 correct trees is this project's own.
fn population(slug: &str) -> &'static str {
    if slug.ends_with("-correct") {
        "correct"
    } else if slug.ends_with("-sham") {
        "sham"
    } else {
        "wrong"
    }
}

/// One reviewed tree, as the table carries it.
struct Row {
    id: String,
    lang: String,
    population: &'static str,
    ladder: &'static str,
    ending: String,
    findings: usize,
    restatements: usize,
    seconds: f32,
    prompt_tokens: u32,
    completion_tokens: u32,
    reasoning_tokens: u32,
}

/// 🚨 **The one model call per wrong tree, and it can neither refuse nor fail
/// anything.**
///
/// There is no assertion about what the reviewer says, and that is the design
/// rather than a gap: the ladder's verdict was computed before this ran, this
/// file writes no `Outcome`, and a review that times out or comes back malformed
/// is recorded as exactly that. The only assertion is that the *instrument*
/// worked — that something was written down for every dossier asked about.
#[test]
#[ignore = "one model call per wrong tree — needs the champion loaded at --parallel 1"]
fn the_judge_reads_every_wrong_tree_the_corpora_ship() {
    let dir = out_dir();
    let model = env::var("ABCC_MODEL").expect("set ABCC_MODEL to the model to ask");
    let url = env::var("ABCC_URL").unwrap_or_else(|_| "http://localhost:1234".to_owned());
    let reviews = dir.join("reviews");
    fs::create_dir_all(&reviews).expect("the reviews directory");

    let provider = OpenAiCompat::new(&url).expect("the provider");
    let tools = NoTools::for_head(Head::Commandos);
    let loop_ = TurnLoop::new(&provider as &dyn Provider, &tools, model.clone());

    let all = dossiers(&dir);
    println!(
        "\n### THE JUDGE OVER {} WRONG TREES — {model} at {url}\n",
        all.len()
    );

    let mut rows: Vec<Row> = Vec::new();
    for (n, (slug, written)) in all.iter().enumerate() {
        let path = reviews.join(format!("{slug}.json"));
        if path.exists() {
            println!("=== {} — already reviewed, skipped", written.id);
            if let Some(row) = row_from(&path, written, slug) {
                rows.push(row);
            }
            continue;
        }

        rows.push(ask(&loop_, written, n, &path, slug));
    }

    summarise(&rows);
    write_tsv(&dir.join("reviews.tsv"), &rows);
    assert_eq!(
        rows.len(),
        all.len(),
        "every dossier asked about should have a row, answered or not"
    );
}

/// **One tree: build the brief, make the call, write down what came back.**
///
/// 🚨 Every ending is `Ok` here in the sense that matters — the ladder's verdict
/// is already in `written.headline` and nothing this returns can move it. A
/// review that times out, says nothing or comes back malformed produces a row
/// saying so, which is the record ADR-0008 wants and is not a failure of the
/// tree.
fn ask(loop_: &TurnLoop<'_>, written: &Written, n: usize, path: &Path, slug: &str) -> Row {
    // `changed` is not in the brief — `Dossier` carries the report and the
    // report carries the rungs — so it is not reconstructed. Putting a guessed
    // value there would be a field nobody reads that could still be wrong.
    let measured = Measured {
        report: written.report.clone(),
        headline: written.headline.clone(),
        changed: Vec::new(),
    };
    let brief = judge::brief(&Dossier {
        title: &written.title,
        prompt: &written.prompt,
        patch: &written.patch,
        measured: &measured,
    });
    let mut body = Body::opening(brief.clone());
    let (mut control, _handle) = ControlPoint::new();
    // The log is kept in memory: the durable half is `abcc-drive`'s and this is
    // an instrument, but the *events* are what say a phase was nudged or a claim
    // recorded, so they are counted rather than dropped.
    let mut events: Vec<Event> = Vec::new();
    let mut journal = |e: Event| events.push(e);

    let started = Instant::now();
    let ended = loop_.run(
        Head::Commandos,
        AttemptId::at(Seq::new(i64::try_from(n).unwrap_or(0))),
        Some(judge::REVIEW),
        &mut body,
        &mut control,
        &mut journal,
    );
    let seconds = started.elapsed().as_secs_f32();

    let report = ended.report().clone();
    let (ending, text) = match &ended {
        PhaseEnded::Answered { text, .. } => ("answered".to_owned(), Some(text.clone())),
        PhaseEnded::Stopped { stop, .. } => (format!("stopped: {stop:?}"), None),
        PhaseEnded::Unmeasured { why, .. } => (format!("unmeasured: {why}"), None),
    };
    let review: Option<Review> = text.as_deref().and_then(|t| judge::parse(t).ok());
    let findings = review.as_ref().map_or(0, |r| r.findings.len());
    let restatements = review.as_ref().map_or(0, |r| {
        r.findings.iter().filter(|f| restates_a_rung(f)).count()
    });

    println!(
        "=== {} [{}] ladder={} {ending} — {findings} finding(s), {restatements} restatement \
         candidate(s), {seconds:.1} s ({} prompt + {} completion tokens)",
        written.id,
        written.lang,
        verdict(&written.headline),
        report.prompt_tokens,
        report.completion_tokens,
    );
    show(review.as_ref(), text.as_deref());

    let record = serde_json::json!({
        "id": written.id,
        "lang": written.lang,
        "ladder": verdict(&written.headline),
        "ending": ending,
        "seconds": seconds,
        "prompt_tokens": report.prompt_tokens,
        "completion_tokens": report.completion_tokens,
        "reasoning_tokens": report.reasoning_tokens,
        "trace": format!("{:?}", report.trace),
        "turns": report.turns,
        "events": events.len(),
        "raw": text,
        "review": review,
        "brief_chars": brief.len(),
    });
    fs::write(
        path,
        serde_json::to_string_pretty(&record).expect("a review serializes"),
    )
    .expect("writing a review");

    Row {
        id: written.id.clone(),
        lang: written.lang.clone(),
        population: population(slug),
        ladder: verdict(&written.headline),
        ending,
        findings,
        restatements,
        seconds,
        prompt_tokens: report.prompt_tokens,
        completion_tokens: report.completion_tokens,
        reasoning_tokens: report.reasoning_tokens.unwrap_or(0),
    }
}

/// Every finding in full, and every restatement candidate marked — because the
/// flag is a pointer to reading rather than a measurement.
fn show(review: Option<&Review>, text: Option<&str>) {
    if let Some(r) = review {
        println!("    assessment: {}", one_line(&r.assessment));
        for f in &r.findings {
            let mark = if restates_a_rung(f) {
                "⚠ RESTATES?"
            } else {
                "  finding  "
            };
            println!("    {mark} {} — {}", one_line(&f.at), one_line(&f.defect));
            println!("        run: {}", one_line(&f.call));
            println!(
                "        expected: {} / actual: {}",
                one_line(&f.expected),
                one_line(&f.actual)
            );
        }
    } else if let Some(t) = text {
        println!(
            "    ⚠ did not parse as the shape it was asked for; kept verbatim ({} chars)",
            t.len()
        );
    }
}

/// Re-read a review written by an earlier run, so a resumed run still summarises
/// the whole population rather than only the part it did itself.
fn row_from(path: &Path, written: &Written, slug: &str) -> Option<Row> {
    let text = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let review: Option<Review> = serde_json::from_value(v.get("review")?.clone())
        .ok()
        .flatten();
    let findings = review.as_ref().map_or(0, |r| r.findings.len());
    let restatements = review.as_ref().map_or(0, |r| {
        r.findings.iter().filter(|f| restates_a_rung(f)).count()
    });
    let num = |k: &str| {
        u32::try_from(v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0)).unwrap_or(0)
    };
    Some(Row {
        id: written.id.clone(),
        lang: written.lang.clone(),
        population: population(slug),
        ladder: verdict(&written.headline),
        ending: v
            .get("ending")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_owned(),
        findings,
        restatements,
        #[allow(clippy::cast_possible_truncation)]
        seconds: v
            .get("seconds")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32,
        prompt_tokens: num("prompt_tokens"),
        completion_tokens: num("completion_tokens"),
        reasoning_tokens: num("reasoning_tokens"),
    })
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(140) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// One block of the table: how many trees, how many answered, how much volume.
///
/// 🚨 **`silent` is the column the correct half is about.** A tree the reviewer
/// answered about and found nothing on is the *good* outcome there and the *bad*
/// one on a wrong tree, so it is reported rather than averaged into a rate whose
/// direction depends on which half you are reading.
fn block(label: &str, group: &[&Row]) {
    if group.is_empty() {
        return;
    }
    let answered = group.iter().filter(|r| r.ending == "answered").count();
    let findings: usize = group.iter().map(|r| r.findings).sum();
    let restated: usize = group.iter().map(|r| r.restatements).sum();
    let silent = group
        .iter()
        .filter(|r| r.ending == "answered" && r.findings == 0)
        .count();
    let mut secs: Vec<f32> = group.iter().map(|r| r.seconds).collect();
    secs.sort_by(f32::total_cmp);
    let median = secs[secs.len() / 2];
    println!(
        "{label:<22} {:>6} {answered:>9} {findings:>9} {silent:>7} {restated:>13} {median:>9.0}",
        group.len()
    );
}

fn summarise(rows: &[Row]) {
    println!("\n--- THE JUDGE OVER THE CORPORA ---");
    println!(
        "{:<22} {:>6} {:>9} {:>9} {:>7} {:>13} {:>9}",
        "population / ladder",
        "trees",
        "answered",
        "findings",
        "silent",
        "restatement?",
        "median s"
    );
    for pop in ["correct", "wrong", "sham"] {
        let all: Vec<&Row> = rows.iter().filter(|r| r.population == pop).collect();
        if all.is_empty() {
            continue;
        }
        block(&format!("{pop} (all)"), &all);
        for ladder in ["GREEN", "RED", "UNVERIFIED"] {
            let group: Vec<&Row> = all.iter().copied().filter(|r| r.ladder == ladder).collect();
            block(&format!("  {pop} / {ladder}"), &group);
        }
    }
    let answered = rows.iter().filter(|r| r.ending == "answered").count();
    let findings: usize = rows.iter().map(|r| r.findings).sum();
    let restated: usize = rows.iter().map(|r| r.restatements).sum();
    let wall: f32 = rows.iter().map(|r| r.seconds).sum();
    let silent = rows
        .iter()
        .filter(|r| r.ending == "answered" && r.findings == 0)
        .count();
    println!(
        "{:<22} {:>6} {answered:>9} {findings:>9} {silent:>7} {restated:>13} {:>9}",
        "TOTAL",
        rows.len(),
        ""
    );
    println!("\nwall clock: {:.0} s ({:.1} min)", wall, wall / 60.0);
    let pt: u64 = rows.iter().map(|r| u64::from(r.prompt_tokens)).sum();
    let ct: u64 = rows.iter().map(|r| u64::from(r.completion_tokens)).sum();
    let rt: u64 = rows.iter().map(|r| u64::from(r.reasoning_tokens)).sum();
    println!("tokens: {pt} prompt, {ct} completion, of which {rt} reasoning");
    if ct > 0 {
        // 🚨 F522 is this ratio, and the corpora are the population that says
        // whether 93-96% was one call or the shape of the phase.
        #[allow(clippy::cast_precision_loss)]
        let share = 100.0 * rt as f64 / ct as f64;
        println!("reasoning share of completion: {share:.1}%");
    }
}

fn write_tsv(path: &Path, rows: &[Row]) {
    let mut s = String::from(
        "task\tlang\tpopulation\tladder\tending\tfindings\trestatement_candidates\tseconds\tprompt_tokens\tcompletion_tokens\treasoning_tokens\n",
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{}\t{}",
            r.id,
            r.lang,
            r.population,
            r.ladder,
            r.ending,
            r.findings,
            r.restatements,
            r.seconds,
            r.prompt_tokens,
            r.completion_tokens,
            r.reasoning_tokens
        );
    }
    fs::write(path, s).expect("the summary table");
    println!("\nwritten: {}", path.display());
}
