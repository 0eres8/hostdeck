//! The session tree: folders, search, drag and drop, and context menus.

use eframe::egui::{self, Align2, Color32, FontId, Frame, Margin, Pos2, Rect, Response, RichText, Sense, Stroke, Vec2};
use egui_phosphor::regular as icon;
use uuid::Uuid;

use super::dialogs::{Confirm, ConfirmAction, Editor, FolderDialog, ImportDialog, ImportKind};
use super::theme::{self, session_color, Palette};
use super::App;
use crate::model::{folder_leaf, join_folder, Protocol, Session};

#[derive(Clone, Copy)]
struct DragSession(Uuid);

#[derive(Clone)]
struct DragFolder(String);

const ROW_H: f32 = 28.0;
const INDENT: f32 = 16.0;

pub fn protocol_icon(p: Protocol) -> &'static str {
    match p {
        Protocol::Ssh => icon::TERMINAL_WINDOW,
        Protocol::Rdp => icon::MONITOR,
    }
}

fn parent_of(folder: &str) -> &str {
    folder.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

/// Paint a sidebar row background and return its rect and response.
fn row_frame(ui: &mut egui::Ui, p: &Palette, selected: bool, drop_target: bool) -> (Rect, Response) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click_and_drag());
    let fill = if drop_target {
        p.accent.gamma_multiply(0.25)
    } else if selected {
        p.selected
    } else if resp.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, fill);
    if drop_target {
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
    }
    (rect, resp)
}

fn row_text(ui: &egui::Ui, rect: Rect, x: f32, text: &str, color: Color32, trailing: Option<(&str, Color32)>) {
    let painter = ui.painter_at(rect.shrink2(Vec2::new(2.0, 0.0)));
    let mut right = rect.right() - 8.0;
    if let Some((t, c)) = trailing {
        let r = painter.text(Pos2::new(right, rect.center().y), Align2::RIGHT_CENTER, t, FontId::proportional(11.5), c);
        right = r.left() - 6.0;
    }
    let clip = Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(right, rect.bottom()));
    ui.painter_at(clip).text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(14.0), color);
}

impl App {
    /// Folder that new items should go into, based on the selection.
    fn current_folder(&self) -> String {
        self.selected.and_then(|id| self.store.session(id)).map(|s| s.folder.clone()).unwrap_or_default()
    }

    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        let resp = egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(self.store.settings.sidebar_width)
            .size_range(220.0..=520.0)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin { left: 10, right: 10, top: 12, bottom: 8 })
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                self.sidebar_header(ui);
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(format!("{}  Search sessions", icon::MAGNIFYING_GLASS))
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(10, 6)),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if self.store.sessions.is_empty() && self.store.folders.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(icon::PLUGS_CONNECTED).size(34.0).color(p.muted));
                            ui.label(RichText::new("No sessions yet").color(p.muted));
                            ui.label(RichText::new("Create one with + or import from MobaXterm.").size(12.0).color(p.muted));
                        });
                    } else if self.search.trim().is_empty() {
                        self.folder_contents(ui, "", 0);
                    } else {
                        self.search_results(ui);
                    }
                    // The empty area below the tree is a drop target for the root folder.
                    let rest = ui.available_size().max(Vec2::new(ui.available_width(), 48.0));
                    let (rect, resp) = ui.allocate_exact_size(rest, Sense::click());
                    self.accept_drops(&resp, "", rect);
                    resp.context_menu(|ui| self.folder_menu(ui, ""));
                });
            });
        let width = resp.response.rect.width();
        if (width - self.store.settings.sidebar_width).abs() > 1.0 && !ui.input(|i| i.pointer.any_down()) {
            self.store.settings.sidebar_width = width;
            self.save();
        }
        self.drag_preview(ui.ctx());
    }

    fn sidebar_header(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("SESSIONS").font(theme::semibold(12.0)).color(p.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new(icon::DOTS_THREE_VERTICAL).size(16.0), |ui| {
                    if ui.button(format!("{}  Import MobaXterm sessions…", icon::DOWNLOAD_SIMPLE)).clicked() {
                        self.import = Some(ImportDialog::new(ImportKind::MobaXterm));
                        ui.close();
                    }
                    if ui.button(format!("{}  Import ~/.ssh/config…", icon::DOWNLOAD_SIMPLE)).clicked() {
                        self.import = Some(ImportDialog::new(ImportKind::SshConfig));
                        ui.close();
                    }
                    if ui.button(format!("{}  Import Hostdeck file…", icon::DOWNLOAD_SIMPLE)).clicked() {
                        self.import = Some(ImportDialog::new(ImportKind::Hostdeck));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(format!("{}  Export sessions…", icon::UPLOAD_SIMPLE)).clicked() {
                        self.import = Some(ImportDialog::new(ImportKind::Export));
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Import and export");
                if ui.button(RichText::new(icon::FOLDER_PLUS).size(16.0)).on_hover_text("New folder").clicked() {
                    self.folder_dialog = Some(FolderDialog::create(self.current_folder()));
                }
                ui.menu_button(RichText::new(icon::PLUS).size(16.0), |ui| {
                    let folder = self.current_folder();
                    self.new_session_items(ui, &folder);
                })
                .response
                .on_hover_text("New session");
            });
        });
    }

    fn new_session_items(&mut self, ui: &mut egui::Ui, folder: &str) {
        if ui.button(format!("{}  New SSH session", icon::TERMINAL_WINDOW)).clicked() {
            self.editor = Some(Editor::create(Session::new(Protocol::Ssh, folder)));
            ui.close();
        }
        if ui.button(format!("{}  New RDP session", icon::MONITOR)).clicked() {
            self.editor = Some(Editor::create(Session::new(Protocol::Rdp, folder)));
            ui.close();
        }
    }

    fn folder_contents(&mut self, ui: &mut egui::Ui, folder: &str, depth: usize) {
        let subfolders: Vec<String> = self
            .store
            .all_folders()
            .into_iter()
            .filter(|f| parent_of(f) == folder)
            .collect();
        for sub in subfolders {
            self.folder_row(ui, &sub, depth);
            if !self.store.settings.collapsed_folders.contains(&sub) {
                self.folder_contents(ui, &sub, depth + 1);
            }
        }
        let mut sessions: Vec<(Uuid, String)> = self
            .store
            .sessions
            .iter()
            .filter(|s| s.folder == folder)
            .map(|s| (s.id, s.display_name()))
            .collect();
        sessions.sort_by_key(|(_, name)| name.to_lowercase());
        for (id, _) in sessions {
            self.session_row(ui, id, depth, false);
        }
    }

    fn search_results(&mut self, ui: &mut egui::Ui) {
        let q = self.search.trim().to_string();
        let mut hits: Vec<(Uuid, String)> = self
            .store
            .sessions
            .iter()
            .filter(|s| s.matches(&q))
            .map(|s| (s.id, s.display_name()))
            .collect();
        hits.sort_by_key(|(_, n)| n.to_lowercase());
        if hits.is_empty() {
            ui.label(RichText::new(format!("No sessions match “{q}”")).color(self.palette.muted));
        }
        for (id, _) in hits {
            self.session_row(ui, id, 0, true);
        }
    }

    fn folder_row(&mut self, ui: &mut egui::Ui, folder: &str, depth: usize) {
        let p = self.palette.clone();
        let collapsed = self.store.settings.collapsed_folders.contains(folder);
        let hovering_drop = egui::DragAndDrop::has_any_payload(ui.ctx());
        let (rect, resp) = row_frame(ui, &p, false, false);
        let is_target = hovering_drop
            && resp.contains_pointer()
            && (resp.dnd_hover_payload::<DragSession>().is_some()
                || resp.dnd_hover_payload::<DragFolder>().is_some_and(|d| !is_within(folder, &d.0)));
        if is_target {
            ui.painter().rect_filled(rect, 6.0, p.accent.gamma_multiply(0.25));
        }
        let x = rect.left() + 6.0 + depth as f32 * INDENT;
        let painter = ui.painter();
        let caret = if collapsed { icon::CARET_RIGHT } else { icon::CARET_DOWN };
        painter.text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, caret, FontId::proportional(12.0), p.muted);
        let folder_icon = if collapsed { icon::FOLDER } else { icon::FOLDER_OPEN };
        painter.text(Pos2::new(x + 16.0, rect.center().y), Align2::LEFT_CENTER, folder_icon, FontId::proportional(15.0), p.accent.gamma_multiply(0.85));
        let count = self.store.sessions_in(folder, true).len();
        row_text(ui, rect, x + 38.0, folder_leaf(folder), p.text, Some((&count.to_string(), p.muted)));

        if resp.clicked() {
            if collapsed {
                self.store.settings.collapsed_folders.remove(folder);
            } else {
                self.store.settings.collapsed_folders.insert(folder.to_string());
            }
            self.save();
        }
        resp.dnd_set_drag_payload(DragFolder(folder.to_string()));
        self.accept_drops(&resp, folder, rect);
        resp.context_menu(|ui| self.folder_menu(ui, folder));
    }

    fn session_row(&mut self, ui: &mut egui::Ui, id: Uuid, depth: usize, show_folder: bool) {
        let p = self.palette.clone();
        let Some(s) = self.store.session(id).cloned() else { return };
        let (rect, resp) = row_frame(ui, &p, self.selected == Some(id), false);
        let x = rect.left() + 6.0 + depth as f32 * INDENT + 16.0;
        ui.painter().text(
            Pos2::new(x, rect.center().y),
            Align2::LEFT_CENTER,
            protocol_icon(s.protocol),
            FontId::proportional(15.0),
            session_color(s.color),
        );
        let trailing = if show_folder && !s.folder.is_empty() {
            Some((folder_leaf(&s.folder), p.muted))
        } else if s.favorite {
            Some((icon::STAR, Color32::from_rgb(0xe0, 0xa8, 0x2e)))
        } else {
            None
        };
        row_text(ui, rect, x + 22.0, &s.display_name(), p.text, trailing);

        let resp = resp.on_hover_ui(|ui| {
            ui.label(RichText::new(s.display_name()).strong());
            ui.label(format!("{}  {}", s.protocol.label(), s.endpoint()));
            if !s.notes.is_empty() {
                ui.label(RichText::new(&s.notes).size(12.0).color(p.muted));
            }
            ui.label(RichText::new("Double-click to connect").size(11.0).color(p.muted));
        });
        if resp.clicked() {
            self.selected = Some(id);
        }
        if resp.double_clicked() {
            let ctx = ui.ctx().clone();
            self.connect_id(&ctx, id);
        }
        resp.dnd_set_drag_payload(DragSession(id));
        resp.context_menu(|ui| self.session_menu(ui, id));
    }

    /// Move a dragged session or folder into `folder` when it is dropped on `resp`.
    fn accept_drops(&mut self, resp: &Response, folder: &str, rect: Rect) {
        if let Some(d) = resp.dnd_hover_payload::<DragSession>() {
            let _ = d;
            resp.ctx.layer_painter(resp.layer_id).rect_stroke(rect, 6.0, Stroke::new(1.0, self.palette.accent), egui::StrokeKind::Inside);
        }
        if let Some(d) = resp.dnd_release_payload::<DragSession>()
            && let Some(s) = self.store.session_mut(d.0)
                && s.folder != folder {
                    s.folder = folder.to_string();
                    self.save();
                }
        if let Some(d) = resp.dnd_release_payload::<DragFolder>() {
            let src = d.0.clone();
            if !is_within(folder, &src) && parent_of(&src) != folder {
                let dest = join_folder(folder, folder_leaf(&src));
                if self.store.all_folders().contains(&dest) {
                    self.toast_error(format!("A folder named “{}” already exists there.", folder_leaf(&src)));
                } else {
                    self.store.rename_folder(&src, &dest);
                    self.save();
                }
            }
        }
    }

    fn drag_preview(&self, ctx: &egui::Context) {
        let label = if let Some(d) = egui::DragAndDrop::payload::<DragSession>(ctx) {
            self.store.session(d.0).map(|s| format!("{}  {}", protocol_icon(s.protocol), s.display_name()))
        } else {
            egui::DragAndDrop::payload::<DragFolder>(ctx).map(|d| format!("{}  {}", icon::FOLDER, folder_leaf(&d.0)))
        };
        let (Some(label), Some(pos)) = (label, ctx.pointer_interact_pos()) else { return };
        egui::Area::new(egui::Id::new("drag-preview"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + Vec2::new(14.0, 10.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| ui.label(label));
            });
    }

    fn session_menu(&mut self, ui: &mut egui::Ui, id: Uuid) {
        let Some(s) = self.store.session(id).cloned() else { return };
        if ui.button(format!("{}  Connect", icon::SIGN_IN)).clicked() {
            let ctx = ui.ctx().clone();
            self.connect_id(&ctx, id);
            ui.close();
        }
        if ui.button(format!("{}  Edit…", icon::PENCIL_SIMPLE)).clicked() {
            self.editor = Some(Editor::edit(s.clone(), self.keyring_ok));
            ui.close();
        }
        if ui.button(format!("{}  Duplicate", icon::COPY)).clicked() {
            self.duplicate_session(id);
            ui.close();
        }
        let fav = if s.favorite { "Remove from favorites" } else { "Add to favorites" };
        if ui.button(format!("{}  {fav}", icon::STAR)).clicked() {
            if let Some(s) = self.store.session_mut(id) {
                s.favorite = !s.favorite;
            }
            self.save();
            ui.close();
        }
        ui.menu_button(format!("{}  Move to", icon::FOLDER), |ui| {
            let mut targets: Vec<String> = vec![String::new()];
            targets.extend(self.store.all_folders());
            for f in targets {
                let label = if f.is_empty() { "(Top level)".to_string() } else { f.clone() };
                if ui.add_enabled(f != s.folder, egui::Button::new(label)).clicked() {
                    if let Some(s) = self.store.session_mut(id) {
                        s.folder = f;
                    }
                    self.save();
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button(RichText::new(format!("{}  Delete…", icon::TRASH)).color(self.palette.danger)).clicked() {
            self.confirm = Some(Confirm::new(
                format!("Delete “{}”?", s.display_name()),
                "The session and its saved password are removed.".into(),
                ConfirmAction::DeleteSession(id),
            ));
            ui.close();
        }
    }

    fn folder_menu(&mut self, ui: &mut egui::Ui, folder: &str) {
        self.new_session_items(ui, folder);
        if ui.button(format!("{}  New folder…", icon::FOLDER_PLUS)).clicked() {
            self.folder_dialog = Some(FolderDialog::create(folder.to_string()));
            ui.close();
        }
        if folder.is_empty() {
            return;
        }
        let ids = self.store.sessions_in(folder, false);
        if ui.add_enabled(!ids.is_empty(), egui::Button::new(format!("{}  Open all ({})", icon::SIGN_IN, ids.len()))).clicked() {
            let ctx = ui.ctx().clone();
            for id in ids {
                self.connect_id(&ctx, id);
            }
            ui.close();
        }
        if ui.button(format!("{}  Rename…", icon::PENCIL_SIMPLE)).clicked() {
            self.folder_dialog = Some(FolderDialog::rename(folder.to_string()));
            ui.close();
        }
        ui.separator();
        if ui.button(RichText::new(format!("{}  Delete folder…", icon::TRASH)).color(self.palette.danger)).clicked() {
            let n = self.store.sessions_in(folder, true).len();
            self.confirm = Some(Confirm::new(
                format!("Delete folder “{}”?", folder_leaf(folder)),
                format!("This removes the folder, its subfolders and {n} session{}.", if n == 1 { "" } else { "s" }),
                ConfirmAction::DeleteFolder(folder.to_string()),
            ));
            ui.close();
        }
    }
}

/// True if `folder` is `ancestor` or inside it.
fn is_within(folder: &str, ancestor: &str) -> bool {
    folder == ancestor || folder.starts_with(&format!("{ancestor}/"))
}
