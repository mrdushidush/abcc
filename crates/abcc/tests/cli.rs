//! The argument surface, which is a pure function and is tested like one.
//!
//! Every shape here is one somebody will type. The parse is hand-rolled and has
//! no dependency behind it, so the guard against a flag that silently does
//! nothing is this file rather than a library's reputation.

use std::time::Duration;

use abcc::cli::{self, CliError, Command, Paint};
use abcc_engine::{Temperature, Tier};
use abcc_tui::Theme;

fn parse(line: &str) -> Result<cli::Invocation, CliError> {
    cli::parse(line.split_whitespace().map(str::to_owned))
}

/// Quoting is the shell's job, so a test that needs an argument with a space in
/// it has to hand the words over itself.
fn parse_args(args: &[&str]) -> Result<cli::Invocation, CliError> {
    cli::parse(args.iter().map(|a| (*a).to_owned()))
}

// ---------------------------------------------------------------------------
// the shape of an invocation
// ---------------------------------------------------------------------------

#[test]
fn nothing_at_all_is_help_and_help_is_not_a_failure() {
    assert_eq!(parse(""), Err(CliError::Help));
    assert_eq!(parse("--help"), Err(CliError::Help));
    assert_eq!(parse("run --help"), Err(CliError::Help));
    assert_eq!(abcc::AppError::Cli(CliError::Help).exit_code(), 0);
}

/// The version names the commit the binary was built from, so a log says which
/// build wrote it: `0.1.0+52f7bd0`, or the bare package version without git.
#[test]
fn the_version_names_the_commit_it_was_built_from() {
    let package = env!("CARGO_PKG_VERSION");
    let head = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    match head {
        Some(sha) => assert_eq!(abcc::VERSION, format!("{package}+{sha}")),
        None => assert_eq!(abcc::VERSION, package),
    }
}

#[test]
fn the_version_flag_prints_and_exits_zero() {
    let version = abcc::VERSION.to_owned();
    assert_eq!(parse("--version"), Err(CliError::Version(version.clone())));
    assert_eq!(parse("board --version"), Err(CliError::Version(version)));
    let v = abcc::AppError::Cli(CliError::Version("0.0.0".to_owned()));
    //   🚨 THE SEAM THE GATE CANNOT SEE. `exit_code` ends in `_ => 1`, so a
    // variant nobody adds an arm for compiles, passes every other test and
    // reports a failing status for something `main` exits 0 on.
    assert_eq!(v.exit_code(), 0);
}

#[test]
fn a_usage_error_exits_two_and_a_refusal_exits_one() {
    let usage = abcc::AppError::Cli(CliError::Usage("nope".to_owned()));
    assert_eq!(usage.exit_code(), 2);
    assert_eq!(abcc::AppError::Refused("nope".to_owned()).exit_code(), 1);
}

#[test]
fn the_global_flags_may_appear_before_or_after_the_verb() {
    let before = parse("--repo D:/x board").expect("before");
    let after = parse("board --repo D:/x").expect("after");
    assert_eq!(before, after);
    assert_eq!(before.repo.as_deref(), Some(std::path::Path::new("D:/x")));
}

#[test]
fn an_unknown_verb_is_named_rather_than_ignored() {
    let Err(CliError::Usage(said)) = parse("fly") else {
        panic!("an unknown verb has to be a usage error");
    };
    assert!(said.contains("\"fly\""), "{said}");
}

#[test]
fn a_flag_with_no_value_after_it_is_an_error_and_not_a_silent_default() {
    for line in ["run --model", "task --title", "review a 1 --by"] {
        let Err(CliError::Usage(said)) = parse(line) else {
            panic!("{line} should not parse");
        };
        assert!(said.contains("needs a value"), "{line}: {said}");
    }
}

// ---------------------------------------------------------------------------
// tasks
// ---------------------------------------------------------------------------

#[test]
fn a_task_id_is_the_same_whether_or_not_it_carries_its_prefix() {
    // `t42` is what the board prints, so it is what an operator pastes back.
    let prefixed = parse("accept t42").expect("prefixed");
    let bare = parse("accept 42").expect("bare");
    assert_eq!(prefixed, bare);
    assert_eq!(
        prefixed.command,
        Command::Accept {
            task: cli::TaskRef(42),
            note: None
        }
    );
}

#[test]
fn a_task_id_that_is_not_one_says_what_one_looks_like() {
    for bad in ["accept banana", "accept t0", "accept -3", "run --task tt1"] {
        let Err(CliError::Usage(said)) = parse(bad) else {
            panic!("{bad} should not parse");
        };
        assert!(said.contains("`t42`"), "{bad}: {said}");
    }
}

#[test]
fn a_prompt_with_spaces_arrives_whole() {
    let parsed = parse_args(&["task", "make one() return two"]).expect("task");
    assert_eq!(
        parsed.command,
        Command::Task {
            prompt: "make one() return two".to_owned(),
            title: None
        }
    );
}

#[test]
fn two_positionals_are_refused_rather_than_one_being_dropped() {
    let Err(CliError::Usage(said)) = parse("task make it faster") else {
        panic!("an unquoted prompt should not parse");
    };
    assert!(said.contains("quote it"), "{said}");
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

#[test]
fn run_takes_its_options_and_nothing_positional() {
    let parsed = parse("run --task t7 --model champ --url http://x:1/v1 --unit 1 --rounds 3 --idle-gap 12.5 --fingerprint iq3_s")
        .expect("run");
    let Command::Run(run) = parsed.command else {
        panic!("expected a run");
    };
    assert_eq!(run.task, Some(cli::TaskRef(7)));
    assert_eq!(run.model.as_deref(), Some("champ"));
    assert_eq!(run.base_url.as_deref(), Some("http://x:1/v1"));
    assert_eq!(run.fingerprint.as_deref(), Some("iq3_s"));
    assert_eq!(run.unit, Some(1));
    assert_eq!(run.rounds, Some(3));
    assert_eq!(run.idle_gap, Some(Duration::from_millis(12_500)));

    let Err(CliError::Usage(said)) = parse("run t7") else {
        panic!("a bare task id after `run` should not parse");
    };
    assert!(said.contains("no arguments"), "{said}");
}

/// 🚨 **The slot's ceiling is a tier, and a word that is not one of the four is
/// refused with the four spelled out.**
///
/// It must never become a list of tool names: W7 proved four times, across four
/// mechanisms and three authors, that an argument check binds only the tool that
/// has an argument. The class is the thing that can be denied.
#[test]
fn a_ceiling_is_one_of_four_tiers_on_both_run_and_fleet() {
    for (line, expected) in [
        ("run --ceiling no-tools", Tier::NoTools),
        ("run --ceiling read", Tier::Read),
        ("run --ceiling write", Tier::Write),
        ("run --ceiling exec", Tier::Exec),
    ] {
        let parsed = parse(line).expect(line);
        let Command::Run(run) = parsed.command else {
            panic!("expected a run: {line}");
        };
        assert_eq!(run.ceiling, Some(expected), "{line}");
    }

    // The same flag on the fleet, which is where a slot ceiling actually
    // belongs — `run` gets it because a run is one attempt in one slot too.
    let Command::Fleet(fleet) = parse("fleet --ceiling read").expect("fleet").command else {
        panic!("expected a fleet");
    };
    assert_eq!(fleet.ceiling, Some(Tier::Read));

    // Unset is unset, and it is the caller who decides what that means — not a
    // default buried in the parser.
    let Command::Fleet(bare) = parse("fleet").expect("fleet").command else {
        panic!("expected a fleet");
    };
    assert_eq!(bare.ceiling, None);

    // ⚠ The error lists the four rather than only saying no: an operator who
    // typed `readonly` needs the spelling, not a verdict.
    let Err(CliError::Usage(said)) = parse("run --ceiling readonly") else {
        panic!("`readonly` is not a ceiling and should not parse");
    };
    for tier in ["no-tools", "read", "write", "exec"] {
        assert!(said.contains(tier), "{said}");
    }
}

#[test]
fn an_idle_gap_that_is_not_a_positive_length_of_time_is_refused() {
    // ⚠ Zero is not a stricter timeout, it is a stream that has already failed.
    for bad in [
        "run --idle-gap 0",
        "run --idle-gap -5",
        "run --idle-gap soon",
    ] {
        assert!(matches!(parse(bad), Err(CliError::Usage(_))), "{bad}");
    }
}

#[test]
fn watch_names_an_unknown_theme_rather_than_falling_back_to_one() {
    assert_eq!(
        parse("watch").expect("watch").command,
        Command::Watch {
            theme: Theme::Command
        }
    );
    assert_eq!(
        parse("watch --theme classic").expect("classic").command,
        Command::Watch {
            theme: Theme::Classic
        }
    );
    let Err(CliError::Usage(said)) = parse("watch --theme sixel") else {
        panic!("an unknown theme should not parse");
    };
    assert!(said.contains("sixel"), "{said}");
}

// ---------------------------------------------------------------------------
// review — W13's ladder, and the unit conversion that happens once
// ---------------------------------------------------------------------------

#[test]
fn minutes_become_seconds_at_the_edge() {
    // 🚨 The record's unit is seconds because a float that is summed over months
    // stops summing exactly. The conversion is here, once, so nothing downstream
    // holds a float at all.
    let cases = [("2.5", 150_u32), ("1", 60), ("0", 0), ("0.5", 30)];
    for (typed, expected) in cases {
        let parsed = parse(&format!("review abc123 {typed}")).expect(typed);
        let Command::Review { seconds, .. } = parsed.command else {
            panic!("expected a review");
        };
        assert_eq!(seconds, expected, "{typed} minutes");
    }
}

#[test]
fn a_review_that_is_not_a_length_of_time_is_refused() {
    for bad in ["review abc -1", "review abc soon", "review abc 1e30"] {
        assert!(matches!(parse(bad), Err(CliError::Usage(_))), "{bad}");
    }
}

#[test]
fn a_review_carries_who_and_whether_it_crossed_a_boundary() {
    let parsed = parse("review abc123 8 --by david --boundary").expect("review");
    assert_eq!(
        parsed.command,
        Command::Review {
            change: "abc123".to_owned(),
            seconds: 480,
            by: Some("david".to_owned()),
            // M3 counts boundary-crossing changes specifically, so this is a field
            // and not a note somebody has to read.
            crossed_boundary: true,
        }
    );
}

#[test]
fn review_needs_both_the_change_and_the_minutes() {
    for bad in ["review", "review abc123", "review a 1 2"] {
        assert!(matches!(parse(bad), Err(CliError::Usage(_))), "{bad}");
    }
}

// ---------------------------------------------------------------------------
// the help text, which is the only documentation an operator gets
// ---------------------------------------------------------------------------

#[test]
fn every_verb_the_parser_accepts_is_in_the_usage_text() {
    // A command that parses and is not in `--help` is a command nobody finds.
    //
    // ⚠ The list is written out and therefore does not grow by itself, which
    // is the trap `CLAUDE.md` names about the cheap test rung: a list of names
    // does not fail when the workspace grows, it just stops covering things.
    // Check it against the `match` in `cli::parse` when a verb is added.
    for verb in [
        "where", "task", "board", "replay", "fun", "run", "fleet", "watch", "check", "breaker",
        "review", "accept", "reject", "paint", "take", "release",
    ] {
        // The verb is known to the parser: whatever else it complains about, it
        // never complains that this is not a command.
        let complaint = match parse(verb) {
            Ok(_) | Err(CliError::Help | CliError::Version(_)) => String::new(),
            Err(CliError::Usage(said)) => said,
        };
        assert!(
            !complaint.contains("is not a command"),
            "{verb}: {complaint}"
        );
        assert!(
            cli::USAGE.contains(&format!("abcc {verb}")),
            "{verb} is missing from the usage text"
        );
    }
}

/// The eighth verb takes a task and nothing else, at both spellings of an id.
///
/// ⚠ No `--note`: `accept` and `reject` end a task and a note is the record of
/// why, while a take-over is the *beginning* of some work and the record of it is
/// the checkpoint at the other end.
#[test]
fn take_and_release_each_name_one_task_and_take_no_flags() {
    let Command::Take { task } = parse("take t42").expect("take").command else {
        panic!("take has to parse to a take");
    };
    assert_eq!(task, cli::TaskRef(42));
    let Command::Release { task } = parse("release 42").expect("release").command else {
        panic!("release has to parse to a release");
    };
    assert_eq!(task, cli::TaskRef(42));

    for line in ["take", "release"] {
        let Err(CliError::Usage(said)) = parse(line) else {
            panic!("{line} with no task is a usage error");
        };
        assert!(said.contains("a task"), "{line}: {said}");
    }
    for line in ["take t42 --note x", "release t42 now"] {
        assert!(
            parse(line).is_err(),
            "{line} took something it does not have"
        );
    }
}

/// `paint` defaults, and the two flags that can be spelled wrong.
///
/// ⚠ The sprite directory has **no default here** and is not required here
/// either: the parse records what the operator said and `paint` resolves the
/// environment. A parser that read `ABCC_SPRITES` would make this test depend on
/// the machine it runs on.
#[test]
fn paint_defaults_to_a_hundred_and_twenty_pixel_sprite_on_a_640_by_360_field() {
    let Command::Paint(Paint {
        sprites,
        px,
        size,
        corpus,
        play,
        cell,
    }) = parse("paint").expect("paint").command
    else {
        panic!("not paint");
    };
    assert_eq!(sprites, None);
    assert_eq!(play, None, "the default is one frame, not an animation");
    assert_eq!(
        cell, 16,
        "the assumed cell height moved. It is short on purpose (F730): a cell \
         taller than the terminal's real rows reserves too few of them and the \
         picture is clipped in silence, which is the failure nobody can see"
    );
    assert!(
        !corpus,
        "the default is the fleet; the art is behind --corpus"
    );
    assert_eq!(
        px, 120,
        "120 is David's judgement over the art that is actually drawn (F565), \
         and unlike the 150 it replaces it is inside F143's measured 75-120 band"
    );
    assert_eq!(size, (640, 360));

    let Command::Paint(Paint {
        sprites,
        px,
        size,
        corpus,
        ..
    }) = parse("paint --sprites D:/art --px 75 --size 320x200 --corpus")
        .expect("flags")
        .command
    else {
        panic!("not paint");
    };
    assert!(corpus, "--corpus was dropped");
    assert_eq!(sprites.as_deref(), Some("D:/art"));
    assert_eq!(px, 75);
    // 320x200 is VGA mode 13h, which is where the 256-colour palette comes from.
    assert_eq!(size, (320, 200));
}

/// 🚨 **`--play` takes seconds and refuses everything that is not a length of
/// time**, because the alternative is a loop whose end nobody can predict.
///
/// Fractions are allowed on purpose: a two-second run is the one an operator
/// uses to check that the picture moves at all, and `0.5` is a legitimate
/// answer to *did it draw anything*.
#[test]
fn play_takes_seconds_and_cell_takes_pixels() {
    let Command::Paint(Paint { play, cell, .. }) =
        parse("paint --play 2.5 --cell 24").expect("play").command
    else {
        panic!("not paint");
    };
    assert_eq!(play, Some(Duration::from_millis(2_500)));
    assert_eq!(cell, 24);

    for line in [
        "paint --play",
        "paint --play soon",
        "paint --play 0",
        "paint --play -3",
        "paint --play nan",
        "paint --cell 0",
        "paint --cell tall",
    ] {
        assert!(parse(line).is_err(), "{line} was accepted");
    }
}

/// A misspelled size says what the shape is, rather than only that it is wrong.
#[test]
fn a_size_that_is_not_wxh_is_refused_with_an_example() {
    let err = parse("paint --size 640").expect_err("should refuse");
    assert!(err.to_string().contains("640x360"), "{err}");

    let err = parse("paint --size widexhigh").expect_err("should refuse");
    assert!(err.to_string().contains("640x360"), "{err}");

    let err = parse("paint --px many").expect_err("should refuse");
    assert!(err.to_string().contains("--px"), "{err}");
}

/// 🚨 **F831: a stop that cannot be taken out of the path cannot be measured.**
/// `a2468` is the case the flag exists for — the reasoning ceiling ended the
/// first Change phase the quality tier ever reached, correctly by every signal
/// the loop had, and *whether that turn would have produced anything* became
/// unobservable, because the observation is the thing the stop prevents.
///
/// ⚠ **`0` is off and not a ceiling of zero.** `reasoning_chars` starts at 0 and
/// the check is `>=`, so a literal zero would end every turn before its first
/// delta — the opposite of what the operator typing it means.
#[test]
fn the_reasoning_ceiling_can_be_set_and_zero_takes_the_stop_out_of_the_path() {
    let parsed = parse("run --task t7 --reasoning-ceiling 12000").expect("run");
    let Command::Run(run) = parsed.command else {
        panic!("expected a run");
    };
    assert_eq!(run.reasoning_ceiling, Some(12_000));
    assert_eq!(
        abcc::run::limits_for(&run).reasoning_ceiling,
        12_000,
        "the flag did not reach the limits"
    );

    let parsed = parse("run --task t7 --reasoning-ceiling 0").expect("run");
    let Command::Run(run) = parsed.command else {
        panic!("expected a run");
    };
    assert_eq!(
        abcc::run::limits_for(&run).reasoning_ceiling,
        usize::MAX,
        "0 has to mean off; a ceiling of zero ends every turn before its first delta"
    );

    let parsed = parse("run --task t7").expect("run");
    let Command::Run(run) = parsed.command else {
        panic!("expected a run");
    };
    assert_eq!(run.reasoning_ceiling, None);
    assert_eq!(
        abcc::run::limits_for(&run).reasoning_ceiling,
        50_000,
        "a run that names no ceiling gets the measured one"
    );
}

/// Temperature 0 unless the run says `--temperature server`, and anything else
/// is refused with the spelling rather than guessed at.
#[test]
fn the_temperature_is_zero_unless_the_run_asks_for_the_server_s() {
    let limits = |line: &str| {
        let Command::Run(run) = parse(line).expect("run").command else {
            panic!("expected a run");
        };
        abcc::run::limits_for(&run).temperature
    };
    assert_eq!(limits("run --task t7"), Temperature::Zero);
    assert_eq!(limits("run --task t7 --temperature 0"), Temperature::Zero);
    assert_eq!(
        limits("run --task t7 --temperature server"),
        Temperature::Server
    );
    assert!(parse("run --task t7 --temperature 0.7").is_err());
}

/// Eviction is off unless asked for, on `run` and on `fleet` alike. The window
/// itself comes from the server at confirm time, never from the flag.
#[test]
fn eviction_is_off_unless_the_run_asks_for_it() {
    let evict = |line: &str| match parse(line).expect("parse").command {
        Command::Run(run) | Command::Fleet(run) => run.evict,
        _ => panic!("expected run or fleet"),
    };
    assert!(!evict("run --task t7"));
    assert!(evict("run --task t7 --evict"));
    assert!(evict("fleet --evict"));
    assert_eq!(
        abcc::run::limits_for(&cli::Run::default()).evict_window,
        None
    );
}

/// `chat` takes one ask for a new task, or `--task` for one on the board, and
/// every flag `run` takes.
#[test]
fn chat_takes_an_ask_or_a_task_and_the_run_flags() {
    let chat = |args: &[&str]| match parse_args(args).expect("parse").command {
        Command::Chat(chat) => chat,
        other => panic!("expected chat, got {other:?}"),
    };

    let asked = chat(&["chat", "add a flag for the port", "--title", "port flag"]);
    assert_eq!(asked.prompt.as_deref(), Some("add a flag for the port"));
    assert_eq!(asked.title.as_deref(), Some("port flag"));
    assert_eq!(asked.run.task, None);

    let picked = chat(&["chat", "--task", "t7", "--temperature", "server"]);
    assert_eq!(picked.prompt, None);
    assert_eq!(picked.run.task, Some(cli::TaskRef(7)));
    assert_eq!(picked.run.temperature, Some(Temperature::Server));

    assert_eq!(
        chat(&["chat"]).prompt,
        None,
        "a bare chat asks at the prompt"
    );
    assert!(parse_args(&["chat", "one", "two"]).is_err());
    assert!(parse_args(&["chat", "an ask", "--task", "t7"]).is_err());
}
