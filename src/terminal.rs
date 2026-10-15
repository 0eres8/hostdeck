//! An egui terminal widget on top of `alacritty_terminal`, running a program in a PTY.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use eframe::egui::{
    self, Color32, CursorIcon, EventFilter, FontFamily, FontId, Key, Modifiers, PointerButton, Pos2,
    Rect, Sense, Stroke, Vec2, ViewportCommand,
};
use regex::Regex;

static PASSWORD_PROMPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(password|passphrase)[^:]*:\s*$").unwrap());

pub const MONO: &str = "mono";
pub const MONO_BOLD: &str = "mono-bold";

#[derive(Clone)]
pub struct TermTheme {
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

impl TermTheme {
    pub fn dark() -> Self {
        Self {
            fg: Color32::from_rgb(0xd4, 0xd8, 0xe1),
            bg: Color32::from_rgb(0x12, 0x15, 0x1c),
            cursor: Color32::from_rgb(0x7a, 0xa2, 0xf7),
            selection: Color32::from_rgb(0x2f, 0x3d, 0x5c),
            ansi: [
                hex(0x1d2129), hex(0xf7768e), hex(0x9ece6a), hex(0xe0af68),
                hex(0x7aa2f7), hex(0xbb9af7), hex(0x7dcfff), hex(0xa9b1d6),
                hex(0x4b5263), hex(0xff8ea3), hex(0xb9f27c), hex(0xffc777),
                hex(0x8db0ff), hex(0xcaa9fa), hex(0x9de6ff), hex(0xe6e9ef),
            ],
        }
    }

    pub fn light() -> Self {
        Self {
            fg: Color32::from_rgb(0x2b, 0x30, 0x3b),
            bg: Color32::from_rgb(0xfb, 0xfb, 0xfd),
            cursor: Color32::from_rgb(0x2e, 0x5b, 0xe8),
            selection: Color32::from_rgb(0xc9, 0xd7, 0xfb),
            ansi: [
                hex(0x2b303b), hex(0xc8364b), hex(0x3d8a2e), hex(0x9a6b00),
                hex(0x2e5be8), hex(0x8a3fd1), hex(0x0f7fa3), hex(0xb7bcc7),
                hex(0x6b7280), hex(0xe0455c), hex(0x4fa33c), hex(0xb88400),
                hex(0x4a74f0), hex(0xa25ee0), hex(0x1a98c2), hex(0x8b919d),
            ],
        }
    }

    fn named(&self, n: NamedColor) -> Color32 {
        let i = n as usize;
        match n {
            NamedColor::Foreground | NamedColor::BrightForeground => self.fg,
            NamedColor::Background => self.bg,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => dim(self.fg),
            _ if i < 16 => self.ansi[i],
            // Dim variants follow the foreground/background entries: DimBlack = 259.
            _ if (259..267).contains(&i) => dim(self.ansi[i - 259]),
            _ => self.fg,
        }
    }

    fn indexed(&self, i: u8) -> Color32 {
        let i = i as usize;
        if i < 16 {
            return self.ansi[i];
        }
        if i < 232 {
            let i = i - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            return Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6));
        }
        let v = (8 + (i - 232) * 10) as u8;
        Color32::from_rgb(v, v, v)
    }
}

fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn dim(c: Color32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * 0.66) as u8,
        (c.g() as f32 * 0.66) as u8,
        (c.b() as f32 * 0.66) as u8,
    )
}

#[derive(Clone)]
struct Listener {
    tx: Sender<TermEvent>,
    ctx: egui::Context,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq)]
struct GridSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct AutoLogin {
    password: String,
    deadline: Instant,
}

pub struct TerminalOptions {
    pub font_size: f32,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
}

#[derive(Default)]
pub struct TerminalOutput {
    pub reconnect: bool,
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    events: Receiver<TermEvent>,
    size: GridSize,
    cell: Vec2,
    pub title: Option<String>,
    pub exit_code: Option<i32>,
    autologin: Option<AutoLogin>,
    scroll_accum: f32,
    bell_until: Option<Instant>,
    /// Take keyboard focus on the next frame, e.g. after opening or switching to the tab.
    pub focus_requested: bool,
}

impl Terminal {
    pub fn spawn(
        program: &str,
        args: Vec<String>,
        ctx: &egui::Context,
        scrollback: usize,
        password: Option<String>,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let listener = Listener { tx, ctx: ctx.clone() };
        let size = GridSize { cols: 100, lines: 30 };
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let env = HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ]);
        let options = tty::Options {
            shell: Some(tty::Shell::new(program.to_string(), args)),
            working_directory: dirs::home_dir(),
            drain_on_exit: true,
            env,
        };
        let window = WindowSize { num_lines: 30, num_cols: 100, cell_width: 8, cell_height: 16 };
        let pty = tty::new(&options, window, 0)?;
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self {
            term,
            sender,
            events: rx,
            size,
            cell: Vec2::new(8.0, 16.0),
            title: None,
            exit_code: None,
            autologin: password.filter(|p| !p.is_empty()).map(|password| AutoLogin {
                password,
                deadline: Instant::now() + Duration::from_secs(120),
            }),
            scroll_accum: 0.0,
            bell_until: None,
            focus_requested: true,
        })
    }

    pub fn is_running(&self) -> bool {
        self.exit_code.is_none()
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    fn write_str(&self, s: &str) {
        self.write(s.as_bytes().to_vec());
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.lines as u16,
            num_cols: self.size.cols as u16,
            cell_width: self.cell.x as u16,
            cell_height: self.cell.y as u16,
        }
    }

    fn pump(&mut self, ctx: &egui::Context, theme: &TermTheme) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::Title(t) => self.title = Some(t),
                TermEvent::ResetTitle => self.title = None,
                TermEvent::PtyWrite(s) => self.write_str(&s),
                TermEvent::ClipboardStore(_, s) => ctx.copy_text(s),
                TermEvent::ColorRequest(i, format) => {
                    let c = match i {
                        256 => theme.fg,
                        257 => theme.bg,
                        258 => theme.cursor,
                        i if i < 256 => theme.indexed(i as u8),
                        _ => theme.fg,
                    };
                    self.write_str(&format(Rgb { r: c.r(), g: c.g(), b: c.b() }));
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let reply = format(self.window_size());
                    self.write_str(&reply);
                }
                TermEvent::ChildExit(status) => {
                    self.exit_code = Some(status.code().unwrap_or(-1));
                    self.autologin = None;
                }
                TermEvent::Exit => {
                    if self.exit_code.is_none() {
                        self.exit_code = Some(0);
                    }
                }
                TermEvent::Bell => self.bell_until = Some(Instant::now() + Duration::from_millis(150)),
                _ => {}
            }
        }
        self.try_autologin();
    }

    /// Type a saved password once, at the first password prompt shortly after connecting.
    fn try_autologin(&mut self) {
        let Some(login) = &self.autologin else { return };
        if Instant::now() > login.deadline {
            self.autologin = None;
            return;
        }
        let line = {
            let term = self.term.lock();
            let grid = term.grid();
            let point = grid.cursor.point;
            let row = &grid[point.line];
            (0..grid.columns()).map(|c| row[Column(c)].c).collect::<String>()
        };
        if PASSWORD_PROMPT.is_match(line.trim_end()) {
            let password = format!("{}\r", login.password);
            self.autologin = None;
            self.write(password.into_bytes());
        }
    }

    fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(self.window_size()));
    }

    fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            self.write_str(&format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")));
        } else {
            self.write_str(&text);
        }
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    fn copy_selection(&self, ctx: &egui::Context) {
        if let Some(text) = self.term.lock().selection_to_string()
            && !text.is_empty() {
                ctx.copy_text(text);
            }
    }

    fn point_at(&self, rect: Rect, pos: Pos2) -> (Point, Side) {
        let term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let x = ((pos.x - rect.left()) / self.cell.x).max(0.0);
        let y = ((pos.y - rect.top()) / self.cell.y).max(0.0);
        let col = (x as usize).min(self.size.cols.saturating_sub(1));
        let line = (y as i32).min(self.size.lines as i32 - 1) - offset;
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (Point::new(Line(line), Column(col)), side)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &TermTheme, opts: &TerminalOptions) -> TerminalOutput {
        let mut out = TerminalOutput::default();
        let ctx = ui.ctx().clone();
        self.pump(&ctx, theme);

        let outer = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(outer, Sense::click_and_drag());
        let rect = outer.shrink2(Vec2::new(10.0, 8.0));

        let font = FontId::new(opts.font_size, FontFamily::Name(MONO.into()));
        let bold = FontId::new(opts.font_size, FontFamily::Name(MONO_BOLD.into()));
        let (cw, ch) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        self.cell = Vec2::new(cw, ch.ceil());
        self.resize(GridSize {
            cols: ((rect.width() / self.cell.x) as usize).max(2),
            lines: ((rect.height() / self.cell.y) as usize).max(1),
        });

        if self.focus_requested || response.clicked() || response.drag_started() || response.secondary_clicked() {
            response.request_focus();
        }
        // The click that opened this tab may still be releasing elsewhere and steal focus
        // back, so keep asking until we hold focus with the mouse idle.
        if self.focus_requested && response.has_focus() && !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
            self.focus_requested = false;
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
            self.handle_keyboard(ui, &mut out);
        }
        if response.hovered() {
            ctx.set_cursor_icon(CursorIcon::Text);
        }
        self.handle_mouse(ui, &response, rect, opts);

        self.paint(ui, outer, rect, theme, &font, &bold, focused);
        out
    }

    fn handle_keyboard(&mut self, ui: &egui::Ui, out: &mut TerminalOutput) {
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        let mode = *self.term.lock().mode();
        for event in events {
            match event {
                egui::Event::Text(text) if self.is_running() => {
                    let text = if modifiers.alt { format!("\x1b{text}") } else { text };
                    self.write_str(&text);
                    self.term.lock().scroll_display(Scroll::Bottom);
                }
                egui::Event::Copy => {
                    if modifiers.shift {
                        self.copy_selection(ui.ctx());
                    } else if self.is_running() {
                        self.write(&b"\x03"[..]);
                    }
                }
                egui::Event::Cut if self.is_running() => self.write(&b"\x18"[..]),
                egui::Event::Paste(text) if self.is_running() => self.paste(&text),
                egui::Event::Key { key, pressed: true, modifiers: m, .. } => {
                    if !self.is_running() {
                        if key == Key::R {
                            out.reconnect = true;
                        }
                        continue;
                    }
                    if m.shift && !m.ctrl {
                        let scroll = match key {
                            Key::PageUp => Some(Scroll::PageUp),
                            Key::PageDown => Some(Scroll::PageDown),
                            Key::Home => Some(Scroll::Top),
                            Key::End => Some(Scroll::Bottom),
                            _ => None,
                        };
                        if let Some(s) = scroll {
                            self.term.lock().scroll_display(s);
                            continue;
                        }
                    }
                    if let Some(bytes) = key_bytes(key, m, mode) {
                        self.write(bytes);
                        self.term.lock().scroll_display(Scroll::Bottom);
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_mouse(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect, opts: &TerminalOptions) {
        let ctx = ui.ctx();
        if response.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                self.scroll_accum += dy / self.cell.y;
                let lines = self.scroll_accum.trunc() as i32;
                self.scroll_accum -= lines as f32;
                if lines != 0 {
                    let mode = *self.term.lock().mode();
                    if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                        let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
                        self.write(seq.repeat(lines.unsigned_abs() as usize));
                    } else {
                        self.term.lock().scroll_display(Scroll::Delta(lines));
                    }
                }
            }
        }

        let pointer = response.interact_pointer_pos();
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = pointer {
                let (point, side) = self.point_at(rect, pos);
                self.term.lock().selection = Some(Selection::new(SelectionType::Simple, point, side));
            }
        } else if response.dragged_by(PointerButton::Primary) {
            if let Some(pos) = response.hover_pos().or(pointer) {
                let (point, side) = self.point_at(rect, pos);
                if let Some(sel) = self.term.lock().selection.as_mut() {
                    sel.update(point, side);
                }
            }
        } else if response.drag_stopped_by(PointerButton::Primary) && opts.copy_on_select {
            self.copy_selection(ctx);
        }

        if response.triple_clicked() || response.double_clicked() {
            if let Some(pos) = pointer {
                let ty = if response.triple_clicked() { SelectionType::Lines } else { SelectionType::Semantic };
                let (point, side) = self.point_at(rect, pos);
                let mut sel = Selection::new(ty, point, side);
                sel.update(point, side);
                self.term.lock().selection = Some(sel);
                if opts.copy_on_select {
                    self.copy_selection(ctx);
                }
            }
        } else if response.clicked_by(PointerButton::Primary) {
            self.term.lock().selection = None;
        }

        let paste_click = (opts.right_click_paste && response.secondary_clicked())
            || response.clicked_by(PointerButton::Middle);
        if paste_click && self.is_running() {
            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        ui: &egui::Ui,
        outer: Rect,
        rect: Rect,
        theme: &TermTheme,
        font: &FontId,
        bold: &FontId,
        focused: bool,
    ) {
        let painter = ui.painter_at(outer);
        let bell = self.bell_until.is_some_and(|t| Instant::now() < t);
        painter.rect_filled(outer, 0.0, theme.bg);
        if bell {
            painter.rect_filled(outer, 0.0, theme.fg.gamma_multiply(0.06));
            ui.ctx().request_repaint_after(Duration::from_millis(160));
        }

        let term = self.term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let resolve = |c: AnsiColor| -> Color32 {
            match c {
                AnsiColor::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
                AnsiColor::Named(n) => match colors[n as usize] {
                    Some(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
                    None => theme.named(n),
                },
                AnsiColor::Indexed(i) => match colors[i as usize] {
                    Some(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
                    None => theme.indexed(i),
                },
            }
        };
        let cell_pos = |line: i32, col: usize| {
            Pos2::new(rect.left() + col as f32 * self.cell.x, rect.top() + (line + offset) as f32 * self.cell.y)
        };

        struct Run {
            line: i32,
            col: usize,
            text: String,
            fg: Color32,
            bold: bool,
            underline: bool,
            strike: bool,
        }
        let mut runs: Vec<Run> = Vec::new();
        let selection = content.selection;
        let mut cursor_glyph: Option<(char, bool)> = None;
        let cursor_point = content.cursor.point;

        for indexed in content.display_iter {
            let cell = indexed.cell;
            let point = indexed.point;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = resolve(cell.fg);
            let mut bg = resolve(cell.bg);
            if cell.flags.contains(Flags::BOLD)
                && let AnsiColor::Named(n) = cell.fg
                    && (n as usize) < 8 {
                        fg = theme.ansi[n as usize + 8];
                    }
            if cell.flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if selection.is_some_and(|s| s.contains(point)) {
                bg = theme.selection;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };
            if bg != theme.bg {
                let p = cell_pos(point.line.0, point.column.0);
                painter.rect_filled(
                    Rect::from_min_size(p, Vec2::new(self.cell.x * width + 0.5, self.cell.y)),
                    0.0,
                    bg,
                );
            }
            if point == cursor_point {
                cursor_glyph = Some((cell.c, cell.flags.contains(Flags::BOLD)));
            }

            let c = if cell.c == '\0' { ' ' } else { cell.c };
            let is_bold = cell.flags.contains(Flags::BOLD);
            let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
            let strike = cell.flags.contains(Flags::STRIKEOUT);
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let extends = runs.last().is_some_and(|r| {
                !wide
                    && r.line == point.line.0
                    && r.col + r.text.chars().count() == point.column.0
                    && r.fg == fg
                    && r.bold == is_bold
                    && r.underline == underline
                    && r.strike == strike
            });
            if extends {
                runs.last_mut().unwrap().text.push(c);
            } else {
                runs.push(Run {
                    line: point.line.0,
                    col: point.column.0,
                    text: c.to_string(),
                    fg,
                    bold: is_bold,
                    underline,
                    strike,
                });
            }
            if wide {
                // Force the next cell to start a fresh run at its own column.
                runs.push(Run {
                    line: i32::MIN,
                    col: 0,
                    text: String::new(),
                    fg,
                    bold: false,
                    underline: false,
                    strike: false,
                });
            }
        }

        for run in runs.iter().filter(|r| r.line != i32::MIN) {
            let p = cell_pos(run.line, run.col);
            if !run.text.trim().is_empty() {
                painter.text(p, egui::Align2::LEFT_TOP, &run.text, if run.bold { bold.clone() } else { font.clone() }, run.fg);
            }
            let w = run.text.chars().count() as f32 * self.cell.x;
            if run.underline {
                let y = p.y + self.cell.y - 1.5;
                painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0, run.fg));
            }
            if run.strike {
                let y = p.y + self.cell.y * 0.5;
                painter.line_segment([Pos2::new(p.x, y), Pos2::new(p.x + w, y)], Stroke::new(1.0, run.fg));
            }
        }

        // Cursor.
        let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && self.is_running();
        let cursor_line = cursor_point.line.0 + offset;
        if show_cursor && cursor_line >= 0 && (cursor_line as usize) < self.size.lines {
            let p = cell_pos(cursor_point.line.0, cursor_point.column.0);
            let r = Rect::from_min_size(p, self.cell);
            match (content.cursor.shape, focused) {
                (CursorShape::Hidden, _) => {}
                (_, false) => {
                    painter.rect_stroke(r.shrink(0.5), 1.0, Stroke::new(1.0, theme.cursor), egui::StrokeKind::Inside);
                }
                (CursorShape::Beam, true) => {
                    painter.rect_filled(Rect::from_min_size(p, Vec2::new(2.0, self.cell.y)), 0.0, theme.cursor);
                }
                (CursorShape::Underline, true) => {
                    let bar = Rect::from_min_size(Pos2::new(p.x, p.y + self.cell.y - 2.0), Vec2::new(self.cell.x, 2.0));
                    painter.rect_filled(bar, 0.0, theme.cursor);
                }
                (_, true) => {
                    painter.rect_filled(r, 1.0, theme.cursor);
                    if let Some((c, b)) = cursor_glyph.filter(|(c, _)| !c.is_whitespace() && *c != '\0') {
                        painter.text(p, egui::Align2::LEFT_TOP, c, if b { bold.clone() } else { font.clone() }, theme.bg);
                    }
                }
            }
        }

        // Scrollback position indicator.
        let history = term.grid().history_size();
        if offset > 0 && history > 0 {
            let total = (history + self.size.lines) as f32;
            let track = outer.shrink2(Vec2::new(0.0, 4.0));
            let h = (track.height() * self.size.lines as f32 / total).max(24.0);
            let top = track.top() + (track.height() - h) * (1.0 - offset as f32 / history as f32);
            let bar = Rect::from_min_size(Pos2::new(outer.right() - 6.0, top), Vec2::new(4.0, h));
            painter.rect_filled(bar, 2.0, theme.fg.gamma_multiply(0.35));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// Bytes an xterm-compatible terminal sends for a non-text key press.
fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let modp = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let csi_letter = |c: char| -> String {
        if modp > 1 {
            format!("\x1b[1;{modp}{c}")
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let tilde = |n: u8| -> String {
        if modp > 1 { format!("\x1b[{n};{modp}~") } else { format!("\x1b[{n}~") }
    };
    let ss3 = |c: char| -> String {
        if modp > 1 { format!("\x1b[1;{modp}{c}") } else { format!("\x1bO{c}") }
    };
    let esc = |s: &str| -> String { if m.alt { format!("\x1b{s}") } else { s.to_string() } };

    let s: String = match key {
        Key::ArrowUp => csi_letter('A'),
        Key::ArrowDown => csi_letter('B'),
        Key::ArrowRight => csi_letter('C'),
        Key::ArrowLeft => csi_letter('D'),
        Key::Home => csi_letter('H'),
        Key::End => csi_letter('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Enter => esc("\r"),
        Key::Backspace => esc(if m.ctrl { "\x08" } else { "\x7f" }),
        Key::Tab => if m.shift { "\x1b[Z".into() } else { "\t".into() },
        Key::Escape => "\x1b".into(),
        Key::Space if m.ctrl => "\0".into(),
        Key::OpenBracket if m.ctrl => "\x1b".into(),
        Key::Backslash if m.ctrl => "\x1c".into(),
        Key::CloseBracket if m.ctrl => "\x1d".into(),
        Key::Slash if m.ctrl => "\x1f".into(),
        _ if m.ctrl => {
            let name = key.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => {
                    let code = (c.to_ascii_uppercase() as u8 - b'A' + 1) as char;
                    esc(&code.to_string())
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(s.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_respect_app_cursor_and_modifiers() {
        let none = Modifiers::NONE;
        assert_eq!(key_bytes(Key::ArrowUp, none, TermMode::NONE).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, none, TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers::CTRL, TermMode::NONE).unwrap(), b"\x1b[1;5D");
    }

    #[test]
    fn ctrl_letters_become_control_codes() {
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL, TermMode::NONE).unwrap(), b"\x04");
        assert_eq!(key_bytes(Key::Z, Modifiers::CTRL, TermMode::NONE).unwrap(), b"\x1a");
        assert!(key_bytes(Key::D, Modifiers::NONE, TermMode::NONE).is_none());
    }

    #[test]
    fn password_prompts_are_detected() {
        for p in ["alice@db1's password:", "Password: ", "Enter passphrase for key '/home/a/.ssh/id':"] {
            assert!(PASSWORD_PROMPT.is_match(p.trim_end()), "{p}");
        }
        assert!(!PASSWORD_PROMPT.is_match("Last login: Tue"));
    }
}
