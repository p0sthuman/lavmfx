//! The app's look in one place (M4b): palette, spacing, radii, text sizes,
//! applied to egui and egui_dock. Panels and widgets take colours from
//! [`pal`] — no colour literals elsewhere (group colours are data).
//!
//! Dark: near-black ground, grey section bands, one mint
//! accent for everything active (selection, playing, animated, on).

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle};

/// Named colours of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Behind panels (dock ground, tab bar, grid background).
    pub ground: Color32,
    /// Panel body.
    pub panel: Color32,
    /// Section header band ("▼ Transport").
    pub band: Color32,
    /// Inputs, slider rails, empty slots.
    pub field: Color32,
    pub button: Color32,
    pub button_hover: Color32,
    pub button_active: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub text_strong: Color32,
    /// The accent (mint): selected, playing, animated, toggled on.
    pub accent: Color32,
    /// Quieter accent (playing slot strip, container fill).
    pub accent_dim: Color32,
    /// Text/icons on the accent.
    pub on_accent: Color32,
    /// Opening / pending (amber).
    pub pending: Color32,
    /// Audio (the pink A fader knob).
    pub audio: Color32,
    /// Bypassed / missing / errors.
    pub danger: Color32,
    /// Drop targets and insert lines.
    pub drop: Color32,
    /// Overlay boxes over video (labels on monitors, drag hints).
    pub overlay: Color32,
    pub overlay_text: Color32,
    /// Video area with nothing to show.
    pub video_empty: Color32,
}

pub const DARK: Palette = Palette {
    ground: Color32::from_gray(8),
    panel: Color32::from_gray(17),
    band: Color32::from_gray(38),
    field: Color32::from_gray(27),
    button: Color32::from_gray(46),
    button_hover: Color32::from_gray(62),
    button_active: Color32::from_gray(78),
    border: Color32::from_gray(34),
    text: Color32::from_gray(200),
    text_weak: Color32::from_gray(118),
    text_strong: Color32::from_gray(240),
    accent: Color32::from_rgb(98, 226, 186),
    accent_dim: Color32::from_rgb(28, 70, 60),
    on_accent: Color32::from_gray(10),
    pending: Color32::from_rgb(232, 176, 52),
    audio: Color32::from_rgb(214, 98, 150),
    danger: Color32::from_rgb(206, 78, 62),
    drop: Color32::from_rgb(96, 170, 255),
    overlay: Color32::from_black_alpha(190),
    overlay_text: Color32::from_gray(230),
    video_empty: Color32::from_gray(10),
};

pub const LIGHT: Palette = Palette {
    ground: Color32::from_gray(196),
    panel: Color32::from_gray(232),
    band: Color32::from_gray(212),
    field: Color32::from_gray(248),
    button: Color32::from_gray(214),
    button_hover: Color32::from_gray(200),
    button_active: Color32::from_gray(184),
    border: Color32::from_gray(190),
    text: Color32::from_gray(40),
    text_weak: Color32::from_gray(110),
    text_strong: Color32::from_gray(10),
    accent: Color32::from_rgb(22, 160, 124),
    accent_dim: Color32::from_rgb(196, 232, 220),
    on_accent: Color32::WHITE,
    pending: Color32::from_rgb(196, 130, 10),
    audio: Color32::from_rgb(196, 70, 128),
    danger: Color32::from_rgb(190, 56, 44),
    drop: Color32::from_rgb(30, 110, 220),
    overlay: Color32::from_black_alpha(170),
    overlay_text: Color32::from_gray(240),
    video_empty: Color32::from_gray(20),
};

thread_local! {
    /// Palette set by the last [`apply`] (the UI runs on one thread).
    static CURRENT: std::cell::Cell<Palette> = const { std::cell::Cell::new(DARK) };
}

/// Palette of the current theme, where no `Ui` is at hand (painters).
pub fn cur() -> Palette {
    CURRENT.with(|c| c.get())
}

/// Palette of the current theme.
pub fn pal(ui: &egui::Ui) -> Palette {
    pal_ctx(ui.ctx())
}

pub fn pal_ctx(ctx: &egui::Context) -> Palette {
    if ctx.global_style().visuals.dark_mode {
        DARK
    } else {
        LIGHT
    }
}

/// Corner radius of buttons, fields and boxes (nearly square).
pub const RADIUS: u8 = 2;
/// Height of one param row / button.
pub const ROW_H: f32 = 22.0;

/// User look (Preferences › Appearance): base theme, colour overrides, UI scale.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub light: bool,
    pub accent: Option<[u8; 3]>,
    pub ground: Option<[u8; 3]>,
    pub panel: Option<[u8; 3]>,
    pub band: Option<[u8; 3]>,
    pub text: Option<[u8; 3]>,
    /// UI zoom (1 = 100 %).
    pub scale: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            light: false,
            accent: None,
            ground: None,
            panel: None,
            band: None,
            text: None,
            scale: 1.0,
        }
    }
}

impl Appearance {
    /// Base palette with the overrides applied.
    pub fn palette(&self) -> Palette {
        let mut p = if self.light { LIGHT } else { DARK };
        let c = |v: [u8; 3]| Color32::from_rgb(v[0], v[1], v[2]);
        if let Some(a) = self.accent {
            p.accent = c(a);
            p.accent_dim = p.panel.lerp_to_gamma(p.accent, 0.28);
        }
        if let Some(v) = self.ground {
            p.ground = c(v);
        }
        if let Some(v) = self.panel {
            p.panel = c(v);
        }
        if let Some(v) = self.band {
            p.band = c(v);
        }
        if let Some(v) = self.text {
            p.text = c(v);
            p.text_strong = c(v).lerp_to_gamma(if self.light { Color32::BLACK } else { Color32::WHITE }, 0.4);
        }
        p
    }
}

/// Apply the theme to every egui style.
/// egui's fonts with Hack (its bundled monospace font) added as the last
/// fallback of the normal text font: Ubuntu and the emoji fonts have no
/// arrows (→ ←) or ●, which then showed as empty boxes.
pub fn fonts() -> egui::FontDefinitions {
    let mut f = egui::FontDefinitions::default();
    if let Some(prop) = f.families.get_mut(&egui::FontFamily::Proportional)
        && !prop.iter().any(|n| n == "Hack")
    {
        prop.push("Hack".into());
    }
    f
}

pub fn apply(ctx: &egui::Context, look: &Appearance) {
    let p = look.palette();
    let dark = !look.light;
    ctx.set_zoom_factor(look.scale.clamp(0.6, 2.0));
    CURRENT.with(|c| c.set(p));
    ctx.all_styles_mut(|s| {
        let mut v = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let r = CornerRadius::same(RADIUS);
        v.panel_fill = p.panel;
        v.window_fill = p.panel;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.window_corner_radius = CornerRadius::same(3);
        v.menu_corner_radius = CornerRadius::same(3);
        v.extreme_bg_color = p.field;
        v.text_edit_bg_color = Some(p.field);
        v.faint_bg_color = p.band;
        v.code_bg_color = p.field;
        v.hyperlink_color = p.accent;
        v.warn_fg_color = p.pending;
        v.error_fg_color = p.danger;
        v.selection.bg_fill = p.accent;
        v.selection.stroke = Stroke::new(1.0, p.on_accent);
        v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.3 };
        v.slider_trailing_fill = false;
        v.striped = false;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.panel;
        w.noninteractive.weak_bg_fill = p.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.noninteractive.corner_radius = r;
        for (st, fill) in [
            (&mut w.inactive, p.button),
            (&mut w.hovered, p.button_hover),
            (&mut w.active, p.button_active),
            (&mut w.open, p.button_hover),
        ] {
            st.weak_bg_fill = fill;
            st.corner_radius = r;
            st.expansion = 0.0;
        }
        // `bg_fill` of inactive widgets = slider rails and checkbox boxes.
        w.inactive.bg_fill = p.field;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, p.text);
        w.hovered.bg_fill = p.button_hover;
        w.hovered.bg_stroke = Stroke::new(1.0, p.button_active);
        w.hovered.fg_stroke = Stroke::new(1.5, p.text_strong);
        w.active.bg_fill = p.button_active;
        w.active.bg_stroke = Stroke::new(1.0, p.accent);
        w.active.fg_stroke = Stroke::new(1.5, p.text_strong);
        w.open.bg_fill = p.button_hover;
        w.open.fg_stroke = Stroke::new(1.0, p.text_strong);
        s.visuals = v;
        // Text never spills: it ends in "…" (paragraphs opt in to wrapping).
        s.wrap_mode = Some(egui::TextWrapMode::Truncate);

        let sp = &mut s.spacing;
        sp.item_spacing = egui::vec2(4.0, 3.0);
        sp.button_padding = egui::vec2(5.0, 1.0);
        sp.interact_size.y = ROW_H;
        sp.slider_rail_height = 12.0;
        sp.combo_height = 300.0;
        sp.menu_margin = egui::Margin::same(4);
        sp.indent = 14.0;
        // Solid bars whose handle uses the text colour (the button colour was
        // almost the panel's: the grid's scroll bar was invisible, user).
        let mut sc = egui::style::ScrollStyle::solid();
        sc.bar_width = 10.0;
        sc.handle_min_length = 48.0; // grabbable even with 1000+ columns
        sc.foreground_color = true;
        sc.dormant_handle_opacity = 0.45;
        sc.active_handle_opacity = 0.7;
        sc.interact_handle_opacity = 0.95;
        sc.dormant_background_opacity = 1.0;
        sc.active_background_opacity = 1.0;
        sc.interact_background_opacity = 1.0;
        sp.scroll = sc;

        use egui::FontFamily::{Monospace, Proportional};
        s.text_styles = [
            (TextStyle::Heading, FontId::new(16.0, Proportional)),
            (TextStyle::Body, FontId::new(13.0, Proportional)),
            (TextStyle::Button, FontId::new(12.5, Proportional)),
            (TextStyle::Small, FontId::new(10.5, Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, Monospace)),
        ]
        .into();
    });
}

/// egui_dock look: tabs on the ground colour, active tab = panel, centred
/// panel titles, thin separators.
pub fn dock_style(ui: &egui::Ui) -> egui_dock::Style {
    let p = pal(ui);
    let mut st = egui_dock::Style::from_egui(ui.style());
    st.tab_bar.height = 22.0;
    st.tab_bar.bg_fill = p.ground;
    st.tab_bar.hline_color = p.ground;
    st.tab_bar.corner_radius = CornerRadius::ZERO;
    st.main_surface_border_stroke = Stroke::NONE;
    st.separator.width = 3.0;
    st.separator.color_idle = p.ground;
    st.separator.color_hovered = p.button_active;
    st.separator.color_dragged = p.accent;
    let tab = |bg: Color32, text: Color32| egui_dock::TabInteractionStyle {
        outline_color: Color32::TRANSPARENT,
        corner_radius: CornerRadius::ZERO,
        bg_fill: bg,
        text_color: text,
    };
    st.tab.active = tab(p.panel, p.text_strong);
    st.tab.focused = tab(p.panel, p.text_strong);
    st.tab.hovered = tab(p.band, p.text);
    st.tab.inactive = tab(p.ground, p.text_weak);
    st.tab.active_with_kb_focus = st.tab.active.clone();
    st.tab.focused_with_kb_focus = st.tab.focused.clone();
    st.tab.inactive_with_kb_focus = st.tab.inactive.clone();
    st.tab.hline_below_active_tab_name = false;
    st.tab.tab_body.bg_fill = p.panel;
    st.tab.tab_body.stroke = Stroke::NONE;
    st.tab.tab_body.corner_radius = CornerRadius::ZERO;
    // The add-button slot is our tab menu (`dock::show` draws a chevron on it).
    st.buttons.add_tab_color = Color32::TRANSPARENT;
    st.buttons.add_tab_active_color = Color32::TRANSPARENT;
    st.buttons.add_tab_bg_fill = p.button_hover;
    st.buttons.add_tab_border_color = Color32::TRANSPARENT;
    st.buttons.close_tab_color = p.text_weak;
    st.buttons.close_tab_active_color = p.text_strong;
    st.buttons.close_tab_bg_fill = p.button_hover;
    st.overlay.selection_color = p.accent.gamma_multiply(0.4);
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Symbols the UI uses are drawn by the normal text font, not as boxes.
    #[test]
    fn ui_symbols_have_glyphs() {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts());
        let font = FontId::proportional(14.0);
        let mut out = ctx.run_pass(Default::default(), |ctx| {
            for ch in ['→', '←', '↑', '↓', '↔', '●', '…', '×', '›', '▼', '–'] {
                assert!(ctx.fonts_mut(|f| f.has_glyph(&font, ch)), "no glyph for {ch}");
            }
        });
        // Font atlas uploads nobody draws in a test.
        out.textures_delta.clear();
    }
}
