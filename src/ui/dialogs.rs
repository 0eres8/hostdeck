//! Modal dialogs: session editor, credentials, settings, prompts and import/export.

use std::path::PathBuf;

use eframe::egui::{self, Color32, Id, Margin, RichText, Sense, Stroke, Vec2};
use egui_phosphor::regular as icon;
use uuid::Uuid;

use super::sidebar::protocol_icon;
use super::theme::{self, Palette, SESSION_COLORS};
use super::App;
use crate::import;
use crate::model::{
    folder_leaf, join_folder, CertPolicy, Credential, ForwardKind, PortForward, Protocol, RdpAudio, RdpResolution,
    Session, Store, ThemeChoice,
};
use crate::secrets;

#[derive(Clone, Copy, PartialEq)]
enum EditorTab {
    General,
    Login,
    Options,
    Notes,
}

pub struct Editor {
    draft: Session,
    is_new: bool,
    password: String,
    password_dirty: bool,
    tab: EditorTab,
}

impl Editor {
    pub fn create(session: Session) -> Self {
        Self { draft: session, is_new: true, password: String::new(), password_dirty: false, tab: EditorTab::General }
    }

    pub fn edit(session: Session, keyring_ok: bool) -> Self {
        let password = if session.save_password && keyring_ok {
            secrets::get(&session.password_key()).unwrap_or_default()
        } else {
            String::new()
        };
        Self { draft: session, is_new: false, password, password_dirty: false, tab: EditorTab::General }
    }
}

#[derive(PartialEq)]
enum Outcome {
    Open,
    Cancel,
    Save,
    SaveAndConnect,
}

#[derive(Default)]
pub struct CredentialManager {
    editing: Option<(Credential, String, bool)>,
}

pub struct PasswordPrompt {
    session: Session,
    username: String,
    password: String,
    remember: bool,
    tab: Option<u64>,
    focus: bool,
}

impl PasswordPrompt {
    pub fn new(session: Session, username: String, tab: Option<u64>) -> Self {
        Self { session, username, password: String::new(), remember: false, tab, focus: true }
    }
}

pub enum ConfirmAction {
    DeleteSession(Uuid),
    DeleteFolder(String),
    DeleteCredential(Uuid),
}

pub struct Confirm {
    title: String,
    body: String,
    action: ConfirmAction,
}

impl Confirm {
    pub fn new(title: String, body: String, action: ConfirmAction) -> Self {
        Self { title, body, action }
    }
}

enum FolderMode {
    Create { parent: String },
    Rename { old: String },
}

pub struct FolderDialog {
    mode: FolderMode,
    name: String,
}

impl FolderDialog {
    pub fn create(parent: String) -> Self {
        Self { mode: FolderMode::Create { parent }, name: String::new() }
    }

    pub fn rename(old: String) -> Self {
        let name = folder_leaf(&old).to_string();
        Self { mode: FolderMode::Rename { old }, name }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum ImportKind {
    MobaXterm,
    SshConfig,
    Hostdeck,
    Export,
}

pub struct ImportDialog {
    kind: ImportKind,
    path: String,
    folder: String,
}

impl ImportDialog {
    pub fn new(kind: ImportKind) -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let (path, folder) = match kind {
            ImportKind::MobaXterm => (String::new(), "MobaXterm".to_string()),
            ImportKind::SshConfig => (home.join(".ssh/config").display().to_string(), "SSH config".to_string()),
            ImportKind::Hostdeck => (String::new(), String::new()),
            ImportKind::Export => (home.join("hostdeck-sessions.toml").display().to_string(), String::new()),
        };
        Self { kind, path, folder }
    }
}

fn modal_title(ui: &mut egui::Ui, p: &Palette, glyph: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).size(20.0).color(p.accent));
        ui.label(RichText::new(title).font(theme::semibold(18.0)));
    });
    ui.add_space(10.0);
}

fn primary(text: impl Into<String>, p: &Palette) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(Color32::WHITE)).fill(p.accent)
}

fn hint(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(p.muted));
}

fn form_grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([14.0, 10.0]).min_col_width(130.0).show(ui, add);
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, placeholder: &str) -> egui::Response {
    ui.label(label);
    let r = ui.add(egui::TextEdit::singleline(value).hint_text(placeholder).desired_width(f32::INFINITY));
    ui.end_row();
    r
}

fn pick_file(start: Option<PathBuf>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(dir) = start.filter(|d| d.exists()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file().map(|p| p.display().to_string())
}

fn editor_ui(ed: &mut Editor, ui: &mut egui::Ui, store: &Store, p: &Palette, keyring_ok: bool) -> Outcome {
    let title = if ed.is_new { format!("New {} session", ed.draft.protocol.label()) } else { "Edit session".to_string() };
    modal_title(ui, p, protocol_icon(ed.draft.protocol), &title);

    if ed.is_new {
        ui.horizontal(|ui| {
            for proto in [Protocol::Ssh, Protocol::Rdp] {
                let before = ed.draft.protocol;
                let label = format!("{}  {}", protocol_icon(proto), proto.label());
                if ui.selectable_value(&mut ed.draft.protocol, proto, label).clicked() && before != proto
                    && ed.draft.port == before.default_port() {
                        ed.draft.port = proto.default_port();
                    }
            }
        });
        ui.add_space(6.0);
    }

    ui.horizontal(|ui| {
        let options = format!("{} options", ed.draft.protocol.label());
        for (tab, label) in [
            (EditorTab::General, "General"),
            (EditorTab::Login, "Login"),
            (EditorTab::Options, options.as_str()),
            (EditorTab::Notes, "Notes"),
        ] {
            ui.selectable_value(&mut ed.tab, tab, label);
        }
    });
    ui.separator();
    ui.add_space(6.0);

    egui::ScrollArea::vertical().max_height(430.0).auto_shrink([false, true]).show(ui, |ui| match ed.tab {
        EditorTab::General => general_tab(ed, ui, store, p),
        EditorTab::Login => login_tab(ed, ui, store, p, keyring_ok),
        EditorTab::Options => match ed.draft.protocol {
            Protocol::Ssh => ssh_tab(ed, ui, p),
            Protocol::Rdp => rdp_tab(ed, ui, p),
        },
        EditorTab::Notes => {
            ui.add(egui::TextEdit::multiline(&mut ed.draft.notes).desired_rows(8).desired_width(f32::INFINITY).hint_text("Notes about this host"));
        }
    });

    ui.add_space(12.0);
    let mut outcome = Outcome::Open;
    ui.horizontal(|ui| {
        if ed.draft.host.trim().is_empty() {
            ui.label(RichText::new("A host is required.").size(12.0).color(p.danger));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let valid = !ed.draft.host.trim().is_empty();
            if ui.add_enabled(valid, primary(format!("{}  Save & connect", icon::SIGN_IN), p)).clicked() {
                outcome = Outcome::SaveAndConnect;
            }
            if ui.add_enabled(valid, egui::Button::new("Save")).clicked() {
                outcome = Outcome::Save;
            }
            if ui.button("Cancel").clicked() {
                outcome = Outcome::Cancel;
            }
        });
    });
    outcome
}

fn general_tab(ed: &mut Editor, ui: &mut egui::Ui, store: &Store, p: &Palette) {
    form_grid(ui, "general", |ui| {
        text_row(ui, "Name", &mut ed.draft.name, "Shown in the session list (optional)");
        let host_hint = match ed.draft.protocol {
            Protocol::Ssh => "server.example.com, 10.0.0.5 or an ~/.ssh/config alias",
            Protocol::Rdp => "server.example.com or 10.0.0.20",
        };
        text_row(ui, "Host", &mut ed.draft.host, host_hint);
        ui.label("Port");
        ui.add(egui::DragValue::new(&mut ed.draft.port).range(1..=65535));
        ui.end_row();

        ui.label("Folder");
        let current = if ed.draft.folder.is_empty() { "(Top level)".to_string() } else { ed.draft.folder.clone() };
        egui::ComboBox::from_id_salt("folder").selected_text(current).width(300.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut ed.draft.folder, String::new(), "(Top level)");
            for f in store.all_folders() {
                ui.selectable_value(&mut ed.draft.folder, f.clone(), f);
            }
        });
        ui.end_row();

        ui.label("Color");
        ui.horizontal(|ui| {
            for (i, (name, color)) in SESSION_COLORS.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                ui.painter().circle_filled(rect.center(), 8.0, *color);
                if ed.draft.color == i {
                    ui.painter().circle_stroke(rect.center(), 10.0, Stroke::new(2.0, p.text));
                }
                if resp.on_hover_text(*name).clicked() {
                    ed.draft.color = i;
                }
            }
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut ed.draft.favorite, format!("{}  Show in favorites", icon::STAR));
        ui.end_row();
    });
}

fn login_tab(ed: &mut Editor, ui: &mut egui::Ui, store: &Store, p: &Palette, keyring_ok: bool) {
    form_grid(ui, "login", |ui| {
        ui.label("Credentials");
        let selected = ed
            .draft
            .credential
            .and_then(|id| store.credential(id))
            .map(|c| format!("{}  {}", icon::KEY, c.name))
            .unwrap_or_else(|| "This session's own login".into());
        egui::ComboBox::from_id_salt("cred").selected_text(selected).width(300.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut ed.draft.credential, None, "This session's own login");
            for c in &store.credentials {
                ui.selectable_value(&mut ed.draft.credential, Some(c.id), format!("{}  {} ({})", icon::KEY, c.name, c.username));
            }
        });
        ui.end_row();

        if ed.draft.credential.is_none() {
            text_row(ui, "Username", &mut ed.draft.username, "Leave empty to use your local username or ssh config");
            ui.label("Password");
            if ui.add(egui::TextEdit::singleline(&mut ed.password).password(true).desired_width(f32::INFINITY)).changed() {
                ed.password_dirty = true;
            }
            ui.end_row();
            ui.label("");
            ui.add_enabled_ui(keyring_ok, |ui| {
                ui.checkbox(&mut ed.draft.save_password, "Save password in the system keyring");
            });
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    if !keyring_ok {
        hint(ui, p, "No system keyring is available, so passwords cannot be saved. You will be asked when connecting.");
    }
    match ed.draft.protocol {
        Protocol::Ssh => hint(
            ui,
            p,
            "SSH tries your keys and ssh-agent first. A saved password is typed automatically at the first password prompt; otherwise you are asked in the terminal.",
        ),
        Protocol::Rdp => hint(ui, p, "Without a saved password you are asked each time you connect."),
    }
    if store.credentials.is_empty() {
        hint(ui, p, "Tip: create shared credentials with the key button in the top bar and reuse them across sessions.");
    }
}

fn ssh_tab(ed: &mut Editor, ui: &mut egui::Ui, p: &Palette) {
    let o = &mut ed.draft.ssh;
    form_grid(ui, "ssh", |ui| {
        ui.label("Private key");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut o.identity_file).hint_text("~/.ssh/id_ed25519 (optional)").desired_width(260.0));
            if ui.button("Browse…").clicked()
                && let Some(f) = pick_file(dirs::home_dir().map(|h| h.join(".ssh"))) {
                    o.identity_file = f;
                }
        });
        ui.end_row();
        text_row(ui, "Jump host", &mut o.jump_host, "user@bastion:22 (comma-separate several)");
        text_row(ui, "Remote command", &mut o.remote_command, "Run instead of a login shell, e.g. htop");
        ui.label("Keepalive");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut o.keepalive_secs).range(0..=3600).suffix(" s"));
            hint(ui, p, "0 turns it off");
        });
        ui.end_row();
        ui.label("Features");
        ui.vertical(|ui| {
            ui.checkbox(&mut o.x11_forwarding, "X11 forwarding (run graphical apps)");
            ui.checkbox(&mut o.agent_forwarding, "Forward ssh-agent");
            ui.checkbox(&mut o.compression, "Compression");
        });
        ui.end_row();
    });

    ui.add_space(10.0);
    ui.label(RichText::new("Port forwarding").font(theme::semibold(14.0)));
    let mut remove = None;
    for (i, f) in o.forwards.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("fwd", i)).width(90.0).selected_text(format!("{:?}", f.kind)).show_ui(ui, |ui| {
                ui.selectable_value(&mut f.kind, ForwardKind::Local, "Local");
                ui.selectable_value(&mut f.kind, ForwardKind::Remote, "Remote");
                ui.selectable_value(&mut f.kind, ForwardKind::Dynamic, "Dynamic");
            });
            ui.add(egui::DragValue::new(&mut f.bind_port).range(1..=65535).prefix("port "));
            if f.kind != ForwardKind::Dynamic {
                ui.label("→");
                ui.add(egui::TextEdit::singleline(&mut f.dest_host).hint_text("localhost").desired_width(140.0));
                ui.add(egui::DragValue::new(&mut f.dest_port).range(1..=65535).prefix(":"));
            } else {
                hint(ui, p, "SOCKS proxy");
            }
            if ui.button(icon::X).on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        o.forwards.remove(i);
    }
    if ui.button(format!("{}  Add forward", icon::PLUS)).clicked() {
        o.forwards.push(PortForward { kind: ForwardKind::Local, bind_port: 8080, dest_host: "localhost".into(), dest_port: 80 });
    }

    ui.add_space(10.0);
    form_grid(ui, "ssh-extra", |ui| {
        text_row(ui, "Extra ssh arguments", &mut o.extra_args, "-o StrictHostKeyChecking=ask");
    });
}

fn rdp_tab(ed: &mut Editor, ui: &mut egui::Ui, p: &Palette) {
    let o = &mut ed.draft.rdp;
    form_grid(ui, "rdp", |ui| {
        text_row(ui, "Domain", &mut o.domain, "CORP (optional)");
        ui.label("Display");
        let label = match o.resolution {
            RdpResolution::FitWindow => "Fit to window",
            RdpResolution::Fullscreen => "Fullscreen",
            RdpResolution::Fixed => "Fixed size",
        };
        egui::ComboBox::from_id_salt("res").selected_text(label).width(200.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut o.resolution, RdpResolution::FitWindow, "Fit to window");
            ui.selectable_value(&mut o.resolution, RdpResolution::Fullscreen, "Fullscreen");
            ui.selectable_value(&mut o.resolution, RdpResolution::Fixed, "Fixed size");
        });
        ui.end_row();
        match o.resolution {
            RdpResolution::Fullscreen => {
                ui.label("");
                ui.checkbox(&mut o.multimon, "Use all monitors");
                ui.end_row();
            }
            _ => {
                ui.label(if o.resolution == RdpResolution::FitWindow { "Initial size" } else { "Size" });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut o.width).range(640..=7680));
                    ui.label("×");
                    ui.add(egui::DragValue::new(&mut o.height).range(480..=4320));
                });
                ui.end_row();
            }
        }
        ui.label("Scaling");
        egui::ComboBox::from_id_salt("scale").selected_text(format!("{}%", o.scale)).show_ui(ui, |ui| {
            for s in [100, 140, 180] {
                ui.selectable_value(&mut o.scale, s, format!("{s}%"));
            }
        });
        ui.end_row();
        ui.label("Sound");
        let audio = match o.audio {
            RdpAudio::Local => "Play on this computer",
            RdpAudio::Remote => "Play on the remote computer",
            RdpAudio::Off => "Off",
        };
        egui::ComboBox::from_id_salt("audio").selected_text(audio).width(220.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut o.audio, RdpAudio::Local, "Play on this computer");
            ui.selectable_value(&mut o.audio, RdpAudio::Remote, "Play on the remote computer");
            ui.selectable_value(&mut o.audio, RdpAudio::Off, "Off");
        });
        ui.end_row();
        ui.label("Resources");
        ui.vertical(|ui| {
            ui.checkbox(&mut o.clipboard, "Share clipboard");
            ui.checkbox(&mut o.redirect_home, "Share my home folder as a drive");
            ui.checkbox(&mut o.admin, "Connect to the admin console");
        });
        ui.end_row();
        text_row(ui, "RD Gateway", &mut o.gateway, "gateway.example.com (optional)");
        text_row(ui, "Gateway user", &mut o.gateway_user, "Defaults to the session user");
        ui.label("Certificates");
        let cert = match o.cert_policy {
            CertPolicy::Tofu => "Trust on first use",
            CertPolicy::Ignore => "Accept any (insecure)",
            CertPolicy::Deny => "Reject unknown",
        };
        egui::ComboBox::from_id_salt("cert").selected_text(cert).width(220.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut o.cert_policy, CertPolicy::Tofu, "Trust on first use");
            ui.selectable_value(&mut o.cert_policy, CertPolicy::Ignore, "Accept any (insecure)");
            ui.selectable_value(&mut o.cert_policy, CertPolicy::Deny, "Reject unknown");
        });
        ui.end_row();
        text_row(ui, "Extra FreeRDP args", &mut o.extra_args, "/network:lan +fonts");
    });
    ui.add_space(6.0);
    hint(ui, p, "Remote desktops open in their own window, powered by FreeRDP. Ctrl+Alt+Enter toggles fullscreen.");
    if o.resolution == RdpResolution::Fullscreen {
        hint(ui, p, "In fullscreen, a bar at the top edge lets you minimize, leave fullscreen or disconnect.");
    }
}

impl App {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        self.editor_dialog(ctx);
        self.credentials_dialog(ctx);
        self.settings_dialog(ctx);
        self.password_dialog(ctx);
        self.folder_name_dialog(ctx);
        self.import_dialog(ctx);
        self.confirm_dialog(ctx);
    }

    fn editor_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.editor.take() else { return };
        let p = self.palette.clone();
        let keyring_ok = self.keyring_ok;
        let mut outcome = Outcome::Open;
        let resp = egui::Modal::new(Id::new("editor")).show(ctx, |ui| {
            ui.set_width(600.0);
            outcome = editor_ui(&mut ed, ui, &self.store, &p, keyring_ok);
        });
        if resp.should_close() && outcome == Outcome::Open {
            outcome = Outcome::Cancel;
        }
        match outcome {
            Outcome::Open => self.editor = Some(ed),
            Outcome::Cancel => {}
            Outcome::Save | Outcome::SaveAndConnect => {
                let s = ed.draft.clone();
                if s.credential.is_none() && keyring_ok {
                    if s.save_password {
                        if ed.password_dirty || ed.is_new {
                            if ed.password.is_empty() {
                                secrets::delete(&s.password_key());
                            } else if let Err(e) = secrets::set(&s.password_key(), &ed.password) {
                                self.toast_error(format!("Could not save the password: {e}"));
                            }
                        }
                    } else {
                        secrets::delete(&s.password_key());
                    }
                }
                let id = s.id;
                match self.store.session_mut(id) {
                    Some(existing) => *existing = s,
                    None => self.store.sessions.push(s),
                }
                self.selected = Some(id);
                self.save();
                if outcome == Outcome::SaveAndConnect {
                    // A password typed but not saved is still used for this connection.
                    let typed = (ed.draft.credential.is_none() && !ed.password.is_empty()).then(|| ed.password.clone());
                    self.connect_id_with(ctx, id, typed);
                }
            }
        }
    }

    fn credentials_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut mgr) = self.credentials.take() else { return };
        let p = self.palette.clone();
        let mut close = false;
        let mut delete = None;
        let mut save = None;
        let mut cancel_edit = false;
        let resp = egui::Modal::new(Id::new("credentials")).show(ctx, |ui| {
            ui.set_width(520.0);
            modal_title(ui, &p, icon::KEY, "Credentials");
            hint(ui, &p, "Named logins you can reuse across sessions. Passwords are kept in the system keyring.");
            ui.add_space(8.0);
            if self.store.credentials.is_empty() && mgr.editing.is_none() {
                ui.label(RichText::new("No credentials yet.").color(p.muted));
            }
            for c in &self.store.credentials {
                let used = self.store.sessions.iter().filter(|s| s.credential == Some(c.id)).count();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::KEY).color(p.accent));
                    ui.label(RichText::new(&c.name).strong());
                    ui.label(RichText::new(format!("{} · used by {used}", c.username)).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(icon::TRASH).on_hover_text("Delete").clicked() {
                            delete = Some(c.id);
                        }
                        if ui.button(icon::PENCIL_SIMPLE).on_hover_text("Edit").clicked() {
                            let pw = secrets::get(&c.password_key()).unwrap_or_default();
                            mgr.editing = Some((c.clone(), pw, false));
                        }
                    });
                });
            }
            ui.add_space(8.0);
            if let Some((cred, pw, is_new)) = &mut mgr.editing {
                egui::Frame::new().fill(p.surface_2).corner_radius(8).inner_margin(Margin::same(12)).show(ui, |ui| {
                    form_grid(ui, "cred-form", |ui| {
                        text_row(ui, "Name", &mut cred.name, "e.g. Domain admin");
                        text_row(ui, "Username", &mut cred.username, "DOMAIN\\user or user");
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(pw).password(true).desired_width(f32::INFINITY));
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        let ok = !cred.name.trim().is_empty();
                        if ui.add_enabled(ok, primary(if *is_new { "Add" } else { "Save" }, &p)).clicked() {
                            save = Some((cred.clone(), pw.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_edit = true;
                        }
                    });
                });
            } else if ui.button(format!("{}  Add credential", icon::PLUS)).clicked() {
                mgr.editing = Some((Credential::default(), String::new(), true));
            }
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").clicked() {
                    close = true;
                }
            });
        });
        if cancel_edit {
            mgr.editing = None;
        }
        if let Some((cred, pw)) = save {
            if !self.keyring_ok {
                self.toast_error("No system keyring is available; the password was not saved.");
            } else if pw.is_empty() {
                secrets::delete(&cred.password_key());
            } else if let Err(e) = secrets::set(&cred.password_key(), &pw) {
                self.toast_error(format!("Could not save the password: {e}"));
            }
            match self.store.credentials.iter_mut().find(|c| c.id == cred.id) {
                Some(c) => *c = cred,
                None => self.store.credentials.push(cred),
            }
            mgr.editing = None;
            self.save();
        }
        if let Some(id) = delete {
            let name = self.store.credential(id).map(|c| c.name.clone()).unwrap_or_default();
            self.confirm = Some(Confirm::new(
                format!("Delete credential “{name}”?"),
                "Sessions that use it fall back to their own login.".into(),
                ConfirmAction::DeleteCredential(id),
            ));
        }
        if !(close || resp.should_close()) {
            self.credentials = Some(mgr);
        }
    }

    fn settings_dialog(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let p = self.palette.clone();
        let mut theme = self.store.settings.theme;
        let mut s = self.store.settings.clone();
        let mut close = false;
        let resp = egui::Modal::new(Id::new("settings")).show(ctx, |ui| {
            ui.set_width(480.0);
            modal_title(ui, &p, icon::GEAR_SIX, "Settings");
            form_grid(ui, "settings", |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut theme, ThemeChoice::Dark, format!("{}  Dark", icon::MOON));
                    ui.selectable_value(&mut theme, ThemeChoice::Light, format!("{}  Light", icon::SUN));
                });
                ui.end_row();
                ui.label("Terminal font size");
                ui.add(egui::Slider::new(&mut s.terminal_font_size, 10.0..=24.0).step_by(0.5));
                ui.end_row();
                ui.label("Scrollback");
                ui.add(egui::DragValue::new(&mut s.scrollback_lines).range(1000..=200_000).speed(100).suffix(" lines"));
                ui.end_row();
                ui.label("Mouse");
                ui.vertical(|ui| {
                    ui.checkbox(&mut s.copy_on_select, "Copy text when selected");
                    ui.checkbox(&mut s.right_click_paste, "Paste with right-click");
                });
                ui.end_row();
            });
            ui.add_space(8.0);
            hint(ui, &p, "Keys: Ctrl+C interrupts, Ctrl+Shift+C copies, Ctrl+V or Ctrl+Shift+V pastes, Shift+PageUp/PageDown scrolls.");
            hint(ui, &p, &format!("Sessions are stored in {}", self.store_path.display()));
            hint(ui, &p, &format!("Hostdeck {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").clicked() {
                    close = true;
                }
            });
        });
        if s != self.store.settings {
            self.store.settings = s;
            self.save();
        }
        if theme != self.store.settings.theme {
            self.set_theme(ctx, theme);
        }
        if close || resp.should_close() {
            self.settings_open = false;
        }
    }

    fn password_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut pr) = self.prompt.take() else { return };
        let p = self.palette.clone();
        let mut submit = false;
        let mut cancel = false;
        let keyring_ok = self.keyring_ok;
        let own_login = pr.session.credential.is_none();
        let resp = egui::Modal::new(Id::new("password")).show(ctx, |ui| {
            ui.set_width(420.0);
            modal_title(ui, &p, protocol_icon(pr.session.protocol), &format!("Connect to {}", pr.session.display_name()));
            form_grid(ui, "pw", |ui| {
                ui.label("Username");
                ui.add_enabled(own_login, egui::TextEdit::singleline(&mut pr.username).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("Password");
                let r = ui.add(egui::TextEdit::singleline(&mut pr.password).password(true).desired_width(f32::INFINITY));
                if pr.focus {
                    r.request_focus();
                    pr.focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.end_row();
            });
            let saved = self.store.session(pr.session.id).is_some();
            if keyring_ok && own_login && saved {
                ui.checkbox(&mut pr.remember, "Remember this password");
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary(format!("{}  Connect", icon::SIGN_IN), &p)).clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if submit {
            if pr.remember {
                if let Some(s) = self.store.session_mut(pr.session.id) {
                    s.save_password = true;
                    s.username = pr.username.clone();
                }
                if let Err(e) = secrets::set(&pr.session.password_key(), &pr.password) {
                    self.toast_error(format!("Could not save the password: {e}"));
                }
                self.save();
            }
            let PasswordPrompt { session, username, password, tab, .. } = pr;
            self.open_rdp(ctx, session, &username, password, tab);
        } else if !(cancel || resp.should_close()) {
            self.prompt = Some(pr);
        }
    }

    fn folder_name_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.folder_dialog.take() else { return };
        let p = self.palette.clone();
        let mut done = false;
        let mut cancel = false;
        let title = match &d.mode {
            FolderMode::Create { parent } if parent.is_empty() => "New folder".to_string(),
            FolderMode::Create { parent } => format!("New folder in {}", folder_leaf(parent)),
            FolderMode::Rename { .. } => "Rename folder".to_string(),
        };
        let resp = egui::Modal::new(Id::new("folder")).show(ctx, |ui| {
            ui.set_width(380.0);
            modal_title(ui, &p, icon::FOLDER_PLUS, &title);
            let r = ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("Folder name").desired_width(f32::INFINITY));
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = true;
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(primary("OK", &p)).clicked() {
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        let name = d.name.trim().replace('/', "-");
        if done && !name.is_empty() {
            match &d.mode {
                FolderMode::Create { parent } => {
                    let path = join_folder(parent, &name);
                    self.store.settings.collapsed_folders.remove(parent);
                    self.store.folders.insert(path);
                }
                FolderMode::Rename { old } => {
                    let parent = old.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                    let new = join_folder(parent, &name);
                    if new != *old && self.store.all_folders().contains(&new) {
                        self.toast_error(format!("A folder named “{name}” already exists there."));
                    } else {
                        self.store.rename_folder(old, &new);
                    }
                }
            }
            self.save();
        } else if !(cancel || resp.should_close()) {
            self.folder_dialog = Some(d);
        }
    }

    fn import_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.import.take() else { return };
        let p = self.palette.clone();
        let mut run = false;
        let mut cancel = false;
        let (title, about, action) = match d.kind {
            ImportKind::MobaXterm => (
                "Import MobaXterm sessions",
                "In MobaXterm, right-click your sessions and choose “Export sessions”, then pick the .mxtsessions file. SSH and RDP sessions are imported with their folders, host, port, user and key file.",
                "Import",
            ),
            ImportKind::SshConfig => (
                "Import ~/.ssh/config",
                "Each Host entry becomes a session that connects through its alias, so every option in your ssh config keeps working.",
                "Import",
            ),
            ImportKind::Hostdeck => ("Import Hostdeck file", "Import sessions exported from another Hostdeck.", "Import"),
            ImportKind::Export => (
                "Export sessions",
                "Writes all sessions and folders to a file. Passwords and credentials are not included.",
                "Export",
            ),
        };
        let resp = egui::Modal::new(Id::new("import")).show(ctx, |ui| {
            ui.set_width(520.0);
            let glyph = if d.kind == ImportKind::Export { icon::UPLOAD_SIMPLE } else { icon::DOWNLOAD_SIMPLE };
            modal_title(ui, &p, glyph, title);
            hint(ui, &p, about);
            ui.add_space(8.0);
            form_grid(ui, "import", |ui| {
                ui.label("File");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(330.0));
                    if d.kind != ImportKind::Export && ui.button("Browse…").clicked()
                        && let Some(f) = pick_file(dirs::home_dir()) {
                            d.path = f;
                        }
                });
                ui.end_row();
                if matches!(d.kind, ImportKind::MobaXterm | ImportKind::SshConfig) {
                    text_row(ui, "Into folder", &mut d.folder, "(Top level)");
                }
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!d.path.trim().is_empty(), primary(action, &p)).clicked() {
                    run = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if run {
            let path = PathBuf::from(crate::launch::expand_home(d.path.trim()));
            let folder = d.folder.trim().trim_matches('/').to_string();
            let result = match d.kind {
                ImportKind::MobaXterm => import::read_mobaxterm(&path, &folder).map(Some),
                ImportKind::SshConfig => import::read_ssh_config(&path, &folder).map(Some),
                ImportKind::Hostdeck => import::read_hostdeck(&path).map(Some),
                ImportKind::Export => import::export_hostdeck(&self.store, &path).map(|_| None),
            };
            match result {
                Ok(Some(report)) => {
                    let skipped = report.skipped.len();
                    let (added, dupes) = import::merge(&mut self.store, report);
                    self.save();
                    let mut msg = format!("Imported {added} session{}", if added == 1 { "" } else { "s" });
                    if dupes > 0 {
                        msg.push_str(&format!(", {dupes} already existed"));
                    }
                    if skipped > 0 {
                        msg.push_str(&format!(", {skipped} skipped (only SSH and RDP are supported)"));
                    }
                    self.toast(msg);
                }
                Ok(None) => self.toast(format!("Exported {} sessions to {}", self.store.sessions.len(), path.display())),
                Err(e) => {
                    self.toast_error(format!("{e:#}"));
                    self.import = Some(d);
                }
            }
        } else if !(cancel || resp.should_close()) {
            self.import = Some(d);
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm.take() else { return };
        let p = self.palette.clone();
        let mut yes = false;
        let mut no = false;
        let resp = egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
            ui.set_width(400.0);
            modal_title(ui, &p, icon::WARNING_CIRCLE, &c.title);
            ui.label(&c.body);
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let delete = egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(p.danger);
                if ui.add(delete).clicked() {
                    yes = true;
                }
                if ui.button("Cancel").clicked() {
                    no = true;
                }
            });
        });
        if yes {
            match c.action {
                ConfirmAction::DeleteSession(id) => self.delete_session(id),
                ConfirmAction::DeleteFolder(f) => {
                    for id in self.store.sessions_in(&f, true) {
                        if let Some(s) = self.store.session(id) {
                            secrets::delete(&s.password_key());
                        }
                    }
                    let n = self.store.delete_folder(&f);
                    self.save();
                    self.toast(format!("Deleted folder and {n} session{}", if n == 1 { "" } else { "s" }));
                }
                ConfirmAction::DeleteCredential(id) => {
                    if let Some(cred) = self.store.credential(id) {
                        secrets::delete(&cred.password_key());
                    }
                    self.store.credentials.retain(|c| c.id != id);
                    for s in &mut self.store.sessions {
                        if s.credential == Some(id) {
                            s.credential = None;
                        }
                    }
                    self.save();
                }
            }
        } else if !(no || resp.should_close()) {
            self.confirm = Some(c);
        }
    }
}
