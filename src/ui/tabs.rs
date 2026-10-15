//! Tab bar, home dashboard and the content of SSH and RDP tabs.

use eframe::egui::{self, Align2, Color32, FontId, Frame, Key, Margin, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use egui_phosphor::regular as icon;
use uuid::Uuid;

use super::dialogs::{Editor, ImportDialog, ImportKind};
use super::sidebar::protocol_icon;
use super::theme::{self, session_color, Palette};
use super::{App, TabKind};
use crate::launch;
use crate::model::{now_secs, Protocol, Session};
use crate::terminal::TerminalOptions;

enum TabAction {
    Activate(usize),
    Close(usize),
    CloseOthers(usize),
    Reconnect(usize),
    Duplicate(usize),
}

fn ago(secs: u64) -> String {
    let d = now_secs().saturating_sub(secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

impl App {
    pub(super) fn central(&mut self, ui: &mut egui::Ui) {
        self.global_shortcuts(ui.ctx());
        let p = self.palette.clone();
        egui::CentralPanel::default().frame(Frame::new().fill(p.bg)).show(ui, |ui| {
            self.tab_bar(ui);
            let index = self.active.min(self.tabs.len() - 1);
            // Give the terminal keyboard focus whenever its tab becomes the active one.
            if self.shown_tab != Some(self.tabs[index].id) {
                self.shown_tab = Some(self.tabs[index].id);
                if let TabKind::Terminal(t) = &mut self.tabs[index].kind {
                    t.focus_requested = true;
                }
            }
            match &self.tabs[index].kind {
                TabKind::Home => self.home(ui),
                TabKind::Terminal(_) => self.terminal_view(ui, index),
                TabKind::Rdp(_) => self.rdp_view(ui, index),
            }
        });
    }

    fn global_shortcuts(&mut self, ctx: &egui::Context) {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        let (prev, next, close, dup) = ctx.input_mut(|i| {
            (
                i.consume_key(ctrl_shift, Key::Tab),
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(ctrl_shift, Key::W),
                i.consume_key(ctrl_shift, Key::D),
            )
        });
        let n = self.tabs.len();
        if next {
            self.active = (self.active + 1) % n;
        }
        if prev {
            self.active = (self.active + n - 1) % n;
        }
        if close {
            self.close_tab(self.active);
        }
        if dup && self.active > 0 {
            self.duplicate_tab(ctx, self.active);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        let mut action = None;
        Frame::new()
            .fill(p.surface)
            .inner_margin(Margin { left: 8, right: 8, top: 6, bottom: 0 })
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("tabs")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for i in 0..self.tabs.len() {
                                if let Some(a) = self.tab_pill(ui, i, &p) {
                                    action = Some(a);
                                }
                            }
                        });
                    });
            });
        let ctx = ui.ctx().clone();
        match action {
            Some(TabAction::Activate(i)) => self.active = i,
            Some(TabAction::Close(i)) => self.close_tab(i),
            Some(TabAction::CloseOthers(i)) => {
                let keep = self.tabs[i].id;
                self.tabs.retain(|t| t.id == keep || matches!(t.kind, TabKind::Home));
                self.active = self.tabs.iter().position(|t| t.id == keep).unwrap_or(0);
            }
            Some(TabAction::Reconnect(i)) => self.reconnect(&ctx, i),
            Some(TabAction::Duplicate(i)) => self.duplicate_tab(&ctx, i),
            None => {}
        }
    }

    fn tab_pill(&self, ui: &mut egui::Ui, i: usize, p: &Palette) -> Option<TabAction> {
        let tab = &self.tabs[i];
        let is_home = matches!(tab.kind, TabKind::Home);
        let active = i == self.active;
        let title = truncate(&tab.title(), 26);
        let font = FontId::proportional(13.5);
        let text_w = ui.fonts_mut(|f| f.layout_no_wrap(title.clone(), font.clone(), p.text).size().x);
        let width = if is_home { 44.0 } else { (text_w + 90.0).clamp(130.0, 280.0) };
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());

        let fill = if active { p.bg } else if resp.hovered() { p.hover } else { Color32::TRANSPARENT };
        ui.painter().rect_filled(rect, egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, fill);
        let accent = if is_home { p.accent } else { session_color(tab.session.color) };
        if active {
            let line = Rect::from_min_size(rect.left_top() + Vec2::new(8.0, 0.0), Vec2::new(rect.width() - 16.0, 2.0));
            ui.painter().rect_filled(line, 1.0, accent);
        }
        let cy = rect.center().y + 1.0;
        if is_home {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon::HOUSE, FontId::proportional(16.0), if active { p.text } else { p.muted });
            let resp = resp.on_hover_text("Home");
            return resp.clicked().then_some(TabAction::Activate(i));
        }

        let glyph = protocol_icon(tab.session.protocol);
        ui.painter().text(Pos2::new(rect.left() + 12.0, cy), Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), accent);
        let text_color = if active { p.text } else { p.muted };
        ui.painter_at(rect.shrink(4.0)).text(Pos2::new(rect.left() + 34.0, cy), Align2::LEFT_CENTER, &title, font, text_color);

        let dot = if tab.is_live() { p.success } else { p.muted.gamma_multiply(0.6) };
        ui.painter().circle_filled(Pos2::new(rect.right() - 36.0, cy), 3.0, dot);

        let close_rect = Rect::from_center_size(Pos2::new(rect.right() - 16.0, cy), Vec2::splat(20.0));
        let close = ui.interact(close_rect, resp.id.with("close"), Sense::click());
        if close.hovered() {
            ui.painter().rect_filled(close_rect, 4.0, p.surface_2);
        }
        ui.painter().text(close_rect.center(), Align2::CENTER_CENTER, icon::X, FontId::proportional(12.0), if close.hovered() { p.text } else { p.muted });

        let status = match &tab.kind {
            TabKind::Terminal(t) if t.is_running() => "Connected".to_string(),
            TabKind::Terminal(t) => format!("Closed (exit code {})", t.exit_code.unwrap_or(-1)),
            TabKind::Rdp(_) if tab.is_live() => "Remote desktop running".to_string(),
            _ => "Disconnected".to_string(),
        };
        let resp = resp.on_hover_text(format!("{}\n{}  ·  {status}", tab.session.display_name(), tab.session.endpoint()));

        let mut action = None;
        resp.context_menu(|ui| {
            if ui.button(format!("{}  Reconnect", icon::ARROW_CLOCKWISE)).clicked() {
                action = Some(TabAction::Reconnect(i));
                ui.close();
            }
            if ui.button(format!("{}  Duplicate", icon::COPY)).clicked() {
                action = Some(TabAction::Duplicate(i));
                ui.close();
            }
            ui.separator();
            if ui.button("Close").clicked() {
                action = Some(TabAction::Close(i));
                ui.close();
            }
            if ui.button("Close other tabs").clicked() {
                action = Some(TabAction::CloseOthers(i));
                ui.close();
            }
        });
        if action.is_some() {
            return action;
        }
        if close.clicked() || resp.middle_clicked() {
            Some(TabAction::Close(i))
        } else if resp.clicked() {
            Some(TabAction::Activate(i))
        } else {
            None
        }
    }

    fn terminal_view(&mut self, ui: &mut egui::Ui, index: usize) {
        let p = self.palette.clone();
        let opts = TerminalOptions {
            font_size: self.store.settings.terminal_font_size,
            copy_on_select: self.store.settings.copy_on_select,
            right_click_paste: self.store.settings.right_click_paste,
        };
        let exit = match &self.tabs[index].kind {
            TabKind::Terminal(t) => t.exit_code,
            _ => None,
        };
        let mut reconnect = false;
        let mut close = false;
        if let Some(code) = exit {
            egui::Panel::bottom(egui::Id::new(("ended", self.tabs[index].id)))
                .frame(Frame::new().fill(p.surface_2).inner_margin(Margin::symmetric(14, 8)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (glyph, color) = if code == 0 { (icon::CHECK_CIRCLE, p.muted) } else { (icon::WARNING_CIRCLE, p.danger) };
                        ui.label(RichText::new(glyph).color(color).size(16.0));
                        let msg = if code == 0 {
                            "The connection was closed.".to_string()
                        } else {
                            format!("The connection ended with exit code {code}. Details are shown above.")
                        };
                        ui.label(msg);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close tab").clicked() {
                                close = true;
                            }
                            let btn = egui::Button::new(RichText::new(format!("{}  Reconnect (R)", icon::ARROW_CLOCKWISE)).color(Color32::WHITE)).fill(p.accent);
                            if ui.add(btn).clicked() {
                                reconnect = true;
                            }
                        });
                    });
                });
        }
        if let TabKind::Terminal(t) = &mut self.tabs[index].kind {
            let out = t.ui(ui, &p.terminal, &opts);
            reconnect |= out.reconnect;
        }
        let ctx = ui.ctx().clone();
        if reconnect {
            self.reconnect(&ctx, index);
        } else if close {
            self.close_tab(index);
        }
    }

    fn rdp_view(&mut self, ui: &mut egui::Ui, index: usize) {
        let p = self.palette.clone();
        let tab = &mut self.tabs[index];
        let session = tab.session.clone();
        let TabKind::Rdp(rdp) = &mut tab.kind else { return };
        let exit = rdp.process.as_mut().and_then(|proc| proc.poll());
        let running = rdp.process.is_some() && exit.is_none();
        let log = rdp.process.as_ref().map(|proc| proc.log_lines()).unwrap_or_default();
        let elapsed = rdp.started.elapsed().as_secs();
        let error = rdp.error.clone();
        let user_disconnected = rdp.user_disconnected;

        let mut reconnect = false;
        let mut disconnect = false;
        let mut close = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(620.0);
                Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(14).inner_margin(Margin::same(28)).show(ui, |ui| {
                    ui.set_width(560.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(icon::MONITOR).size(46.0).color(session_color(session.color)));
                        ui.label(RichText::new(session.display_name()).font(theme::semibold(20.0)));
                        ui.label(RichText::new(session.endpoint()).color(p.muted));
                        ui.add_space(12.0);
                        if running {
                            ui.label(RichText::new(format!("●  Remote desktop running  ·  {}:{:02}", elapsed / 60, elapsed % 60)).color(p.success));
                            ui.label(RichText::new("The remote desktop is open in its own window.").color(p.muted));
                        } else if let Some(e) = &error {
                            ui.label(RichText::new(format!("{}  {e}", icon::WARNING_CIRCLE)).color(p.danger));
                        } else {
                            let code = exit.and_then(|s| s.code());
                            let end = launch::explain_rdp_end(code, &log, user_disconnected);
                            let color = if end.normal { p.muted } else { p.danger };
                            let headline = match (end.normal, code) {
                                (false, Some(c)) => format!("{} (exit code {c})", end.headline),
                                _ => end.headline.to_string(),
                            };
                            ui.label(RichText::new(headline).color(color));
                            if let Some(why) = end.detail {
                                ui.label(why);
                            }
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            let w = if running { 250.0 } else { 290.0 };
                            ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                            if running {
                                if ui.button(format!("{}  Disconnect", icon::X)).clicked() {
                                    disconnect = true;
                                }
                                if ui.button(format!("{}  Restart", icon::ARROW_CLOCKWISE)).clicked() {
                                    reconnect = true;
                                }
                            } else {
                                let btn = egui::Button::new(RichText::new(format!("{}  Reconnect", icon::ARROW_CLOCKWISE)).color(Color32::WHITE)).fill(p.accent);
                                if ui.add(btn).clicked() {
                                    reconnect = true;
                                }
                                if ui.button("Close tab").clicked() {
                                    close = true;
                                }
                            }
                        });
                    });
                    if !log.is_empty() {
                        ui.add_space(12.0);
                        egui::CollapsingHeader::new(RichText::new("FreeRDP log").color(p.muted)).id_salt(("rdplog", index)).show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                                for line in log.iter().rev().take(200).rev() {
                                    ui.label(RichText::new(line).monospace().size(11.5).color(p.muted));
                                }
                            });
                        });
                    }
                });
            });
        });
        let ctx = ui.ctx().clone();
        if disconnect {
            if let TabKind::Rdp(r) = &mut self.tabs[index].kind
                && let Some(proc) = r.process.as_mut()
            {
                r.user_disconnected = true;
                proc.kill();
            }
        } else if reconnect {
            if let TabKind::Rdp(r) = &mut self.tabs[index].kind {
                r.process = None;
            }
            self.reconnect(&ctx, index);
        } else if close {
            self.close_tab(index);
        }
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        let p = self.palette.clone();
        let mut connect: Option<Uuid> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = (ui.available_width() - 72.0).min(1040.0);
            let pad = (ui.available_width() - width) / 2.0;
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(30.0);
                    ui.label(RichText::new("Welcome to Hostdeck").font(theme::semibold(26.0)));
                    ui.label(RichText::new("Your SSH and remote desktop connections, in one place.").color(p.muted));
                    ui.add_space(20.0);

                    let actions: [(&str, &str, &str, u8); 4] = [
                        (icon::TERMINAL_WINDOW, "New SSH session", "Keys, jump hosts and tunnels", 0),
                        (icon::MONITOR, "New RDP session", "Windows desktops via FreeRDP", 1),
                        (icon::DOWNLOAD_SIMPLE, "Import MobaXterm", "Bring your .mxtsessions export", 2),
                        (icon::FILE_ARROW_UP, "Import ~/.ssh/config", "Every Host alias as a session", 3),
                    ];
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
                        for (glyph, title, sub, which) in actions {
                            if action_card(ui, &p, glyph, title, sub).clicked() {
                                match which {
                                    0 => self.editor = Some(Editor::create(Session::new(Protocol::Ssh, ""))),
                                    1 => self.editor = Some(Editor::create(Session::new(Protocol::Rdp, ""))),
                                    2 => self.import = Some(ImportDialog::new(ImportKind::MobaXterm)),
                                    _ => self.import = Some(ImportDialog::new(ImportKind::SshConfig)),
                                }
                            }
                        }
                    });

                    let mut favorites: Vec<Session> = self.store.sessions.iter().filter(|s| s.favorite).cloned().collect();
                    favorites.sort_by_key(|s| s.display_name().to_lowercase());
                    if !favorites.is_empty() {
                        ui.add_space(28.0);
                        section(ui, &p, icon::STAR, "Favorites");
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
                            for s in &favorites {
                                if session_card(ui, &p, s).double_clicked() {
                                    connect = Some(s.id);
                                }
                            }
                        });
                        ui.label(RichText::new("Double-click a card to connect.").size(11.5).color(p.muted));
                    }

                    let mut recent: Vec<Session> = self.store.sessions.iter().filter(|s| s.last_used.is_some()).cloned().collect();
                    recent.sort_by_key(|s| std::cmp::Reverse(s.last_used));
                    recent.truncate(8);
                    ui.add_space(28.0);
                    section(ui, &p, icon::CLOCK_COUNTER_CLOCKWISE, "Recent");
                    if recent.is_empty() {
                        ui.label(RichText::new("Sessions you connect to appear here.").color(p.muted));
                    }
                    for s in &recent {
                        Frame::new().fill(p.surface).corner_radius(8).inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(protocol_icon(s.protocol)).size(16.0).color(session_color(s.color)));
                                ui.label(RichText::new(s.display_name()).strong());
                                ui.label(RichText::new(s.endpoint()).color(p.muted));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button(format!("{}  Connect", icon::SIGN_IN)).clicked() {
                                        connect = Some(s.id);
                                    }
                                    if let Some(t) = s.last_used {
                                        ui.label(RichText::new(ago(t)).size(12.0).color(p.muted));
                                    }
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_space(30.0);
                });
            });
        });
        if let Some(id) = connect {
            let ctx = ui.ctx().clone();
            self.connect_id(&ctx, id);
        }
    }
}

fn section(ui: &mut egui::Ui, p: &Palette, glyph: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).color(p.muted));
        ui.label(RichText::new(title.to_uppercase()).font(theme::semibold(12.0)).color(p.muted));
    });
    ui.add_space(6.0);
}

fn card_frame(ui: &mut egui::Ui, p: &Palette, size: Vec2) -> (Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    ui.painter().rect_filled(rect, 12.0, if hovered { p.hover } else { p.surface });
    ui.painter().rect_stroke(rect, 12.0, Stroke::new(1.0, if hovered { p.accent.gamma_multiply(0.7) } else { p.border }), egui::StrokeKind::Inside);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (rect, resp)
}

fn action_card(ui: &mut egui::Ui, p: &Palette, glyph: &str, title: &str, sub: &str) -> egui::Response {
    let (rect, resp) = card_frame(ui, p, Vec2::new(236.0, 92.0));
    let painter = ui.painter_at(rect);
    painter.text(rect.left_top() + Vec2::new(18.0, 18.0), Align2::LEFT_TOP, glyph, FontId::proportional(22.0), p.accent);
    painter.text(rect.left_top() + Vec2::new(18.0, 48.0), Align2::LEFT_TOP, title, theme::semibold(14.5), p.text);
    painter.text(rect.left_top() + Vec2::new(18.0, 68.0), Align2::LEFT_TOP, truncate(sub, 34), FontId::proportional(12.0), p.muted);
    resp
}

fn session_card(ui: &mut egui::Ui, p: &Palette, s: &Session) -> egui::Response {
    let (rect, resp) = card_frame(ui, p, Vec2::new(236.0, 74.0));
    let painter = ui.painter_at(rect);
    let color = session_color(s.color);
    painter.rect_filled(Rect::from_min_size(rect.left_top() + Vec2::new(14.0, 18.0), Vec2::new(38.0, 38.0)), 10.0, color.gamma_multiply(0.18));
    painter.text(rect.left_top() + Vec2::new(33.0, 37.0), Align2::CENTER_CENTER, protocol_icon(s.protocol), FontId::proportional(19.0), color);
    painter.text(rect.left_top() + Vec2::new(64.0, 20.0), Align2::LEFT_TOP, truncate(&s.display_name(), 22), theme::semibold(14.0), p.text);
    painter.text(rect.left_top() + Vec2::new(64.0, 41.0), Align2::LEFT_TOP, truncate(&s.endpoint(), 26), FontId::proportional(12.0), p.muted);
    resp
}
