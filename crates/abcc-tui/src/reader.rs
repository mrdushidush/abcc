//! The reader: a position in the log, a selection on the board, and the six keys
//! that move them.
//!
//! 🚨 **Replay is the live view at a different position, and it is the same
//! code.** [`Reader::rewind`] throws the fold away and re-folds from the origin
//! through the same paged read the tail uses, which is why ADR-0012 can say the
//! after-action screen is nearly free — and why a reader positioned in the past
//! cannot drift from one positioned at the head. The fold is not invertible and
//! nothing pretends it is.
//!
//! ⚠ **This reader writes nothing.** Skeleton's console edge is read-only: the
//! control verbs (ADR-0012 §4) arrive with the binary, which owns the
//! `ControlHandle`. One consequence is worth naming rather than discovering:
//! `t` cycles the theme and **the switch is not recorded**, so W5 F129's
//! *"personality with an off switch"* instrument — off, and later back on — has
//! no data yet. It needs a writer, and it lands with one.

use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use abcc_core::seq::Seq;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind};

use crate::feed::Feed;
use crate::theme::Theme;
use crate::view::{Card, View};

/// How many events one read asks for, and how far one page key moves.
pub const PAGE: usize = 256;

/// What a key press asked the caller to do.
///
/// The reader does not hold the feed, so the two things that need one come back
/// as values. That keeps key handling a pure function of the key and the reader,
/// which is the half worth testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Nothing that needs the log.
    Nothing,
    /// Throw the fold away and re-fold from the origin up to here.
    Rewind(Seq),
    /// Read one more page from where the cursor is.
    Step,
    Quit,
}

/// A position in the log and what is selected there.
#[derive(Debug)]
pub struct Reader {
    view: View,
    theme: Theme,
    follow: bool,
    selected: usize,
    error: Option<String>,
    page: usize,
}

impl Reader {
    #[must_use]
    pub fn new(theme: Theme) -> Reader {
        Reader {
            view: View::new(theme),
            theme,
            follow: true,
            selected: 0,
            error: None,
            page: PAGE,
        }
    }

    #[must_use]
    pub fn view(&self) -> &View {
        &self.view
    }

    /// Whether the reader is tailing the log rather than sitting in the past.
    #[must_use]
    pub fn following(&self) -> bool {
        self.follow
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    #[must_use]
    pub fn card(&self) -> Option<&Card> {
        self.view.card_at(self.selected)
    }

    /// The last read that did not happen. Shown on screen rather than swallowed:
    /// a board that has quietly stopped being refreshed looks exactly like a
    /// board where nothing is happening.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Read the next page from the cursor and fold it. Returns how many landed.
    pub fn pump<F: Feed + ?Sized>(&mut self, feed: &F) -> usize {
        match feed.read_from(self.view.cursor(), self.page) {
            Ok(page) => {
                self.error = None;
                self.view.fold_all(&page);
                self.clamp();
                page.len()
            }
            Err(e) => {
                self.error = Some(e.to_string());
                0
            }
        }
    }

    /// Re-fold from the origin up to `to`, inclusive, and stop following.
    pub fn rewind<F: Feed + ?Sized>(&mut self, feed: &F, to: Seq) {
        self.view = View::new(self.theme);
        self.follow = false;
        let mut at = Seq::ORIGIN;
        'pages: while at < to {
            match feed.read_from(at, self.page) {
                Ok(page) if page.is_empty() => break,
                Ok(page) => {
                    for logged in &page {
                        if logged.seq > to {
                            break 'pages;
                        }
                        self.view.fold(logged);
                        at = logged.seq;
                    }
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    break;
                }
            }
        }
        self.clamp();
    }

    /// Apply a key. Pure: everything that needs the log comes back as an
    /// [`Intent`].
    pub fn on_key(&mut self, key: KeyEvent) -> Intent {
        // ⚠ Windows sends a key event on release as well as on press, so a reader
        // that did not filter would act on every keystroke twice — `j j` for one
        // press of `j`.
        if key.kind != KeyEventKind::Press {
            return Intent::Nothing;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Intent::Quit,
            KeyCode::Char('f' | ' ') => {
                self.follow = !self.follow;
                Intent::Nothing
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select(self.selected.saturating_add(1));
                Intent::Nothing
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select(self.selected.saturating_sub(1));
                Intent::Nothing
            }
            KeyCode::Char('g') | KeyCode::Home => Intent::Rewind(Seq::ORIGIN),
            KeyCode::Char('G') | KeyCode::End => {
                self.follow = true;
                Intent::Step
            }
            KeyCode::Char('[') | KeyCode::PageUp => {
                let back = i64::try_from(self.page).unwrap_or(i64::MAX);
                Intent::Rewind(Seq::new((self.view.cursor().get() - back).max(0)))
            }
            KeyCode::Char(']') | KeyCode::PageDown => Intent::Step,
            // Re-folding is how the theme changes, because a feed line is text by
            // the time it is held. It costs one replay of what is on screen, and
            // it exercises the path the after-action view uses.
            KeyCode::Char('t') => {
                self.theme = match self.theme {
                    Theme::Command => Theme::Classic,
                    Theme::Classic => Theme::Command,
                };
                Intent::Rewind(self.view.cursor())
            }
            _ => Intent::Nothing,
        }
    }

    /// Do what a key asked for. The one place the reader and the log meet after a
    /// key press.
    pub fn act<F: Feed + ?Sized>(&mut self, feed: &F, intent: Intent) -> Intent {
        match intent {
            Intent::Rewind(to) => self.rewind(feed, to),
            Intent::Step => {
                self.pump(feed);
            }
            Intent::Nothing | Intent::Quit => {}
        }
        intent
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        self.clamp();
    }

    fn clamp(&mut self) {
        let n = self.view.cards().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }
}

/// Unix milliseconds now. The reader's clocks are intervals against this, and it
/// is passed into the drawing rather than read inside it so a test can render a
/// stall without waiting for one.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// How often the loop wakes to redraw. The clocks on screen are intervals, so
/// they move even when the log does not — which is the point, because the thing
/// the operator most needs to see is a number that is *still going up*.
pub const TICK: Duration = Duration::from_millis(200);

/// Run the reader against a feed until the operator quits.
///
/// # Errors
///
/// Fails if the terminal cannot be put into, or taken out of, raw mode, or if a
/// draw fails.
pub fn run<F: Feed + ?Sized>(feed: &F, theme: Theme) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = pump_loop(&mut terminal, feed, theme);
    ratatui::try_restore()?;
    result
}

fn pump_loop<F: Feed + ?Sized>(
    terminal: &mut ratatui::DefaultTerminal,
    feed: &F,
    theme: Theme,
) -> io::Result<()> {
    let mut reader = Reader::new(theme);
    loop {
        if reader.following() {
            reader.pump(feed);
        }
        terminal.draw(|frame| crate::render::draw(frame, &reader, now_ms()))?;
        if event::poll(TICK)?
            && let TermEvent::Key(key) = event::read()?
        {
            let intent = reader.on_key(key);
            if reader.act(feed, intent) == Intent::Quit {
                return Ok(());
            }
        }
    }
}
