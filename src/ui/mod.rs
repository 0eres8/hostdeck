//! Application state, connection handling and top-level layout.

mod dialogs;
mod sidebar;
mod tabs;
pub mod theme;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Frame, Margin, RichText};
use egui_phosphor::regular as icon;
use uuid::Uuid;

use crate::launch::{self, RdpProcess};
use crate::model::{self, Protocol, Session, Store};
use crate::secrets;
use crate::terminal::Terminal;
use dialogs::{Confirm, CredentialManager, Editor, FolderDialog, ImportDialog, PasswordPrompt};
use theme::Palette;

pub enum TabKind {
    Home,
    Terminal(Box<Terminal>),
    Rdp(RdpTab),
}

pub struct RdpTab {
    pub process: Option<RdpProcess>,
    pub started: Instant,
    pub error: Option<String>,
    pub user_disconnected: bool,
}

pub struct Tab {
    pub id: u64,
    pub session: Session,
    pub password: Option<String>,
    pub kind: TabKind,
}

impl Tab {
    fn title(&self) -> String {
        match &self.kind {
            TabKind::Home => "Home".into(),
            _ => self.session.display_name(),
        }
    }

    fn is_live(&self) -> bool {
        match &self.kind {
            TabKind::Home => false,
            TabKind::Terminal(t) => t.is_running(),
            TabKind::Rdp(r) => r.process.as_ref().is_some_and(|p| p.exit_status().is_none()),
        }
    }
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

pub struct App {
    store: Store,
    store_path: PathBuf,
    dirty: bool,
    palette: Palette,
    tabs: Vec<Tab>,
    active: usize,
    shown_tab: Option<u64>,
    next_tab_id: u64,
    search: String,
    quick: String,
    selected: Option<Uuid>,
    editor: Option<Editor>,
    credentials: Option<CredentialManager>,
    settings_open: bool,
    prompt: Option<PasswordPrompt>,
    confirm: Option<Confirm>,
    folder_dialog: Option<FolderDialog>,
    import: Option<ImportDialog>,
    toasts: Vec<Toast>,
    keyring_ok: bool,
    rdp_client: Option<PathBuf>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, store_path: PathBuf) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let (store, load_error) = match Store::load(&store_path) {
            Ok(s) => (s, None),
            Err(e) => (Store::default(), Some(format!("{e:#}"))),
        };
        let palette = theme::apply(&cc.egui_ctx, store.settings.theme);
        let mut app = Self {
            store,
            store_path,
            dirty: false,
            palette,
            tabs: vec![Tab { id: 0, session: Session::default(), password: None, kind: TabKind::Home }],
            active: 0,
            shown_tab: None,
            next_tab_id: 1,
            search: String::new(),
            quick: String::new(),
            selected: None,
            editor: None,
            credentials: None,
            settings_open: false,
            prompt: None,
            confirm: None,
            folder_dialog: None,
            import: None,
            toasts: Vec::new(),
            keyring_ok: secrets::available(),
            rdp_client: launch::rdp_client(),
        };
        if let Some(e) = load_error {
            // Never overwrite a config file we could not read.
            app.store_path = app.store_path.with_extension("recovered.toml");
            app.toast_error(format!("Could not load sessions: {e}. Changes go to {}", app.store_path.display()));
        }
        app
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: false, until: Instant::now() + Duration::from_secs(4) });
    }

    pub(crate) fn toast_error(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), error: true, until: Instant::now() + Duration::from_secs(8) });
    }

    pub(crate) fn save(&mut self) {
        self.dirty = true;
    }

    fn flush(&mut self) {
        if self.dirty {
            self.dirty = false;
            if let Err(e) = self.store.save(&self.store_path) {
                self.toast_error(format!("Could not save sessions: {e:#}"));
            }
        }
    }

    pub(crate) fn set_theme(&mut self, ctx: &egui::Context, theme: model::ThemeChoice) {
        self.store.settings.theme = theme;
        self.palette = theme::apply(ctx, theme);
        self.save();
    }

    /// Username and saved password for a session, resolving shared credentials.
    pub(crate) fn login_for(&self, s: &Session) -> (String, Option<String>) {
        if let Some(cred) = s.credential.and_then(|id| self.store.credential(id)) {
            return (cred.username.clone(), secrets::get(&cred.password_key()));
        }
        let password = if s.save_password { secrets::get(&s.password_key()) } else { None };
        (s.username.clone(), password)
    }

    pub(crate) fn connect_id(&mut self, ctx: &egui::Context, id: Uuid) {
        self.connect_id_with(ctx, id, None);
    }

    pub(crate) fn connect_id_with(&mut self, ctx: &egui::Context, id: Uuid, password: Option<String>) {
        if let Some(s) = self.store.session_mut(id) {
            s.touch();
            let s = s.clone();
            self.save();
            self.connect_with(ctx, s, password);
        }
    }

    pub(crate) fn connect(&mut self, ctx: &egui::Context, session: Session) {
        self.connect_with(ctx, session, None);
    }

    fn connect_with(&mut self, ctx: &egui::Context, session: Session, password: Option<String>) {
        if session.host.trim().is_empty() {
            self.toast_error("This session has no host.");
            return;
        }
        let (username, saved) = self.login_for(&session);
        let password = password.or(saved);
        match session.protocol {
            Protocol::Ssh => self.open_ssh(ctx, session, password),
            Protocol::Rdp => match password {
                Some(pw) => self.open_rdp(ctx, session, &username, pw, None),
                None => self.prompt = Some(PasswordPrompt::new(session, username, None)),
            },
        }
    }

    fn push_tab(&mut self, session: Session, password: Option<String>, kind: TabKind) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab { id, session, password, kind });
        self.active = self.tabs.len() - 1;
    }

    fn spawn_ssh(&self, ctx: &egui::Context, session: &Session, password: Option<String>) -> Result<Terminal, String> {
        let (username, _) = self.login_for(session);
        let args = launch::ssh_args(session, &username);
        Terminal::spawn("ssh", args, ctx, self.store.settings.scrollback_lines, password)
            .map_err(|e| format!("Could not start ssh: {e}"))
    }

    fn open_ssh(&mut self, ctx: &egui::Context, session: Session, password: Option<String>) {
        match self.spawn_ssh(ctx, &session, password.clone()) {
            Ok(t) => self.push_tab(session, password, TabKind::Terminal(Box::new(t))),
            Err(e) => self.toast_error(e),
        }
    }

    fn spawn_rdp(&self, ctx: &egui::Context, session: &Session, username: &str, password: &str) -> Result<RdpProcess, String> {
        let Some(program) = &self.rdp_client else {
            return Err("FreeRDP is not installed. Install it with: sudo apt install freerdp3-x11".into());
        };
        let args = launch::rdp_args(session, username, &session.rdp.domain);
        RdpProcess::spawn(program, &args, password, ctx.clone()).map_err(|e| format!("Could not start FreeRDP: {e}"))
    }

    /// Open an RDP tab, or restart the RDP process of an existing tab.
    pub(crate) fn open_rdp(&mut self, ctx: &egui::Context, session: Session, username: &str, password: String, tab: Option<u64>) {
        let mut session = session;
        if session.credential.is_none() {
            session.username = username.to_string();
        }
        let result = self.spawn_rdp(ctx, &session, username, &password);
        let (process, error) = match result {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(e)),
        };
        let kind = RdpTab { process, started: Instant::now(), error, user_disconnected: false };
        match tab.and_then(|id| self.tabs.iter_mut().find(|t| t.id == id)) {
            Some(t) => {
                t.kind = TabKind::Rdp(kind);
                t.password = Some(password);
            }
            None => self.push_tab(session, Some(password), TabKind::Rdp(kind)),
        }
    }

    pub(crate) fn reconnect(&mut self, ctx: &egui::Context, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let session = tab.session.clone();
        let password = tab.password.clone();
        let id = tab.id;
        match session.protocol {
            Protocol::Ssh => match self.spawn_ssh(ctx, &session, password) {
                Ok(t) => self.tabs[index].kind = TabKind::Terminal(Box::new(t)),
                Err(e) => self.toast_error(e),
            },
            Protocol::Rdp => {
                let (username, _) = self.login_for(&session);
                match password {
                    Some(pw) => self.open_rdp(ctx, session, &username, pw, Some(id)),
                    None => self.prompt = Some(PasswordPrompt::new(session, username, Some(id))),
                }
            }
        }
    }

    pub(crate) fn duplicate_tab(&mut self, ctx: &egui::Context, index: usize) {
        if let Some(tab) = self.tabs.get(index) {
            let session = tab.session.clone();
            let password = tab.password.clone();
            match session.protocol {
                Protocol::Ssh => self.open_ssh(ctx, session, password),
                Protocol::Rdp => {
                    let (username, _) = self.login_for(&session);
                    match password {
                        Some(pw) => self.open_rdp(ctx, session, &username, pw, None),
                        None => self.prompt = Some(PasswordPrompt::new(session, username, None)),
                    }
                }
            }
        }
    }

    pub(crate) fn close_tab(&mut self, index: usize) {
        if index == 0 || index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.active >= index {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    pub(crate) fn delete_session(&mut self, id: Uuid) {
        if let Some(s) = self.store.session(id) {
            secrets::delete(&s.password_key());
        }
        self.store.sessions.retain(|s| s.id != id);
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.save();
    }

    pub(crate) fn duplicate_session(&mut self, id: Uuid) {
        if let Some(s) = self.store.session(id) {
            let mut copy = s.clone();
            copy.id = Uuid::new_v4();
            copy.name = format!("{} (copy)", s.display_name());
            copy.save_password = false;
            copy.last_used = None;
            copy.use_count = 0;
            self.selected = Some(copy.id);
            self.store.sessions.push(copy);
            self.save();
        }
    }

    fn quick_connect(&mut self, ctx: &egui::Context) {
        match model::quick_session(&self.quick) {
            Some(mut s) => {
                s.name = s.endpoint();
                self.quick.clear();
                self.connect(ctx, s);
            }
            None => self.toast_error("Type a host such as user@server, server:2222 or rdp://server."),
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        egui::Panel::top("top")
            .frame(Frame::new().fill(p.surface).inner_margin(Margin::symmetric(14, 9)).stroke(egui::Stroke::new(1.0, p.border)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::TERMINAL_WINDOW).size(22.0).color(p.accent));
                    ui.label(RichText::new("Hostdeck").font(theme::semibold(17.0)));
                    ui.add_space(18.0);

                    let width = (ui.available_width() - 190.0).clamp(220.0, 560.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.quick)
                            .hint_text(format!("{}  Quick connect: user@host, host:2222 or rdp://host", icon::LIGHTNING))
                            .desired_width(width)
                            .margin(Margin::symmetric(10, 5)),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button(format!("{} Connect", icon::SIGN_IN)).clicked() || enter) && !self.quick.trim().is_empty() {
                        let ctx = ui.ctx().clone();
                        self.quick_connect(&ctx);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new(icon::GEAR_SIX).size(17.0)).on_hover_text("Settings").clicked() {
                            self.settings_open = true;
                        }
                        let (theme_icon, next) = match self.store.settings.theme {
                            model::ThemeChoice::Dark => (icon::SUN, model::ThemeChoice::Light),
                            model::ThemeChoice::Light => (icon::MOON, model::ThemeChoice::Dark),
                        };
                        if ui.button(RichText::new(theme_icon).size(17.0)).on_hover_text("Toggle light/dark").clicked() {
                            let ctx = ui.ctx().clone();
                            self.set_theme(&ctx, next);
                        }
                        if ui.button(RichText::new(icon::KEY).size(17.0)).on_hover_text("Credentials").clicked() {
                            self.credentials = Some(CredentialManager::default());
                        }
                    });
                });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        egui::Panel::bottom("status")
            .frame(Frame::new().fill(p.surface).inner_margin(Margin::symmetric(14, 4)).stroke(egui::Stroke::new(1.0, p.border)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let live = self.tabs.iter().filter(|t| t.is_live()).count();
                    let small = |t: String| RichText::new(t).size(12.0).color(p.muted);
                    ui.label(small(format!("{} sessions", self.store.sessions.len())));
                    ui.label(small("·".into()));
                    let dot = if live > 0 { p.success } else { p.muted };
                    ui.label(RichText::new("●").size(10.0).color(dot));
                    ui.label(small(format!("{live} active connection{}", if live == 1 { "" } else { "s" })));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.keyring_ok {
                            ui.label(small(format!("{} Passwords stored in system keyring", icon::KEY)));
                        } else {
                            ui.label(RichText::new(format!("{} Keyring unavailable: passwords are not saved", icon::WARNING_CIRCLE)).size(12.0).color(p.danger))
                                .on_hover_text(secrets::status_message().unwrap_or_default());
                        }
                        if self.rdp_client.is_none() {
                            ui.label(RichText::new(format!("{} FreeRDP not installed", icon::WARNING_CIRCLE)).size(12.0).color(p.danger))
                                .on_hover_text("sudo apt install freerdp3-x11");
                        }
                    });
                });
            });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        let p = self.palette.clone();
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-18.0, -36.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    let color = if t.error { p.danger } else { p.success };
                    Frame::new()
                        .fill(p.surface_2)
                        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.7)))
                        .corner_radius(8)
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.horizontal_wrapped(|ui| {
                                let i = if t.error { icon::WARNING_CIRCLE } else { icon::CHECK_CIRCLE };
                                ui.label(RichText::new(i).color(color).size(16.0));
                                ui.label(&t.text);
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.top_bar(ui);
        self.status_bar(ui);
        self.sidebar(ui);
        self.central(ui);
        self.dialogs(&ctx);
        self.toasts_ui(&ctx);
        // Keep RDP status fresh while a client process runs.
        if self.tabs.iter().any(|t| matches!(t.kind, TabKind::Rdp(_)) && t.is_live()) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        self.flush();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c: Color32 = self.palette.bg;
        egui::Rgba::from(c).to_array()
    }
}
