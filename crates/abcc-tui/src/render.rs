//! Drawing. Plain text, and deliberately so.
//!
//! ADR-0012 makes the sixel battlefield the flagship and puts it at the **Console**
//! milestone, on a read path already proven. This is that read path: the fallback
//! ladder's bottom rung, which every terminal answers, and the thing Console
//! extends rather than replaces. There are no sprites here and adding one is a
//! milestone, not a patch.
//!
//! What the screen is arranged around is the operator's question *"is anything
//! happening, and can I still do something about it?"* — so the silence clock is
//! in the header, the stall marker is on the board, and the state contract is in
//! the footer, where it says in words what the state promises about the slot and
//! the workspace it is holding.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line as TextLine;
use ratatui::widgets::{Block, List, ListItem, Paragraph};

use abcc_core::task::{BootAction, Watchdog};

use crate::line::minutes;
use crate::reader::Reader;
use crate::view::{Card, Pulse, View};

/// Draw the whole reader.
pub fn draw(frame: &mut Frame, reader: &Reader, now_ms: i64) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(7),
    ])
    .areas(frame.area());
    let [board, feed] =
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(body);

    frame.render_widget(Paragraph::new(header_line(reader, now_ms)), header);
    draw_board(frame, reader, board, now_ms);
    draw_feed(frame, reader, feed);
    frame.render_widget(footer_widget(reader, now_ms), footer);
}

fn header_line(reader: &Reader, now_ms: i64) -> TextLine<'static> {
    let view = reader.view();
    let theme = view.theme();
    let mut parts = vec!["ABCC".to_string()];
    parts.push(match view.mode() {
        Some(mode) => theme.mode(mode).to_string(),
        // No `RunStarted` in what has been read. Not "unknown mode" — the run's
        // mode is its first event, so its absence means the reader is positioned
        // before it or the run never wrote one.
        None => "mode not yet seen".to_string(),
    });
    if let Some(v) = view.version() {
        parts.push(format!("v{v}"));
    }
    if let Some(pid) = view.pid() {
        parts.push(format!("pid {pid}"));
    }
    parts.push(format!("seq {}", view.cursor()));
    parts.push(if reader.following() { "LIVE" } else { "REPLAY" }.to_string());
    parts.push(format!("{} events", view.events()));
    if let Some(ms) = view.silence_ms(now_ms) {
        parts.push(format!("quiet {}", human_ms(ms)));
    }
    // 🚨 `changes`, and the word is load-bearing: the ladder's unit is the
    // change and not the recording, so two passes over one change are one
    // change here. `Ladder` owns that rule for both readers of it.
    let ladder = view.ladder();
    parts.push(format!(
        "review {} / {}",
        minutes(ladder.total_seconds()),
        ladder.changes.len()
    ));
    TextLine::from(parts.join(" · ")).style(Style::new().add_modifier(Modifier::BOLD))
}

fn draw_board(frame: &mut Frame, reader: &Reader, area: Rect, now_ms: i64) {
    let view = reader.view();
    let cards = view.cards();
    let items: Vec<ListItem> = if cards.is_empty() {
        vec![ListItem::new("no tasks on the log yet")]
    } else {
        cards
            .iter()
            .enumerate()
            .map(|(i, card)| board_row(view, card, i == reader.selected(), now_ms))
            .collect()
    };
    let title = format!(" BOARD · {} tasks ", cards.len());
    frame.render_widget(List::new(items).block(Block::bordered().title(title)), area);
}

fn board_row(view: &View, card: &Card, selected: bool, now_ms: i64) -> ListItem<'static> {
    let pulse = view.pulse(card, now_ms);
    let mark = if pulse.stalled() { "!" } else { " " };
    let text = format!(
        "{mark}{:<5} {:<22} {}",
        card.id.to_string(),
        view.theme().state(&card.state),
        card.label()
    );
    let mut style = Style::new();
    if pulse.stalled() {
        style = style.fg(Color::Yellow);
    }
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    ListItem::new(TextLine::from(text).style(style))
}

fn draw_feed(frame: &mut Frame, reader: &Reader, area: Rect) {
    let view = reader.view();
    let from = view.first_at_ms().unwrap_or(0);
    // Two rows of the area are the border. The feed keeps far more than fits
    // (F112 bounds it at all), so the tail is what is shown.
    let room = usize::from(area.height).saturating_sub(2);
    let lines = view.feed();
    let skip = lines.len().saturating_sub(room);
    let items: Vec<ListItem> = if lines.is_empty() {
        vec![ListItem::new("nothing on the log at this position")]
    } else {
        lines
            .iter()
            .skip(skip)
            .map(|l| {
                ListItem::new(format!(
                    "{:>5}  {:>8}  {}",
                    l.seq.to_string(),
                    human_ms(l.at_ms - from),
                    l.text
                ))
            })
            .collect()
    };
    let title = format!(" FEED · {} lines held ", lines.len());
    frame.render_widget(List::new(items).block(Block::bordered().title(title)), area);
}

fn footer_widget(reader: &Reader, now_ms: i64) -> Paragraph<'static> {
    let view = reader.view();
    let mut lines: Vec<TextLine<'static>> = Vec::new();
    match view.card_at(reader.selected()) {
        None => lines.push(TextLine::from("nothing selected")),
        Some(card) => {
            lines.push(
                TextLine::from(format!("{} · {}", card.id, card.label()))
                    .style(Style::new().add_modifier(Modifier::BOLD)),
            );
            lines.push(TextLine::from(format!(
                "{} since seq {} · {} attempts · {}",
                view.theme().state(&card.state),
                card.since,
                card.attempts,
                pulse_text(view.pulse(card, now_ms))
            )));
            lines.push(TextLine::from(contract_text(card)));
            if let Some(question) = &card.question {
                lines.push(
                    TextLine::from(format!("? {question}")).style(Style::new().fg(Color::Cyan)),
                );
            }
        }
    }
    if let Some(error) = reader.error() {
        lines.push(
            TextLine::from(format!("log unreadable: {error}")).style(Style::new().fg(Color::Red)),
        );
    }
    lines.push(TextLine::from(
        "q quit · f follow · j/k select · g start · G live · [ ] page · t theme",
    ));
    Paragraph::new(lines).block(Block::bordered())
}

/// The state's own contract, in words. It is on the screen because every verb an
/// operator is about to use ends in *"and then what happens to its model slot and
/// its workspace lock?"* — and this is the row that answers it.
fn contract_text(card: &Card) -> String {
    let c = card.state.contract();
    let mut parts = Vec::new();
    parts.push(if c.holds_slot {
        "holds a slot"
    } else {
        "no slot"
    });
    parts.push(if c.holds_workspace {
        "holds the workspace"
    } else {
        "no workspace"
    });
    parts.push(match c.watchdog {
        Watchdog::NotWatched => "not watched",
        Watchdog::Reap { .. } => "reaped on its bound",
        Watchdog::WatchNeverReap => "watched, never reaped",
    });
    parts.push(match c.boot {
        BootAction::Stands => "survives a restart",
        BootAction::Requeue => "requeued by a restart",
        BootAction::RepresentPrompt => "re-asked after a restart",
    });
    if c.terminal {
        parts.push("terminal");
    }
    parts.join(" · ")
}

fn pulse_text(pulse: Pulse) -> String {
    match pulse {
        Pulse::Untimed => "no clock to read".to_string(),
        Pulse::Since { ms } => format!("in this state {}", human_ms(ms)),
        Pulse::Progress { ms, stalled: false } => format!("last progress {} ago", human_ms(ms)),
        Pulse::Progress { ms, stalled: true } => {
            format!("SILENT for {} — over the ten-second bar", human_ms(ms))
        }
        // Never a warning, however long it has been: the watchdog distinguishes
        // *no human yet* from *no progress*, and only the second one is a fault.
        Pulse::Waiting { ms } => format!("waiting on a human for {}", human_ms(ms)),
    }
}

/// Milliseconds an operator can read at a glance. No calendar and no dependency:
/// every clock on this screen is an interval, because `at_ms` is data and the
/// cursor is a `seq` (ADR-0012 §3).
#[must_use]
pub fn human_ms(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 10_000 {
        return format!("{}.{}s", ms / 1000, (ms % 1000) / 100);
    }
    if ms < 60_000 {
        return format!("{}s", ms / 1000);
    }
    format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
}
