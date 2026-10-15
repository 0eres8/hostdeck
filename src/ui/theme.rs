//! Fonts, colors and widget styling.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    TextStyle, Visuals,
};

use crate::model::ThemeChoice;
use crate::terminal::{TermTheme, MONO, MONO_BOLD};

pub const SEMIBOLD: &str = "semibold";

/// Session accent colors, chosen to stay readable on both themes.
pub const SESSION_COLORS: [(&str, Color32); 9] = [
    ("Blue", Color32::from_rgb(0x5b, 0x8d, 0xef)),
    ("Violet", Color32::from_rgb(0x9b, 0x7b, 0xe8)),
    ("Teal", Color32::from_rgb(0x2b, 0xb3, 0xa8)),
    ("Green", Color32::from_rgb(0x5a, 0xb5, 0x5e)),
    ("Amber", Color32::from_rgb(0xe0, 0xa8, 0x2e)),
    ("Orange", Color32::from_rgb(0xec, 0x7c, 0x3a)),
    ("Red", Color32::from_rgb(0xe5, 0x53, 0x5f)),
    ("Pink", Color32::from_rgb(0xe0, 0x6c, 0xb0)),
    ("Slate", Color32::from_rgb(0x8a, 0x94, 0xa6)),
];

pub fn session_color(i: usize) -> Color32 {
    SESSION_COLORS[i % SESSION_COLORS.len()].1
}

#[derive(Clone)]
pub struct Palette {
    pub accent: Color32,
    pub bg: Color32,
    pub surface: Color32,
    pub surface_2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub danger: Color32,
    pub success: Color32,
    pub terminal: TermTheme,
}

impl Palette {
    pub fn new(theme: ThemeChoice) -> Self {
        match theme {
            ThemeChoice::Dark => Self {
                accent: Color32::from_rgb(0x6e, 0x9b, 0xff),
                bg: Color32::from_rgb(0x12, 0x15, 0x1c),
                surface: Color32::from_rgb(0x18, 0x1c, 0x24),
                surface_2: Color32::from_rgb(0x20, 0x25, 0x30),
                border: Color32::from_rgb(0x2a, 0x30, 0x3c),
                text: Color32::from_rgb(0xe3, 0xe6, 0xec),
                muted: Color32::from_rgb(0x8b, 0x93, 0xa3),
                hover: Color32::from_rgb(0x24, 0x2a, 0x36),
                selected: Color32::from_rgb(0x26, 0x34, 0x52),
                danger: Color32::from_rgb(0xf0, 0x6a, 0x74),
                success: Color32::from_rgb(0x6c, 0xc8, 0x7a),
                terminal: TermTheme::dark(),
            },
            ThemeChoice::Light => Self {
                accent: Color32::from_rgb(0x2e, 0x5b, 0xe8),
                bg: Color32::from_rgb(0xfb, 0xfb, 0xfd),
                surface: Color32::from_rgb(0xf3, 0xf4, 0xf7),
                surface_2: Color32::from_rgb(0xe9, 0xeb, 0xf0),
                border: Color32::from_rgb(0xdc, 0xdf, 0xe6),
                text: Color32::from_rgb(0x1f, 0x23, 0x2b),
                muted: Color32::from_rgb(0x6b, 0x72, 0x80),
                hover: Color32::from_rgb(0xe6, 0xe9, 0xf0),
                selected: Color32::from_rgb(0xd9, 0xe3, 0xfc),
                danger: Color32::from_rgb(0xc8, 0x36, 0x4b),
                success: Color32::from_rgb(0x2f, 0x8a, 0x45),
                terminal: TermTheme::light(),
            },
        }
    }
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.into(), FontData::from_static(bytes).into());
    };
    add(&mut fonts, "inter", include_bytes!("../../assets/fonts/Inter-Regular.otf"));
    add(&mut fonts, "inter-semibold", include_bytes!("../../assets/fonts/Inter-SemiBold.otf"));
    add(&mut fonts, "jbmono", include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"));
    add(&mut fonts, "jbmono-bold", include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf"));

    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let with = |first: &str| -> Vec<String> {
        std::iter::once(first.to_string()).chain(defaults.iter().cloned()).collect()
    };
    fonts.families.insert(FontFamily::Proportional, with("inter"));
    fonts.families.insert(FontFamily::Monospace, with("jbmono"));
    fonts.families.insert(FontFamily::Name(MONO.into()), with("jbmono"));
    fonts.families.insert(FontFamily::Name(MONO_BOLD.into()), with("jbmono-bold"));
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), with("inter-semibold"));
    // CJK text falls back to a system font when one is installed (Ubuntu desktops ship Noto CJK).
    let cjk = [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    ]
    .iter()
    .find_map(|p| std::fs::read(p).ok());
    if let Some(bytes) = cjk {
        fonts.font_data.insert("cjk".into(), FontData::from_owned(bytes).into());
        for family in fonts.families.values_mut() {
            family.push("cjk".into());
        }
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // The bundled Inter has its private-use glyphs removed, so Phosphor icons (which live
    // in that range) resolve to the icon font even though Inter comes first.
    if let Some(f) = fonts.families.get_mut(&FontFamily::Name(SEMIBOLD.into())) {
        f.insert(1, "phosphor".into());
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, theme: ThemeChoice) -> Palette {
    let p = Palette::new(theme);
    let (mut visuals, egui_theme) = match theme {
        ThemeChoice::Dark => (Visuals::dark(), egui::Theme::Dark),
        ThemeChoice::Light => (Visuals::light(), egui::Theme::Light),
    };
    ctx.set_theme(egui_theme);
    let r = CornerRadius::same(6);
    visuals.panel_fill = p.surface;
    visuals.window_fill = p.surface;
    visuals.extreme_bg_color = p.bg;
    visuals.faint_bg_color = p.surface_2;
    visuals.code_bg_color = p.surface_2;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(90) };
    visuals.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(60) };
    visuals.hyperlink_color = p.accent;
    visuals.selection.bg_fill = p.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, p.accent);
    visuals.override_text_color = Some(p.text);

    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    for (state, fill) in [(&mut w.inactive, p.surface_2), (&mut w.hovered, p.hover), (&mut w.active, p.selected)] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = r;
        state.fg_stroke = Stroke::new(1.0, p.text);
    }
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.bg_fill = p.hover;
    w.open.weak_bg_fill = p.hover;
    w.open.corner_radius = r;
    w.noninteractive.corner_radius = r;

    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.window_margin = Margin::same(18);
        style.spacing.menu_margin = Margin::same(6);
        style.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Name(SEMIBOLD.into()))),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
    p
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
