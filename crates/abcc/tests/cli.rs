//! The argument surface, which is a pure function and is tested like one.
//!
//! Every shape here is one somebody will type. The parse is hand-rolled and has
//! no dependency behind it, so the guard against a flag that silently does
//! nothing is this file rather than a library's reputation.

use std::time::Duration;

use abcc::cli::{self, CliError, Command};
use abcc_engine::Tier;
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
    for verb in [
        "where", "task", "board", "fun", "run", "watch", "check", "review", "accept", "reject",
    ] {
        // The verb is known to the parser: whatever else it complains about, it
        // never complains that this is not a command.
        let complaint = match parse(verb) {
            Ok(_) | Err(CliError::Help) => String::new(),
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

/// `paint` defaults, and the two flags that can be spelled wrong.
///
/// ⚠ The sprite directory has **no default here** and is not required here
/// either: the parse records what the operator said and `paint` resolves the
/// environment. A parser that read `ABCC_SPRITES` would make this test depend on
/// the machine it runs on.
#[test]
fn paint_defaults_to_a_hundred_and_fifty_pixel_sprite_on_a_640_by_360_field() {
    let Command::Paint {
        sprites,
        px,
        size,
        corpus,
    } = parse("paint").expect("paint").command
    else {
        panic!("not paint");
    };
    assert_eq!(sprites, None);
    assert!(
        !corpus,
        "the default is the fleet; the art is behind --corpus"
    );
    assert_eq!(
        px, 150,
        "150 is David's judgement over the art that is actually drawn (F565),          which is not the art F143's 75-120 band was judged on"
    );
    assert_eq!(size, (640, 360));

    let Command::Paint {
        sprites,
        px,
        size,
        corpus,
    } = parse("paint --sprites D:/art --px 75 --size 320x200 --corpus")
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
