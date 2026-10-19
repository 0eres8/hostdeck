//! An egui widget that shows a built-in RDP session, resizes the remote desktop with the
//! view, and passes keyboard, mouse and clipboard through.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, EventFilter, Key, MouseWheelUnit, PointerButton, Pos2, Rect, Sense,
    TextureHandle, TextureOptions, Vec2,
};
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, WheelRotations};
use ironrdp::pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};

use crate::rdp::{Input, Phase, RdpSession};

/// Wait this long after the view stops changing size before resizing the remote desktop.
const RESIZE_SETTLE: Duration = Duration::from_millis(300);

pub struct RdpView {
    pub session: RdpSession,
    texture: Option<TextureHandle>,
    mouse: Database,
    /// Keys held down on the remote side, as (extended, scancode).
    keys_down: BTreeSet<(bool, u8)>,
    /// Mouse buttons pressed inside the view, which keep the pointer captured.
    buttons_down: u8,
    wheel: Vec2,
    sent_size: Option<(u16, u16, u32)>,
    pending_size: Option<((u16, u16, u32), Instant)>,
    had_focus: bool,
    pub focus_requested: bool,
}

impl RdpView {
    pub fn new(session: RdpSession) -> Self {
        Self {
            session,
            texture: None,
            mouse: Database::new(),
            keys_down: BTreeSet::new(),
            buttons_down: 0,
            wheel: Vec2::ZERO,
            sent_size: None,
            pending_size: None,
            had_focus: false,
            focus_requested: true,
        }
    }

    pub fn phase(&self) -> Phase {
        self.session.shared().phase.clone()
    }

    pub fn is_running(&self) -> bool {
        self.session.is_running()
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.session.shared().log.clone()
    }

    pub fn disconnect(&self) {
        self.session.send(Input::Disconnect);
    }

    /// Send Ctrl+Alt+Del, which the local desktop would otherwise intercept.
    pub fn ctrl_alt_del(&mut self) {
        let keys = [(false, 0x1D), (false, 0x38), (true, 0x53)];
        let mut events: Vec<_> = keys.iter().map(|&k| key_event(k, true)).collect();
        events.extend(keys.iter().rev().map(|&k| key_event(k, false)));
        self.session.send(Input::Events(events));
    }

    /// Show the remote desktop in all of the available space. `scale` is the extra display
    /// scaling in percent chosen for the session.
    pub fn ui(&mut self, ui: &mut egui::Ui, scale: u32, background: Color32) {
        let ctx = ui.ctx().clone();
        let outer = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(outer, Sense::click_and_drag());
        let ppp = ctx.pixels_per_point();
        ui.painter().rect_filled(outer, 0.0, background);

        self.report_size(&ctx, outer, ppp, scale);
        self.update_texture(&ctx);
        if let Some(text) = self.session.shared().remote_clipboard.take() {
            ctx.copy_text(text);
        }

        let connected = self.phase() == Phase::Connected;
        let image_rect = self.texture.as_ref().map(|t| fit(outer, t.size_vec2() / ppp, ppp));
        if let (Some(t), Some(r)) = (&self.texture, image_rect) {
            ui.painter().image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }

        if self.focus_requested || response.clicked() || response.drag_started() {
            response.request_focus();
        }
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
        } else if self.had_focus {
            self.release_keys();
        }
        self.had_focus = focused;

        if !connected {
            return;
        }
        if let (Some(t), Some(r)) = (&self.texture, image_rect) {
            let remote = t.size_vec2();
            if response.hovered() {
                // The remote pointer is drawn into the image.
                ctx.set_cursor_icon(CursorIcon::None);
            }
            self.handle_input(ui, &response, r, remote, focused);
        }
    }

    fn report_size(&mut self, ctx: &egui::Context, rect: Rect, ppp: f32, scale: u32) {
        let width = (rect.width() * ppp).round().clamp(200.0, 8192.0) as u16;
        let height = (rect.height() * ppp).round().clamp(200.0, 8192.0) as u16;
        let scale = ((ppp * scale as f32).round() as u32).clamp(100, 500);
        let size = (width, height, scale);
        if self.sent_size == Some(size) {
            self.pending_size = None;
            return;
        }
        let now = Instant::now();
        match self.pending_size {
            // Connect at the first size right away; later sizes wait until resizing stops.
            _ if self.sent_size.is_none() => {}
            Some((pending, since)) if pending == size && now - since >= RESIZE_SETTLE => {}
            Some((pending, _)) if pending == size => {
                ctx.request_repaint_after(RESIZE_SETTLE);
                return;
            }
            _ => {
                self.pending_size = Some((size, now));
                ctx.request_repaint_after(RESIZE_SETTLE);
                return;
            }
        }
        self.sent_size = Some(size);
        self.pending_size = None;
        self.session.send(Input::Resize { width, height, scale });
    }

    fn update_texture(&mut self, ctx: &egui::Context) {
        let mut shared = self.session.shared();
        let f = &mut shared.frame;
        if f.width == 0 || f.height == 0 {
            return;
        }
        let options = TextureOptions::LINEAR;
        match &mut self.texture {
            Some(t) if t.size() == [f.width, f.height] => {
                if let Some([x, y, w, h]) = f.dirty.take() {
                    let mut region = Vec::with_capacity(w * h * 4);
                    for row in y..y + h {
                        let start = (row * f.width + x) * 4;
                        region.extend_from_slice(&f.pixels[start..start + w * 4]);
                    }
                    t.set_partial([x, y], ColorImage::from_rgba_unmultiplied([w, h], &region), options);
                }
            }
            _ => {
                let image = ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.pixels);
                self.texture = Some(ctx.load_texture("rdp-desktop", image, options));
                f.dirty = None;
            }
        }
    }

    fn handle_input(&mut self, ui: &egui::Ui, response: &egui::Response, image: Rect, remote: Vec2, focused: bool) {
        let to_remote = |pos: Pos2| -> MousePosition {
            let rel = (pos - image.min) / image.size();
            MousePosition {
                x: (rel.x * remote.x).clamp(0.0, remote.x - 1.0) as u16,
                y: (rel.y * remote.y).clamp(0.0, remote.y - 1.0) as u16,
            }
        };
        let hovered = response.hovered();
        let mut ops: Vec<Operation> = Vec::new();
        let mut keys: Vec<FastPathInputEvent> = Vec::new();

        for event in ui.input(|i| i.events.clone()) {
            match event {
                egui::Event::PointerMoved(pos) if hovered || self.buttons_down != 0 => {
                    ops.push(Operation::MouseMove(to_remote(pos)));
                }
                egui::Event::PointerButton { pos, button, pressed, .. } => {
                    let Some((bit, b)) = mouse_button(button) else { continue };
                    if pressed && !image.contains(pos) {
                        continue;
                    }
                    if !pressed && self.buttons_down & bit == 0 {
                        continue;
                    }
                    ops.push(Operation::MouseMove(to_remote(pos)));
                    if pressed {
                        self.buttons_down |= bit;
                        ops.push(Operation::MouseButtonPressed(b));
                    } else {
                        self.buttons_down &= !bit;
                        ops.push(Operation::MouseButtonReleased(b));
                    }
                }
                egui::Event::MouseWheel { unit, delta, .. } if hovered => {
                    let notches = match unit {
                        MouseWheelUnit::Line => delta,
                        MouseWheelUnit::Page => delta * 3.0,
                        MouseWheelUnit::Point => delta / 50.0,
                    };
                    // egui moves content right for positive x; RDP scrolls right for positive units.
                    self.wheel += Vec2::new(-notches.x, notches.y) * 120.0;
                    for (vertical, amount) in [(true, &mut self.wheel.y), (false, &mut self.wheel.x)] {
                        let units = amount.trunc().clamp(-255.0, 255.0);
                        if units != 0.0 {
                            *amount -= units;
                            ops.push(Operation::WheelRotations(WheelRotations {
                                is_vertical: vertical,
                                rotation_units: units as i16,
                            }));
                        }
                    }
                }
                egui::Event::Key { key, physical_key, pressed, .. } if focused => {
                    if let Some(code) = scancode(physical_key.unwrap_or(key)) {
                        keys.push(self.key(code, pressed));
                    }
                }
                // egui turns Ctrl+C, Ctrl+X and Ctrl+V into clipboard events; Ctrl is already
                // held on the remote side, so press the letter. Its release arrives as a key event.
                egui::Event::Copy if focused => keys.push(self.key((false, 0x2E), true)),
                egui::Event::Cut if focused => keys.push(self.key((false, 0x2D), true)),
                egui::Event::Paste(text) if focused => {
                    self.session.send(Input::LocalClipboard(text));
                    keys.push(self.key((false, 0x2F), true));
                }
                _ => {}
            }
        }

        let mut events: Vec<FastPathInputEvent> = self.mouse.apply(ops).into_iter().collect();
        events.extend(keys);
        if !events.is_empty() {
            self.session.send(Input::Events(events));
        }
    }

    fn key(&mut self, code: (bool, u8), pressed: bool) -> FastPathInputEvent {
        if pressed {
            self.keys_down.insert(code);
        } else {
            self.keys_down.remove(&code);
        }
        key_event(code, pressed)
    }

    /// Release everything held on the remote side, so no key stays stuck after focus moves.
    fn release_keys(&mut self) {
        let mut events: Vec<FastPathInputEvent> = std::mem::take(&mut self.keys_down)
            .into_iter()
            .map(|code| key_event(code, false))
            .collect();
        events.extend(self.mouse.release_all());
        self.buttons_down = 0;
        if !events.is_empty() {
            self.session.send(Input::Events(events));
        }
    }
}

/// The largest rect with the image's aspect ratio that fits in `outer`, centered and aligned
/// to physical pixels.
fn fit(outer: Rect, image: Vec2, ppp: f32) -> Rect {
    let s = (outer.width() / image.x).min(outer.height() / image.y);
    let size = image * s;
    let min = outer.center() - size / 2.0;
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_size(Pos2::new(snap(min.x), snap(min.y)), size)
}

fn mouse_button(b: PointerButton) -> Option<(u8, MouseButton)> {
    Some(match b {
        PointerButton::Primary => (1, MouseButton::Left),
        PointerButton::Secondary => (2, MouseButton::Right),
        PointerButton::Middle => (4, MouseButton::Middle),
        PointerButton::Extra1 => (8, MouseButton::X1),
        PointerButton::Extra2 => (16, MouseButton::X2),
    })
}

fn key_event((extended, code): (bool, u8), pressed: bool) -> FastPathInputEvent {
    let mut flags = KeyboardFlags::empty();
    if extended {
        flags |= KeyboardFlags::EXTENDED;
    }
    if !pressed {
        flags |= KeyboardFlags::RELEASE;
    }
    FastPathInputEvent::KeyboardEvent(flags, code)
}

/// PC/AT scan code set 1 for a physical key, as (extended, code). The remote desktop applies
/// its own keyboard layout, as with any RDP client.
fn scancode(key: Key) -> Option<(bool, u8)> {
    use Key::*;
    let normal = |c| Some((false, c));
    let extended = |c| Some((true, c));
    match key {
        Escape => normal(0x01),
        Num1 => normal(0x02),
        Num2 => normal(0x03),
        Num3 => normal(0x04),
        Num4 => normal(0x05),
        Num5 => normal(0x06),
        Num6 => normal(0x07),
        Num7 => normal(0x08),
        Num8 => normal(0x09),
        Num9 => normal(0x0A),
        Num0 => normal(0x0B),
        Minus => normal(0x0C),
        Equals | Plus => normal(0x0D),
        Backspace => normal(0x0E),
        Tab => normal(0x0F),
        Q => normal(0x10),
        W => normal(0x11),
        E => normal(0x12),
        R => normal(0x13),
        T => normal(0x14),
        Y => normal(0x15),
        U => normal(0x16),
        I => normal(0x17),
        O => normal(0x18),
        P => normal(0x19),
        OpenBracket | OpenCurlyBracket => normal(0x1A),
        CloseBracket | CloseCurlyBracket => normal(0x1B),
        Enter => normal(0x1C),
        ControlLeft => normal(0x1D),
        A => normal(0x1E),
        S => normal(0x1F),
        D => normal(0x20),
        F => normal(0x21),
        G => normal(0x22),
        H => normal(0x23),
        J => normal(0x24),
        K => normal(0x25),
        L => normal(0x26),
        Semicolon | Colon => normal(0x27),
        Quote => normal(0x28),
        Backtick => normal(0x29),
        ShiftLeft => normal(0x2A),
        Backslash | Pipe => normal(0x2B),
        Z => normal(0x2C),
        X => normal(0x2D),
        C => normal(0x2E),
        V => normal(0x2F),
        B => normal(0x30),
        N => normal(0x31),
        M => normal(0x32),
        Comma => normal(0x33),
        Period => normal(0x34),
        Slash | Questionmark => normal(0x35),
        ShiftRight => normal(0x36),
        AltLeft => normal(0x38),
        Space => normal(0x39),
        F1 => normal(0x3B),
        F2 => normal(0x3C),
        F3 => normal(0x3D),
        F4 => normal(0x3E),
        F5 => normal(0x3F),
        F6 => normal(0x40),
        F7 => normal(0x41),
        F8 => normal(0x42),
        F9 => normal(0x43),
        F10 => normal(0x44),
        IntlBackslash => normal(0x56),
        F11 => normal(0x57),
        F12 => normal(0x58),
        ControlRight => extended(0x1D),
        AltRight => extended(0x38),
        Home => extended(0x47),
        ArrowUp => extended(0x48),
        PageUp => extended(0x49),
        ArrowLeft => extended(0x4B),
        ArrowRight => extended(0x4D),
        End => extended(0x4F),
        ArrowDown => extended(0x50),
        PageDown => extended(0x51),
        Insert => extended(0x52),
        Delete => extended(0x53),
        SuperLeft => extended(0x5B),
        SuperRight => extended(0x5C),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_follow_set_1() {
        assert_eq!(scancode(Key::A), Some((false, 0x1E)));
        assert_eq!(scancode(Key::Enter), Some((false, 0x1C)));
        assert_eq!(scancode(Key::ArrowLeft), Some((true, 0x4B)));
        assert_eq!(scancode(Key::ControlRight), Some((true, 0x1D)));
        assert_eq!(scancode(Key::F12), Some((false, 0x58)));
        assert_eq!(scancode(Key::F13), None);
    }

    #[test]
    fn release_sets_the_release_flag() {
        assert_eq!(key_event((true, 0x53), false), FastPathInputEvent::KeyboardEvent(KeyboardFlags::EXTENDED | KeyboardFlags::RELEASE, 0x53));
        assert_eq!(key_event((false, 0x1E), true), FastPathInputEvent::KeyboardEvent(KeyboardFlags::empty(), 0x1E));
    }

    /// Drives the widget headlessly against a real server:
    /// `HOSTDECK_RDP_TEST=host:port:user:password cargo test -- --ignored live_view`
    #[test]
    #[ignore]
    fn live_view() {
        let target = std::env::var("HOSTDECK_RDP_TEST").expect("set HOSTDECK_RDP_TEST=host:port:user:password");
        let parts: Vec<&str> = target.splitn(4, ':').collect();
        let dir = std::env::temp_dir().join(format!("hostdeck-test-{}", uuid::Uuid::new_v4()));
        let ctx = egui::Context::default();
        let params = crate::rdp::Params {
            host: parts[0].into(),
            port: parts[1].parse().unwrap(),
            username: parts[2].into(),
            domain: String::new(),
            password: parts[3].into(),
            clipboard: true,
            cert_policy: crate::model::CertPolicy::Tofu,
            known_hosts: dir.join("rdp_known_hosts"),
        };
        let mut view = RdpView::new(RdpSession::spawn(params, ctx.clone()).unwrap());
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));
        let frame = |view: &mut RdpView, events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            // A renderer would upload these; the test only needs them consumed.
            ctx.run_ui(input, |ui| view.ui(ui, 100, Color32::BLACK)).textures_delta.clear();
        };

        let start = Instant::now();
        while !(view.phase() == Phase::Connected && view.texture.is_some()) {
            assert!(start.elapsed() < Duration::from_secs(20), "{:?}", view.log_lines());
            frame(&mut view, Vec::new());
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(view.sent_size.map(|s| (s.0, s.1)), Some((1200, 800)));
        println!("texture {:?}", view.texture.as_ref().unwrap().size());

        // Let focus settle, then type and move the mouse through egui events.
        for _ in 0..3 {
            frame(&mut view, Vec::new());
        }
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut view, vec![key(Key::ShiftLeft, true), key(Key::H, true), key(Key::H, false), key(Key::ShiftLeft, false)]);
        frame(&mut view, vec![egui::Event::PointerMoved(Pos2::new(600.0, 400.0))]);
        frame(&mut view, vec![egui::Event::Paste("pasted text".into())]);
        frame(&mut view, vec![key(Key::V, false)]);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(view.phase(), Phase::Connected, "{:?}", view.log_lines());
        assert!(view.keys_down.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn image_fits_and_keeps_its_shape() {
        let outer = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0));
        let r = fit(outer, Vec2::new(800.0, 600.0), 1.0);
        assert!((r.height() - 500.0).abs() < 0.01);
        assert!((r.width() / r.height() - 800.0 / 600.0).abs() < 0.01);
        assert!((r.center().x - 500.0).abs() < 1.0);
        // Same size as the view: drawn 1:1.
        assert_eq!(fit(outer, Vec2::new(1000.0, 500.0), 1.0), outer);
    }
}
