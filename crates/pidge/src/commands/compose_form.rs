//! Full-screen TUI compose form — used by `mail new` and `drafts edit`
//! (later: `mail reply`, `mail reply-all`, `mail forward`).
//!
//! Replaces the inquire wizard's field-by-field prompt sequence with a single
//! ratatui form so users can navigate between fields, see the whole message
//! while editing, and add attachments without leaving the screen.
//!
//! ### Field layout
//!
//! ```text
//! ╭─ <context title> ─────────────────────────────╮
//! │ From:     [account ▾]                          │
//! │ To:       alice@example.com                    │
//! │ Cc:                                            │
//! │ Bcc:                                           │
//! │ Subject:  Q4 review                            │
//! │ Attach:   report.pdf, screenshot.png           │
//! ├────────────────────────────────────────────────┤
//! │ Body lives here. Tab moves between fields;    │
//! │ inside the body, Enter inserts a newline.     │
//! ╰────────────────────────────────────────────────╯
//!  Tab next · Shift-Tab prev · Ctrl-S send …
//! ```
//!
//! ### Key bindings
//!
//! Global (any focus):
//! - `Tab` / `Shift-Tab`: cycle focus
//! - `Ctrl-S`: validate and send (`Outcome::Send`)
//! - `Ctrl-D`: validate and save as draft (`Outcome::Draft`)
//! - `Ctrl-A`: enter add-attachment mode
//! - `Esc`: ask "discard?" — if confirmed, returns `Outcome::Cancel`
//!
//! Field-specific:
//! - From: `Left` / `Right` or `Space` cycle signed-in accounts
//! - Attach: `x` removes the last attachment
//! - Body: `Enter` inserts a newline; `Up`/`Down` move between body lines
//! - Single-line text fields (To/Cc/Bcc/Subject): `Enter` advances to the
//!   next field; arrows + Backspace inside the line behave like a regular
//!   text input
//!
//! Modals (AddingAttach, ConfirmingCancel) intercept all keys until dismissed.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use tui_textarea::{Input, Key, TextArea};

// --- public types ----------------------------------------------------------

/// What kind of compose the user is doing — drives the form's title and a
/// few sensible defaults (e.g. body-optional for replies, since Graph
/// supplies the auto-quoted text).
#[derive(Debug, Clone)]
#[allow(dead_code)] // Reply / Forward variants ship in a follow-up PR that
// routes those commands through the TUI form as well. Keeping the enum
// shape stable now avoids a breaking change to callers later.
pub enum Context {
    NewSend,
    Reply { subject: String, all: bool },
    Forward { subject: String },
    EditDraft,
}

/// What the user filled in. Lists are split from the form's single-line
/// strings on `,` / `;` here so the caller doesn't have to repeat the work.
#[derive(Debug, Clone, Default)]
pub struct Compose {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<PathBuf>,
}

/// What the form returned. `Send` and `Draft` carry the validated `Compose`;
/// `Cancel` means the user explicitly discarded.
#[derive(Debug, Clone)]
pub enum Outcome {
    Send(Compose),
    Draft(Compose),
    Cancel,
}

/// Run the TUI form to completion.
///
/// `accounts` is the list of e-mail addresses the user is signed in to —
/// used to populate the From dropdown. `initial` pre-fills every field
/// (use `Compose::default()` for a blank new e-mail).
pub fn run(initial: Compose, accounts: Vec<String>, context: Context) -> Result<Outcome> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, State::new(initial, accounts, context));
    ratatui::restore();
    result
}

// --- state -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    To,
    Cc,
    Bcc,
    Subject,
    Attach,
    Body,
}

const FIELD_ORDER: [Field; 7] = [
    Field::From,
    Field::To,
    Field::Cc,
    Field::Bcc,
    Field::Subject,
    Field::Attach,
    Field::Body,
];

#[derive(Clone, PartialEq, Eq)]
enum Mode {
    Edit,
    AddingAttach,
    ConfirmingCancel,
    /// Soft-warning overlay before sending — empty subject and/or body
    /// triggers this rather than hard-failing. Y/Enter sends anyway;
    /// N/Esc goes back to Edit with focus on `focus_on_cancel`.
    ConfirmingSend {
        warning: String,
        focus_on_cancel: Field,
    },
    ShowingError(String),
}

struct State<'a> {
    accounts: Vec<String>,
    from_idx: usize,
    to: TextArea<'a>,
    cc: TextArea<'a>,
    bcc: TextArea<'a>,
    subject: TextArea<'a>,
    body: TextArea<'a>,
    attachments: Vec<PathBuf>,
    /// Index of the currently-highlighted attachment when the Attach field
    /// is focused. Always < attachments.len(); clamped on remove and on
    /// add-modal exit. Meaningless when attachments is empty.
    attach_selected: usize,
    focus: Field,
    mode: Mode,
    attach_input: TextArea<'a>,
    context: Context,
}

impl<'a> State<'a> {
    fn new(initial: Compose, accounts: Vec<String>, context: Context) -> Self {
        let from_idx = accounts
            .iter()
            .position(|a| a == &initial.from)
            .unwrap_or(0);

        let mut s = Self {
            accounts,
            from_idx,
            to: single_line(initial.to.join(", ")),
            cc: single_line(initial.cc.join(", ")),
            bcc: single_line(initial.bcc.join(", ")),
            subject: single_line(initial.subject),
            body: multi_line(initial.body),
            attachments: initial.attachments,
            attach_selected: 0,
            focus: Field::To,
            mode: Mode::Edit,
            attach_input: single_line(String::new()),
            context,
        };
        // If we have no recipients yet (new send), keep focus on To so the
        // user can start typing immediately. Otherwise start in the body —
        // the recipients are already filled.
        if !s.to.is_empty() && !s.subject.is_empty() {
            s.focus = Field::Body;
        }
        s
    }

    /// True when every required field passes validation. On failure returns
    /// the field the user needs to fix and a user-facing message — the
    /// caller moves focus to that field and shows the message in a
    /// dismiss-on-any-key modal so the user stays inside the form.
    fn validate(&self) -> Result<Compose, (Field, String)> {
        let to = parse_addresses(&first_line(&self.to)).map_err(|e| (Field::To, e.to_string()))?;
        if to.is_empty() {
            return Err((Field::To, "At least one recipient is required.".to_string()));
        }
        let cc = parse_addresses(&first_line(&self.cc)).map_err(|e| (Field::Cc, e.to_string()))?;
        let bcc =
            parse_addresses(&first_line(&self.bcc)).map_err(|e| (Field::Bcc, e.to_string()))?;
        let from = self
            .accounts
            .get(self.from_idx)
            .cloned()
            .unwrap_or_default();
        Ok(Compose {
            from,
            to,
            cc,
            bcc,
            subject: first_line(&self.subject),
            body: self.body.lines().join("\n"),
            attachments: self.attachments.clone(),
        })
    }

    fn focus_next(&mut self) {
        let i = FIELD_ORDER.iter().position(|f| f == &self.focus).unwrap();
        self.focus = FIELD_ORDER[(i + 1) % FIELD_ORDER.len()];
    }

    fn focus_prev(&mut self) {
        let i = FIELD_ORDER.iter().position(|f| f == &self.focus).unwrap();
        self.focus = FIELD_ORDER[(i + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()];
    }

    fn cycle_from(&mut self, dir: i32) {
        if self.accounts.is_empty() {
            return;
        }
        let len = self.accounts.len() as i32;
        let new = (self.from_idx as i32 + dir).rem_euclid(len);
        self.from_idx = new as usize;
    }

    /// Set every TextArea's cursor style so only the currently-focused field
    /// shows a visible block cursor. Non-focused fields get a no-op style
    /// (default Style with no REVERSED modifier), which makes their cursors
    /// effectively invisible. When the AddingAttach modal is up, the modal's
    /// input gets the visible cursor and all main fields hide.
    fn apply_cursor_styles(&mut self) {
        let visible = Style::default().add_modifier(Modifier::REVERSED);
        let hidden = Style::default();

        let in_modal = matches!(self.mode, Mode::AddingAttach);
        self.attach_input
            .set_cursor_style(if in_modal { visible } else { hidden });

        let pick = |target: Field| {
            if !in_modal && self.focus == target {
                visible
            } else {
                hidden
            }
        };
        // We can't use the closure across set_cursor_style calls (each call
        // re-borrows self mutably), so resolve each style up-front.
        let to_s = pick(Field::To);
        let cc_s = pick(Field::Cc);
        let bcc_s = pick(Field::Bcc);
        let subj_s = pick(Field::Subject);
        let body_s = pick(Field::Body);
        self.to.set_cursor_style(to_s);
        self.cc.set_cursor_style(cc_s);
        self.bcc.set_cursor_style(bcc_s);
        self.subject.set_cursor_style(subj_s);
        self.body.set_cursor_style(body_s);
    }
}

fn single_line(initial: String) -> TextArea<'static> {
    let mut t = if initial.is_empty() {
        TextArea::default()
    } else {
        TextArea::new(vec![initial])
    };
    // Hide the textarea's own block; we draw our own labels and outline.
    t.set_cursor_line_style(Style::default());
    t
}

fn multi_line(initial: String) -> TextArea<'static> {
    let lines: Vec<String> = if initial.is_empty() {
        vec![String::new()]
    } else {
        initial.split('\n').map(String::from).collect()
    };
    let mut t = TextArea::new(lines);
    t.set_cursor_line_style(Style::default());
    t
}

fn first_line(t: &TextArea) -> String {
    t.lines().first().cloned().unwrap_or_default()
}

/// Split a "a@b, c@d; e@f" string into individual addresses, validating each
/// loosely (must contain `@`). Empty input returns an empty Vec without
/// erroring — the caller decides whether that's acceptable.
pub fn parse_addresses(raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for s in raw.split([',', ';']) {
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        if !s.contains('@') {
            anyhow::bail!("'{s}' doesn't look like an e-mail address");
        }
        out.push(s.to_string());
    }
    Ok(out)
}

// --- main loop -------------------------------------------------------------

fn run_loop(terminal: &mut DefaultTerminal, mut state: State) -> Result<Outcome> {
    loop {
        state.apply_cursor_styles();
        terminal
            .draw(|frame| draw(frame, &state))
            .context("terminal draw failed")?;
        let ev = event::read().context("event read failed")?;
        if let Some(outcome) = handle_event(&mut state, ev)? {
            return Ok(outcome);
        }
    }
}

fn handle_event(state: &mut State, ev: Event) -> Result<Option<Outcome>> {
    let key = match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => k,
        _ => return Ok(None),
    };

    // Modals take precedence over edit-mode handlers.
    match state.mode.clone() {
        Mode::ConfirmingCancel => return Ok(handle_confirm_cancel(state, key)),
        Mode::ConfirmingSend {
            focus_on_cancel, ..
        } => return Ok(handle_confirm_send(state, key, focus_on_cancel)),
        Mode::AddingAttach => {
            handle_adding_attach(state, key);
            return Ok(None);
        }
        Mode::ShowingError(_) => {
            // Any key dismisses the error overlay.
            state.mode = Mode::Edit;
            return Ok(None);
        }
        Mode::Edit => {}
    }

    // Global shortcuts.
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            state.mode = Mode::ConfirmingCancel;
            return Ok(None);
        }
        (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
            // Sending warns on empty subject/body so the user has a chance
            // to think twice before launching an unintended blank e-mail.
            return Ok(try_submit(
                state,
                Outcome::Send,
                /*warn_on_empty*/ true,
            ));
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            // Drafts are explicitly for incomplete work — no nags.
            return Ok(try_submit(
                state,
                Outcome::Draft,
                /*warn_on_empty*/ false,
            ));
        }
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
            state.attach_input = single_line(String::new());
            state.mode = Mode::AddingAttach;
            return Ok(None);
        }
        (KeyCode::Tab, _) => {
            state.focus_next();
            return Ok(None);
        }
        (KeyCode::BackTab, _) => {
            state.focus_prev();
            return Ok(None);
        }
        _ => {}
    }

    // Field-specific dispatch. Single-line fields treat Enter as "advance
    // to next field"; the body treats it as a newline. From and Attach
    // have their own non-text handlers.
    match state.focus {
        Field::From => handle_from(state, key),
        Field::To => handle_single_line(&mut state.to, key, &mut state.focus),
        Field::Cc => handle_single_line(&mut state.cc, key, &mut state.focus),
        Field::Bcc => handle_single_line(&mut state.bcc, key, &mut state.focus),
        Field::Subject => handle_single_line(&mut state.subject, key, &mut state.focus),
        Field::Attach => handle_attach(state, key),
        Field::Body => {
            let _ = state.body.input(crossterm_to_input(key));
        }
    }
    Ok(None)
}

fn handle_single_line(ta: &mut TextArea, key: KeyEvent, focus: &mut Field) {
    if matches!(key.code, KeyCode::Enter) {
        let i = FIELD_ORDER.iter().position(|f| f == focus).unwrap();
        *focus = FIELD_ORDER[(i + 1) % FIELD_ORDER.len()];
        return;
    }
    let _ = ta.input(crossterm_to_input(key));
}

/// Attempt to wrap up the form via `make_outcome` (either Send or Draft).
/// On hard validation failure we stay inside the form: focus moves to the
/// offending field and a "Please fix" modal pops up. On a soft warning
/// (empty subject/body, only when `warn_on_empty` is set), a "Send anyway?"
/// modal asks for explicit confirmation. Only a clean pass returns an
/// actual outcome to the run loop.
fn try_submit(
    state: &mut State,
    make_outcome: fn(Compose) -> Outcome,
    warn_on_empty: bool,
) -> Option<Outcome> {
    match state.validate() {
        Ok(compose) => {
            if warn_on_empty {
                if let Some((warning, focus_on_cancel)) = soft_warnings(&compose) {
                    state.mode = Mode::ConfirmingSend {
                        warning,
                        focus_on_cancel,
                    };
                    return None;
                }
            }
            Some(make_outcome(compose))
        }
        Err((field, msg)) => {
            state.focus = field;
            state.mode = Mode::ShowingError(msg);
            None
        }
    }
}

/// Return a user-facing warning string + the field the user should land on
/// if they cancel the send. None means the message is safe to send as-is.
fn soft_warnings(c: &Compose) -> Option<(String, Field)> {
    let subj = c.subject.trim().is_empty();
    let body = c.body.trim().is_empty();
    match (subj, body) {
        (true, true) => Some((
            "Subject and body are both empty.".to_string(),
            Field::Subject,
        )),
        (true, false) => Some(("Subject is empty.".to_string(), Field::Subject)),
        (false, true) => Some(("Body is empty.".to_string(), Field::Body)),
        (false, false) => None,
    }
}

fn handle_confirm_cancel(state: &mut State, key: KeyEvent) -> Option<Outcome> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(Outcome::Cancel),
        _ => {
            state.mode = Mode::Edit;
            None
        }
    }
}

/// Y/Enter confirms the send despite an empty subject/body; anything else
/// dismisses the warning and drops focus on the field the user can fix.
fn handle_confirm_send(
    state: &mut State,
    key: KeyEvent,
    focus_on_cancel: Field,
) -> Option<Outcome> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            // Re-validate. The form is unchanged since the warning was
            // raised, so this should succeed; if for some reason it
            // doesn't, fall through to the regular error path.
            match state.validate() {
                Ok(compose) => Some(Outcome::Send(compose)),
                Err((field, msg)) => {
                    state.focus = field;
                    state.mode = Mode::ShowingError(msg);
                    None
                }
            }
        }
        _ => {
            state.focus = focus_on_cancel;
            state.mode = Mode::Edit;
            None
        }
    }
}

fn handle_adding_attach(state: &mut State, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => state.mode = Mode::Edit,
        KeyCode::Enter => {
            let raw = first_line(&state.attach_input);
            let path = PathBuf::from(raw.trim());
            if !path.as_os_str().is_empty() {
                if path.is_file() {
                    state.attachments.push(path);
                    // Highlight the newly-added file when the user lands
                    // back on the Attach row.
                    state.attach_selected = state.attachments.len() - 1;
                } else {
                    state.mode = Mode::ShowingError(format!("Not a file: {}", path.display()));
                    return;
                }
            }
            state.mode = Mode::Edit;
        }
        _ => {
            let _ = state.attach_input.input(crossterm_to_input(key));
        }
    }
}

fn handle_from(state: &mut State, key: KeyEvent) {
    match key.code {
        KeyCode::Left | KeyCode::Up => state.cycle_from(-1),
        KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => state.cycle_from(1),
        KeyCode::Enter => state.focus_next(),
        _ => {}
    }
}

fn handle_attach(state: &mut State, key: KeyEvent) {
    match key.code {
        // Add: 'a' or Enter opens the path-input modal.
        KeyCode::Char('a') | KeyCode::Enter => {
            state.attach_input = single_line(String::new());
            state.mode = Mode::AddingAttach;
        }
        // Remove the currently-highlighted attachment, then clamp selection.
        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace
            if !state.attachments.is_empty() =>
        {
            let idx = state.attach_selected.min(state.attachments.len() - 1);
            state.attachments.remove(idx);
            if state.attach_selected >= state.attachments.len() {
                state.attach_selected = state.attachments.len().saturating_sub(1);
            }
        }
        // ←/→ navigate within the attachment list. Don't escape the field on
        // boundaries (Tab does that).
        KeyCode::Left if state.attach_selected > 0 => {
            state.attach_selected -= 1;
        }
        KeyCode::Right if state.attach_selected + 1 < state.attachments.len() => {
            state.attach_selected += 1;
        }
        _ => {}
    }
}

fn crossterm_to_input(key: KeyEvent) -> Input {
    Input {
        key: match key.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Enter => Key::Enter,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Tab => Key::Tab,
            KeyCode::Esc => Key::Esc,
            _ => Key::Null,
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

// --- rendering -------------------------------------------------------------

fn draw(frame: &mut Frame, state: &State) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", context_title(&state.context)).bold());
    frame.render_widget(&outer, area);
    let inner = outer.inner(area);

    // Vertical layout: 6 header rows, 1 separator, body, 1 footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // From
            Constraint::Length(1), // To
            Constraint::Length(1), // Cc
            Constraint::Length(1), // Bcc
            Constraint::Length(1), // Subject
            Constraint::Length(1), // Attach
            Constraint::Length(1), // separator
            Constraint::Min(3),    // Body
            Constraint::Length(1), // Footer
        ])
        .split(inner);

    draw_from_row(frame, chunks[0], state);
    draw_text_row(frame, chunks[1], "To:", &state.to, state.focus == Field::To);
    draw_text_row(frame, chunks[2], "Cc:", &state.cc, state.focus == Field::Cc);
    draw_text_row(
        frame,
        chunks[3],
        "Bcc:",
        &state.bcc,
        state.focus == Field::Bcc,
    );
    draw_text_row(
        frame,
        chunks[4],
        "Subject:",
        &state.subject,
        state.focus == Field::Subject,
    );
    draw_attach_row(frame, chunks[5], state);
    frame.render_widget(
        Paragraph::new("─".repeat(inner.width as usize))
            .style(Style::default().fg(Color::DarkGray)),
        chunks[6],
    );
    draw_body(frame, chunks[7], state);
    draw_footer(frame, chunks[8], state);

    // Modal overlays.
    match &state.mode {
        Mode::AddingAttach => draw_attach_modal(frame, area, state),
        Mode::ConfirmingCancel => draw_cancel_modal(frame, area),
        Mode::ConfirmingSend { warning, .. } => draw_confirm_send_modal(frame, area, warning),
        Mode::ShowingError(msg) => draw_error_modal(frame, area, msg),
        Mode::Edit => {}
    }
}

fn context_title(ctx: &Context) -> String {
    match ctx {
        Context::NewSend => "New e-mail".to_string(),
        Context::Reply { subject, all } => {
            format!("{}: {}", if *all { "Reply-all" } else { "Reply" }, subject)
        }
        Context::Forward { subject } => format!("Forward: {subject}"),
        Context::EditDraft => "Edit draft".to_string(),
    }
}

const LABEL_WIDTH: usize = 10;

fn draw_from_row(frame: &mut Frame, area: Rect, state: &State) {
    let focused = state.focus == Field::From;
    let account = state
        .accounts
        .get(state.from_idx)
        .cloned()
        .unwrap_or_else(|| "(no accounts)".to_string());
    let value = if state.accounts.len() > 1 {
        format!("{account}  ◂ ▸")
    } else {
        account
    };
    let line = Line::from(vec![label_span("From:", focused), Span::raw(value)]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_text_row(frame: &mut Frame, area: Rect, label: &str, ta: &TextArea, focused: bool) {
    // Reserve the first LABEL_WIDTH columns for the label, render the
    // textarea in the rest.
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(LABEL_WIDTH as u16), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![label_span(label, focused)])),
        columns[0],
    );
    frame.render_widget(ta, columns[1]);
}

fn draw_attach_row(frame: &mut Frame, area: Rect, state: &State) {
    let focused = state.focus == Field::Attach;
    let mut spans: Vec<Span> = vec![label_span("Attach:", focused)];

    if state.attachments.is_empty() {
        let hint = if focused {
            "(none — press 'a' or Enter to add)"
        } else {
            "(none — Ctrl-A to add)"
        };
        spans.push(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray).italic(),
        ));
    } else {
        // Render each filename. When the field is focused, highlight the
        // selected one with reverse-video so it's obvious which one a
        // remove ('x') or arrow keypress will act on.
        for (i, p) in state.attachments.iter().enumerate() {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            let style = if focused && i == state.attach_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            spans.push(Span::styled(name, style));
            if i + 1 < state.attachments.len() {
                spans.push(Span::raw("  "));
            }
        }
        if focused {
            spans.push(Span::styled(
                "   · ←→ select · x remove · a add",
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(frame: &mut Frame, area: Rect, state: &State) {
    let focused = state.focus == Field::Body;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(if focused {
            Style::default().fg(Color::LightCyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(label_span("Body", focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(&state.body, inner);
}

fn draw_footer(frame: &mut Frame, area: Rect, _state: &State) {
    let help = Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::LightYellow)),
        Span::raw(" next · "),
        Span::styled("Shift-Tab", Style::default().fg(Color::LightYellow)),
        Span::raw(" prev · "),
        Span::styled("Ctrl-S", Style::default().fg(Color::LightGreen)),
        Span::raw(" send · "),
        Span::styled("Ctrl-D", Style::default().fg(Color::LightCyan)),
        Span::raw(" draft · "),
        Span::styled("Ctrl-A", Style::default().fg(Color::LightMagenta)),
        Span::raw(" attach · "),
        Span::styled("Esc", Style::default().fg(Color::LightRed)),
        Span::raw(" cancel"),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::Gray)),
        area,
    );
}

fn draw_attach_modal(frame: &mut Frame, area: Rect, state: &State) {
    let modal = centered_rect(60, 5, area);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightMagenta))
        .title(" Add attachment ".bold());
    let inner = block.inner(modal);
    frame.render_widget(block, modal);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(Paragraph::new("Path:".dim()), rows[0]);
    frame.render_widget(&state.attach_input, rows[1]);
    frame.render_widget(
        Paragraph::new(
            Line::from(vec![
                Span::styled("Enter", Style::default().fg(Color::LightGreen)),
                Span::raw(" add · "),
                Span::styled("Esc", Style::default().fg(Color::LightRed)),
                Span::raw(" cancel"),
            ])
            .style(Style::default().fg(Color::Gray)),
        ),
        rows[2],
    );
}

fn draw_cancel_modal(frame: &mut Frame, area: Rect) {
    let modal = centered_rect(50, 5, area);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightRed))
        .title(" Discard ".bold());
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    let body = Paragraph::new(vec![
        Line::from("Discard this message?"),
        Line::from(""),
        Line::from(vec![
            Span::styled("Y/Enter", Style::default().fg(Color::LightRed)),
            Span::raw(" discard · "),
            Span::styled("N/Esc", Style::default().fg(Color::LightGreen)),
            Span::raw(" keep editing"),
        ])
        .style(Style::default().fg(Color::Gray)),
    ])
    .wrap(Wrap { trim: false });
    frame.render_widget(body, inner.inner(Margin::new(1, 0)));
}

fn draw_confirm_send_modal(frame: &mut Frame, area: Rect, warning: &str) {
    let modal = centered_rect(60, 6, area);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightYellow))
        .title(" Send anyway? ".bold());
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    let body = Paragraph::new(vec![
        Line::from(warning.to_string()),
        Line::from(""),
        Line::from(vec![
            Span::styled("Y/Enter", Style::default().fg(Color::LightGreen)),
            Span::raw(" send · "),
            Span::styled("N/Esc", Style::default().fg(Color::LightYellow)),
            Span::raw(" go back"),
        ])
        .style(Style::default().fg(Color::Gray)),
    ])
    .wrap(Wrap { trim: false });
    frame.render_widget(body, inner.inner(Margin::new(1, 0)));
}

fn draw_error_modal(frame: &mut Frame, area: Rect, msg: &str) {
    let modal = centered_rect(60, 6, area);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightRed))
        .title(" Please fix ".bold());
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(msg.to_string()),
            Line::from(""),
            Line::from("Press any key to continue.".dim()),
        ])
        .wrap(Wrap { trim: false }),
        inner.inner(Margin::new(1, 0)),
    );
}

fn label_span(text: &str, focused: bool) -> Span<'_> {
    if focused {
        Span::styled(
            format!("{text:<LABEL_WIDTH$}"),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
    } else {
        Span::styled(
            format!("{text:<LABEL_WIDTH$}"),
            Style::default().fg(Color::LightCyan).bold(),
        )
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

// --- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_addresses_splits_on_comma_and_semicolon() {
        let v = parse_addresses("alice@example.com, bob@example.com; carol@example.com").unwrap();
        assert_eq!(
            v,
            ["alice@example.com", "bob@example.com", "carol@example.com"]
        );
    }

    #[test]
    fn parse_addresses_skips_empty_segments() {
        let v = parse_addresses("alice@example.com,,bob@example.com").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn parse_addresses_rejects_missing_at_sign() {
        assert!(parse_addresses("alice").is_err());
        assert!(parse_addresses("alice@example.com, not-an-email").is_err());
    }

    #[test]
    fn parse_addresses_empty_input_returns_empty_vec() {
        assert!(parse_addresses("").unwrap().is_empty());
        assert!(parse_addresses(", ; ").unwrap().is_empty());
    }

    #[test]
    fn state_focus_next_wraps_around() {
        let mut s = State::new(Compose::default(), vec!["a@b".into()], Context::NewSend);
        s.focus = Field::To;
        s.focus_next();
        assert!(matches!(s.focus, Field::Cc));
        s.focus_next();
        assert!(matches!(s.focus, Field::Bcc));
        s.focus_next();
        assert!(matches!(s.focus, Field::Subject));
        s.focus_next();
        assert!(matches!(s.focus, Field::Attach));
        s.focus_next();
        assert!(matches!(s.focus, Field::Body));
        s.focus_next();
        assert!(matches!(s.focus, Field::From));
    }

    #[test]
    fn cycle_from_wraps_in_both_directions() {
        let mut s = State::new(
            Compose::default(),
            vec!["a@b".into(), "c@d".into(), "e@f".into()],
            Context::NewSend,
        );
        s.from_idx = 0;
        s.cycle_from(1);
        assert_eq!(s.from_idx, 1);
        s.cycle_from(-1);
        assert_eq!(s.from_idx, 0);
        s.cycle_from(-1);
        assert_eq!(s.from_idx, 2);
    }
}
