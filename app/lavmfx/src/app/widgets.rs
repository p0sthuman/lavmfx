//! Reusable widgets: wheel-aware sliders, the parameter row, choice combos.

use eframe::egui::{self, Color32, RichText, vec2};

pub fn choice_combo(ui: &mut egui::Ui, id: &str, hover: &str, p: &mut lavmfx_core::ChoiceParam) -> bool {
    choice_combo_with(ui, id, hover, p, |_| true)
}

/// Combo for a choice param. Options for which `ready` is false (not
/// implemented yet) are shown dimmed with a note. Returns true on change.
pub fn choice_combo_with(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    hover: &str,
    p: &mut lavmfx_core::ChoiceParam,
    ready: impl Fn(&str) -> bool,
) -> bool {
    let mut sel = None;
    let shown = if ready(&p.value) {
        RichText::new(&p.value)
    } else {
        RichText::new(format!("{} *", p.value)).weak()
    };
    // Fixed width (`spacing.combo_width`); long values end in "…" (full text on hover).
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(ui.spacing().combo_width)
        .truncate()
        .height(400.0)
        .show_ui(ui, |ui| {
            for (i, o) in p.options.iter().enumerate() {
                let ok = ready(o);
                let text = if ok { RichText::new(o) } else { RichText::new(o).weak() };
                let mut r = ui.selectable_label(p.index == i as i32, text);
                if !ok {
                    r = r.on_hover_text("Not implemented yet: renders as Alpha");
                }
                if r.clicked() {
                    sel = Some(i);
                }
            }
        })
        .response
        .on_hover_text(hover);
    sel.is_some_and(|i| p.select(i))
}

/// Small on/off button (B, S, …). On = accent; "B" (bypass) on = danger.
pub fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str, hover: &str) {
    toggle_box(ui, value, label, hover, vec2(0.0, 0.0));
}

/// Layer Autopilot buttons: Previous · Off · Next · Random (one lit).
/// Returns true on change.
pub fn autopilot_buttons(ui: &mut egui::Ui, layer: &mut lavmfx_core::Layer, ids: &mut lavmfx_core::IdGen) -> bool {
    use lavmfx_core::autopilot::Step;
    let cur = layer.autopilot_step();
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (label, step, hover) in [
            (
                "◀ Prev",
                Some(Step::Previous),
                "Play the layer's clips in reverse order",
            ),
            ("Off", None, "Autopilot off: you're in command"),
            ("Next ▶", Some(Step::Next), "Play the layer's clips in order"),
            ("Random", Some(Step::Random), "Play the layer's clips in random order"),
        ] {
            let mut on = cur == step;
            if toggle_box(ui, &mut on, label, hover, vec2(52.0, 20.0)).clicked() && cur != step {
                layer.set_autopilot(ids, step);
                changed = true;
            }
        }
    });
    changed
}

/// [`toggle`] with a minimum size (the grid's big B / S squares).
pub fn toggle_box(ui: &mut egui::Ui, value: &mut bool, label: &str, hover: &str, size: egui::Vec2) -> egui::Response {
    let pal = super::theme::pal(ui);
    let (fill, text) = match (*value, label) {
        (false, _) => (pal.button, pal.text),
        (true, "B") => (pal.danger, pal.text_strong),
        (true, _) => (pal.accent, pal.on_accent),
    };
    let text = if size.y > 20.0 {
        RichText::new(label).color(text)
    } else {
        RichText::new(label).small().color(text)
    };
    let b = egui::Button::new(text).fill(fill).min_size(size);
    let r = ui.add(b).on_hover_text(hover);
    if r.clicked() {
        *value = !*value;
    }
    r
}

/// Icons drawn as shapes (the default fonts lack ◀ ⏸ ▶ ✕).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Back,
    Pause,
    Forward,
    /// Previous / next clip (bar + triangle).
    PrevClip,
    NextClip,
    Close,
    Undo,
    Redo,
    /// Pan tool.
    Hand,
    /// Snapping.
    Magnet,
    Grid,
    /// Mask / polygon shapes (the Add menu and Input Mask buttons).
    ShapeRect,
    ShapeEllipse,
    ShapeTriangle,
    ShapeHexagon,
    /// Free-hand points.
    Pen,
    /// Triangles pointing up / down (move up, move down, unfolded).
    Up,
    Down,
}

/// Flat icon button; `on` = accent fill.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, on: bool, size: egui::Vec2, enabled: bool) -> egui::Response {
    let pal = super::theme::pal(ui);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, r) = ui.allocate_exact_size(size, sense);
    let bg = if on {
        pal.accent
    } else if enabled && r.hovered() {
        pal.button_hover
    } else {
        pal.button
    };
    let fg = if on {
        pal.on_accent
    } else if enabled {
        pal.text
    } else {
        pal.text_weak
    };
    let p = ui.painter();
    p.rect_filled(rect, super::theme::RADIUS as f32, bg);
    let c = rect.center();
    let k = (size.x.min(size.y) / 24.0).clamp(0.45, 1.2);
    match icon {
        Icon::Back | Icon::Forward => {
            let d = if icon == Icon::Forward { 1.0 } else { -1.0 };
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - 4.0 * k * d, c.y - 6.0 * k),
                    egui::pos2(c.x + 5.0 * k * d, c.y),
                    egui::pos2(c.x - 4.0 * k * d, c.y + 6.0 * k),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        Icon::PrevClip | Icon::NextClip => {
            let d = if icon == Icon::NextClip { 1.0 } else { -1.0 };
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - 3.5 * k * d, c.y - 5.5 * k),
                    egui::pos2(c.x + 3.0 * k * d, c.y),
                    egui::pos2(c.x - 3.5 * k * d, c.y + 5.5 * k),
                ],
                fg,
                egui::Stroke::NONE,
            ));
            let x = c.x + 3.5 * k * d;
            p.rect_filled(
                egui::Rect::from_center_size(egui::pos2(x + 0.75 * k * d, c.y), vec2(2.0 * k, 11.0 * k)),
                0.0,
                fg,
            );
        }
        Icon::Pause => {
            for dx in [-3.5, 1.5] {
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(c.x + dx * k, c.y - 6.0 * k), vec2(2.5 * k, 12.0 * k)),
                    0.0,
                    fg,
                );
            }
        }
        Icon::Undo | Icon::Redo => {
            // Arc with an arrowhead at its left (undo) / right (redo) end.
            let d = if icon == Icon::Redo { -1.0 } else { 1.0 };
            let st = egui::Stroke::new(1.8, fg);
            let r = 5.5 * k;
            let centre = egui::pos2(c.x, c.y + 2.0 * k);
            let pts: Vec<_> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / 12.0);
                    egui::pos2(centre.x - d * r * a.cos(), centre.y - r * a.sin())
                })
                .collect();
            let tip = pts[0];
            p.add(egui::Shape::line(pts, st));
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(tip.x - 3.5 * k, tip.y - 1.0 * k),
                    egui::pos2(tip.x + 3.5 * k, tip.y - 1.0 * k),
                    egui::pos2(tip.x, tip.y + 4.0 * k),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        Icon::Hand => {
            let st = egui::Stroke::new(1.6 * k.max(0.8), fg);
            for (i, h) in [5.0, 7.0, 7.0, 5.5].iter().enumerate() {
                let x = c.x - 4.5 * k + i as f32 * 3.0 * k;
                p.line_segment([egui::pos2(x, c.y), egui::pos2(x, c.y - h * k)], st);
            }
            p.line_segment(
                [
                    egui::pos2(c.x - 6.0 * k, c.y - 1.0 * k),
                    egui::pos2(c.x - 4.5 * k, c.y + 2.0 * k),
                ],
                st,
            );
            p.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(c.x - 5.5 * k, c.y - 1.0 * k),
                    egui::pos2(c.x + 5.5 * k, c.y + 6.0 * k),
                ),
                2.5 * k,
                fg,
            );
        }
        Icon::Magnet => {
            // Horseshoe: two legs and a half ring at the bottom, tips marked.
            let w = 3.0 * k;
            let r = 5.0 * k;
            let st = egui::Stroke::new(w, fg);
            let base = egui::pos2(c.x, c.y + 0.5 * k);
            let arc: Vec<_> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * (i as f32 / 12.0);
                    egui::pos2(base.x - r * a.cos(), base.y + r * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(arc, st));
            for x in [base.x - r, base.x + r] {
                p.line_segment([egui::pos2(x, base.y), egui::pos2(x, c.y - 4.0 * k)], st);
                p.rect_filled(
                    egui::Rect::from_center_size(egui::pos2(x, c.y - 5.5 * k), vec2(w, 2.0 * k)),
                    0.0,
                    if on { pal.text_strong } else { pal.accent },
                );
            }
        }
        Icon::ShapeRect | Icon::ShapeEllipse | Icon::ShapeTriangle | Icon::ShapeHexagon => {
            let st = egui::Stroke::new(1.5, fg);
            let r = 6.0 * k;
            match icon {
                Icon::ShapeRect => {
                    p.rect_stroke(
                        egui::Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r)),
                        0.0,
                        st,
                        egui::StrokeKind::Middle,
                    );
                }
                Icon::ShapeEllipse => {
                    p.circle_stroke(c, r, st);
                }
                _ => {
                    let n = if icon == Icon::ShapeTriangle { 3 } else { 6 };
                    let pts = (0..n)
                        .map(|i| {
                            let a = (i as f32 * 360.0 / n as f32 - 90.0).to_radians();
                            egui::pos2(
                                c.x + r * a.cos(),
                                c.y + r * 1.05 * a.sin() + if n == 3 { 1.5 * k } else { 0.0 },
                            )
                        })
                        .collect();
                    p.add(egui::Shape::closed_line(pts, st));
                }
            }
        }
        Icon::Pen => {
            // A pencil: shaft from top-right to the tip at bottom-left.
            let st = egui::Stroke::new(2.2 * k.max(0.8), fg);
            let (a, b) = (
                egui::pos2(c.x + 5.0 * k, c.y - 5.0 * k),
                egui::pos2(c.x - 3.0 * k, c.y + 3.0 * k),
            );
            p.line_segment([a, b], st);
            p.add(egui::Shape::convex_polygon(
                vec![
                    b + vec2(-1.5 * k, -1.5 * k),
                    b + vec2(1.5 * k, 1.5 * k),
                    egui::pos2(c.x - 6.0 * k, c.y + 6.0 * k),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        Icon::Grid => {
            let st = egui::Stroke::new(1.2, fg);
            let s = 6.0 * k;
            for i in 0..4 {
                let t = -s + i as f32 * (2.0 * s / 3.0);
                p.line_segment([egui::pos2(c.x + t, c.y - s), egui::pos2(c.x + t, c.y + s)], st);
                p.line_segment([egui::pos2(c.x - s, c.y + t), egui::pos2(c.x + s, c.y + t)], st);
            }
        }
        Icon::Up | Icon::Down => {
            let d = if icon == Icon::Down { 1.0 } else { -1.0 };
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - 6.0 * k, c.y - 4.0 * k * d),
                    egui::pos2(c.x + 6.0 * k, c.y - 4.0 * k * d),
                    egui::pos2(c.x, c.y + 5.0 * k * d),
                ],
                fg,
                egui::Stroke::NONE,
            ));
        }
        Icon::Close => {
            let s = 4.0 * k;
            let st = egui::Stroke::new(1.6, fg);
            p.line_segment([egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)], st);
            p.line_segment([egui::pos2(c.x - s, c.y + s), egui::pos2(c.x + s, c.y - s)], st);
        }
    }
    r
}

/// How the mouse wheel steps a slider.
#[derive(Clone, Copy)]
pub enum Wheel {
    /// 4 % of the range per notch.
    Linear,
    /// 4 % of the log scale per notch (matches `Slider::logarithmic`);
    /// below `smallest` the value snaps to the range minimum.
    Log { smallest: f64 },
}

/// Fraction of the range one wheel notch moves; Shift = fine.
const WHEEL_STEP: f64 = 0.04;
const WHEEL_FINE: f64 = 0.004;
/// Trackpads report points; this many points count as one notch.
const WHEEL_POINTS_PER_NOTCH: f32 = 30.0;

/// `value` moved by `step` (fraction of the slider's travel), clamped to min..=max.
pub fn wheel_step(value: f64, min: f64, max: f64, wheel: Wheel, step: f64) -> f64 {
    match wheel {
        Wheel::Linear => value + step * (max - min),
        Wheel::Log { smallest } => {
            let lo = min.max(smallest);
            let span = (max / lo).ln();
            if value < lo {
                if step > 0.0 { lo } else { min }
            } else {
                let t = (value / lo).ln() / span + step;
                if t < 0.0 { min } else { lo * (t.min(1.0) * span).exp() }
            }
        }
    }
    .clamp(min, max)
}

/// Flat slider (dark rail, thin accent line at the value) that
/// also follows the mouse wheel while hovered: up/right = more. Click or drag
/// sets the value; the wheel is consumed, so panels behind it don't scroll.
/// `changed()` covers wheel changes too. Size: `spacing.slider_width` along
/// the slider, a row high across it.
pub fn wheel_slider(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    wheel: Wheel,
    vertical: bool,
) -> egui::Response {
    wheel_slider_line(ui, value, range, wheel, vertical, None)
}

/// [`wheel_slider`] with its own value-line colour.
pub fn wheel_slider_line(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    wheel: Wheel,
    vertical: bool,
    line_color: Option<Color32>,
) -> egui::Response {
    let len = ui.spacing().slider_width.max(8.0);
    let size = if vertical {
        vec2(14.0, len)
    } else {
        vec2(len, super::theme::ROW_H)
    };
    slider_impl(ui, value, range, wheel, vertical, size, None, line_color)
}

/// Height of a [`cap_fader`]'s knob.
pub const CAP_H: f32 = 16.0;

/// Vertical mixer fader of `size` whose knob (full width, `CAP_H` tall)
/// carries `label` (may be empty) in `color`. Drag / wheel move it like
/// [`wheel_slider`]; a plain click is left to the caller (no value box jump).
pub fn cap_fader(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    size: egui::Vec2,
    label: &str,
    color: Color32,
) -> egui::Response {
    slider_impl(ui, value, range, Wheel::Linear, true, size, Some((label, color)), None)
}

#[allow(clippy::too_many_arguments)]
fn slider_impl(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    wheel: Wheel,
    vertical: bool,
    size: egui::Vec2,
    cap: Option<(&str, Color32)>,
    line_color: Option<Color32>,
) -> egui::Response {
    let (min, max) = (*range.start(), *range.end());
    let (rect, mut r) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    // Value <-> 0..1 along the slider (log ranges like the wheel steps).
    let to_t = |v: f64| match wheel {
        Wheel::Linear => ((v - min) / (max - min).max(1e-12)).clamp(0.0, 1.0),
        Wheel::Log { smallest } => {
            let lo = min.max(smallest);
            if v < lo {
                0.0
            } else {
                ((v / lo).ln() / (max / lo).ln()).clamp(0.0, 1.0)
            }
        }
    };
    let from_t = |t: f64| match wheel {
        Wheel::Linear => min + t * (max - min),
        Wheel::Log { smallest } => {
            let lo = min.max(smallest);
            if t <= 0.0 {
                min
            } else {
                lo * (t * (max / lo).ln()).exp()
            }
        }
    };
    wheel_zone(ui, rect);
    // Drag = relative (no jump to the pointer; Shift = fine), like the drive's
    // range handles. Sub-pixel progress is kept in temp memory.
    if r.enabled() && r.dragged() {
        let d = r.drag_delta();
        let len = if vertical {
            rect.height() - if cap.is_some() { CAP_H } else { 0.0 }
        } else {
            rect.width()
        }
        .max(1.0);
        let mut dt = (if vertical { -d.y } else { d.x } / len) as f64;
        if ui.input(|i| i.modifiers.shift) {
            dt *= 0.1;
        }
        let key = r.id.with("drag-t");
        let t0: f64 = if r.drag_started() {
            to_t(*value)
        } else {
            ui.data(|m| m.get_temp(key)).unwrap_or_else(|| to_t(*value))
        };
        let t = (t0 + dt).clamp(0.0, 1.0);
        ui.data_mut(|m| m.insert_temp(key, t));
        let nv = from_t(t).clamp(min, max);
        if nv != *value {
            *value = nv;
            r.mark_changed();
        }
    }
    // Click (no drag): the pointer jumps to the playhead and a value box opens.
    if r.enabled() && r.clicked() && cap.is_none() {
        let t = to_t(*value) as f32;
        let at = if vertical {
            egui::pos2(rect.center().x, rect.bottom() - t * rect.height())
        } else {
            egui::pos2(rect.left() + t * rect.width(), rect.center().y)
        };
        warp_pointer(ui.ctx(), at);
    }
    if slider_value_popup(&r, value, min, max) {
        r.mark_changed();
    }
    if let Some((label, color)) = cap.filter(|_| ui.is_rect_visible(rect)) {
        let pal = super::theme::pal(ui);
        let t = to_t(*value) as f32;
        let travel = rect.height() - CAP_H;
        let rail = egui::Rect::from_center_size(rect.center(), vec2(3.0, travel.max(0.0)));
        ui.painter().rect_filled(rail, 1.0, pal.field);
        let top = rect.top() + (1.0 - t) * travel;
        let knob = egui::Rect::from_min_size(egui::pos2(rect.left(), top), vec2(rect.width(), CAP_H));
        let fill = if r.hovered() || r.dragged() {
            color.lerp_to_gamma(Color32::WHITE, 0.15)
        } else {
            color
        };
        ui.painter().rect_filled(knob, 2.0, fill);
        if !label.is_empty() {
            let text = if color == pal.button { pal.text } else { pal.on_accent };
            ui.painter().text(
                knob.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(11.0),
                text,
            );
        }
    } else if ui.is_rect_visible(rect) {
        let pal = super::theme::pal(ui);
        let rail = if vertical {
            rect.shrink2(vec2(2.0, 0.0))
        } else {
            rect.shrink2(vec2(0.0, 3.0))
        };
        let fill = if r.hovered() || r.dragged() {
            pal.button
        } else {
            pal.field
        };
        ui.painter().rect_filled(rail, super::theme::RADIUS as f32, fill);
        let t = to_t(*value) as f32;
        let col = if r.enabled() {
            line_color.unwrap_or(pal.accent)
        } else {
            pal.text_weak
        };
        let line = if vertical {
            let y = rail.bottom() - t * rail.height();
            egui::Rect::from_min_max(egui::pos2(rail.left(), y - 1.5), egui::pos2(rail.right(), y + 1.5))
        } else {
            let x = rail.left() + t * rail.width();
            egui::Rect::from_min_max(egui::pos2(x - 1.5, rail.top()), egui::pos2(x + 1.5, rail.bottom()))
        };
        ui.painter().rect_filled(line.intersect(rail.expand(1.5)), 0.0, col);
    }
    if !r.contains_pointer() || !r.enabled() || wheel_latched(ui.ctx()) {
        return r;
    }
    let (notches, fine) = take_wheel(ui);
    if notches == 0.0 {
        return r;
    }
    let step = notches * if fine { WHEEL_FINE } else { WHEEL_STEP };
    let new = wheel_step(*value, min, max, wheel, step);
    if new != *value {
        *value = new;
        r.mark_changed();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use lavmfx_core::{DisplayUnits, ParameterView, RangeParam};

    #[test]
    fn values_format_and_parse() {
        let p = RangeParam::new(0, 0.0, 1.0, 0.5).with_view(ParameterView::units(DisplayUnits::Percent));
        assert_eq!(fmt_value(&p), "50 %");
        assert_eq!(parse_value(&p, "25 %"), Some(0.25));
        let s = RangeParam::new(0, 0.0, 10.0, 1.2).with_view(ParameterView::units(DisplayUnits::Seconds));
        assert_eq!(fmt_value(&s), "1.20 s");
        assert_eq!(parse_value(&s, "2,5"), Some(2.5));
        assert!((button_step(&p, false) - 0.01).abs() < 1e-12);
    }

    /// Headless frames over a scroll area holding one slider at x 100..300.
    struct Harness {
        ctx: egui::Context,
        t: f64,
        value: f64,
        offset: f32,
        cmds: Vec<egui::ViewportCommand>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                t: 0.0,
                value: 0.5,
                offset: 0.0,
                cmds: Vec::new(),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.t += 1.0 / 60.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0))),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let value = &mut self.value;
            let mut offset = 0.0;
            let mut out = self.ctx.run_ui(input, |ui| {
                guard_wheel(ui.ctx());
                let o = egui::ScrollArea::vertical().id_salt("panel").show(ui, |ui| {
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        ui.add_space(100.0 - ui.cursor().left());
                        ui.spacing_mut().slider_width = 200.0;
                        wheel_slider(ui, value, 0.0..=1.0, Wheel::Linear, false);
                    });
                    ui.add_space(2000.0); // tall panel: scrollable
                });
                offset = o.state.clamped_offset().y;
            });
            out.textures_delta.clear();
            self.offset = offset;
            self.cmds
                .extend(out.viewport_output.values().flat_map(|v| v.commands.clone()));
        }

        fn pointer(&mut self, x: f32, pressed: Option<bool>) {
            let pos = egui::pos2(x, 31.0);
            let mut ev = vec![egui::Event::PointerMoved(pos)];
            if let Some(p) = pressed {
                ev.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: p,
                    modifiers: Default::default(),
                });
            }
            self.frame(ev);
        }
    }

    #[test]
    fn slider_click_opens_box_without_jumping() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.pointer(260.0, None);
        h.pointer(260.0, Some(true));
        h.pointer(260.0, Some(false));
        h.frame(vec![]);
        assert_eq!(h.value, 0.5, "a click doesn't jump to the pointer");
        assert!(
            h.cmds
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::CursorPosition(p) if (p.x - 200.0).abs() < 3.0)),
            "pointer sent to the playhead (x 200): {:?}",
            h.cmds
        );
    }

    #[test]
    fn slider_drag_is_relative() {
        let mut h = Harness::new();
        h.frame(vec![]);
        // Press far left of the playhead, drag 40 px right: +0.2, no jump to 0.1.
        h.pointer(120.0, None);
        h.pointer(120.0, Some(true));
        for x in [130.0, 140.0, 150.0, 160.0] {
            h.pointer(x, None);
        }
        h.pointer(160.0, Some(false));
        assert!((h.value - 0.7).abs() < 0.03, "relative drag: {}", h.value);
    }

    /// Knobs take the wheel too (they read the raw events: the guard zeroes the
    /// smoothed delta over them, which knobs used to read, so they never turned).
    /// The ✕ next to a search box clears it; a folded category hides its items
    /// but opens while searching.
    #[test]
    fn search_clear_button_and_categories() {
        let ctx = egui::Context::default();
        let mut text = String::from("glow");
        let mut clear_at = None;
        let mut shown = Vec::new();
        let frame = |events: Vec<egui::Event>,
                     f: u32,
                     text: &mut String,
                     clear_at: &mut Option<egui::Pos2>,
                     shown: &mut Vec<bool>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 200.0))),
                time: Some(f64::from(f) / 60.0),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let r = search_field(ui, text, "Search");
                // The clear button sits right of the box.
                *clear_at = Some(egui::pos2(
                    r.rect.right() + ROW_H * 0.5 + ui.spacing().item_spacing.x,
                    r.rect.center().y,
                ));
                let searching = !text.is_empty();
                shown.push(category(ui, "t", "CAT", searching, |_| ()).is_some());
            });
            out.textures_delta.clear();
        };
        frame(vec![], 0, &mut text, &mut clear_at, &mut shown);
        let at = clear_at.unwrap();
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(
            vec![egui::Event::PointerMoved(at)],
            1,
            &mut text,
            &mut clear_at,
            &mut shown,
        );
        frame(vec![click(true)], 2, &mut text, &mut clear_at, &mut shown);
        frame(vec![click(false)], 3, &mut text, &mut clear_at, &mut shown);
        assert!(text.is_empty(), "cleared: {text:?}");
        assert!(shown.iter().all(|s| *s), "category open by default");
    }

    #[test]
    fn wheel_turns_a_knob() {
        let ctx = egui::Context::default();
        let mut p = lavmfx_core::RangeParam::new(1, 0.0, 1.0, 0.5);
        let mut offset = 0.0;
        for f in 0..40 {
            let mut events = vec![egui::Event::PointerMoved(egui::pos2(30.0, 40.0))];
            if (5..8).contains(&f) {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: vec2(0.0, 1.0),
                    modifiers: Default::default(),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Wheel,
                });
            }
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0))),
                time: Some(f as f64 / 60.0),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                guard_wheel(ui.ctx());
                let o = egui::ScrollArea::vertical().id_salt("panel").show(ui, |ui| {
                    ui.add_space(20.0);
                    knob(ui, "K", &mut p);
                    ui.add_space(2000.0);
                });
                offset = o.state.clamped_offset().y;
            });
            out.textures_delta.clear();
        }
        assert!(p.value > 0.5 + 2.0 * WHEEL_STEP, "knob turned: {}", p.value);
        assert_eq!(offset, 0.0, "panel kept still");
    }

    #[test]
    fn wheel_over_slider_does_not_scroll_the_panel() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.pointer(200.0, None);
        let wheel = |d: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, d),
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        };
        for _ in 0..3 {
            h.frame(vec![egui::Event::PointerMoved(egui::pos2(200.0, 31.0)), wheel(-1.0)]);
        }
        for _ in 0..30 {
            h.frame(vec![egui::Event::PointerMoved(egui::pos2(200.0, 31.0))]);
        }
        assert_eq!(h.offset, 0.0, "panel kept still");
        assert!(h.value < 0.5, "the slider took the wheel: {}", h.value);
        // Off the slider the panel scrolls as usual.
        for _ in 0..3 {
            h.frame(vec![egui::Event::PointerMoved(egui::pos2(200.0, 150.0)), wheel(-1.0)]);
        }
        for _ in 0..30 {
            h.frame(vec![egui::Event::PointerMoved(egui::pos2(200.0, 150.0))]);
        }
        assert!(h.offset > 0.0, "panel scrolls elsewhere");
    }

    /// A panel that is scrolling keeps the wheel when a slider passes under the
    /// pointer; after a pause the wheel goes to the slider again.
    #[test]
    fn panel_scroll_latches_over_sliders() {
        let mut h = Harness::new();
        let wheel = |d: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, d),
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        };
        let at = |y: f32| egui::Event::PointerMoved(egui::pos2(200.0, y));
        // Scroll down off the slider, then pause.
        for _ in 0..6 {
            h.frame(vec![at(31.0 + 120.0), wheel(-1.0)]);
        }
        for _ in 0..60 {
            h.frame(vec![at(150.0)]);
        }
        let down = h.offset;
        assert!(down > 40.0, "scrolled down: {down}");
        // Scroll back up with the pointer where the slider lands: the panel keeps going.
        for _ in 0..30 {
            h.frame(vec![at(31.0), wheel(1.0)]);
        }
        for _ in 0..60 {
            h.frame(vec![at(31.0)]);
        }
        assert_eq!(h.offset, 0.0, "panel scrolled back to the top");
        assert_eq!(h.value, 0.5, "the slider was passed over, not changed");
        // A new wheel turn after the pause goes to the slider.
        h.frame(vec![at(31.0), wheel(-1.0)]);
        assert!(h.value < 0.5, "slider takes the wheel after a pause: {}", h.value);
        assert_eq!(h.offset, 0.0);
    }

    #[test]
    fn wheel_steps() {
        assert!((wheel_step(0.5, 0.0, 1.0, Wheel::Linear, 0.02) - 0.52).abs() < 1e-9);
        assert_eq!(wheel_step(0.99, 0.0, 1.0, Wheel::Linear, 0.02), 1.0);
        assert_eq!(wheel_step(-1.0, -1.0, 1.0, Wheel::Linear, -0.02), -1.0);
        let log = Wheel::Log { smallest: 0.05 };
        // From "cut" the first notch up lands on the smallest value, down goes back to 0.
        assert_eq!(wheel_step(0.0, 0.0, 10.0, log, 0.02), 0.05);
        assert_eq!(wheel_step(0.05, 0.0, 10.0, log, -0.02), 0.0);
        // 50 notches cover the whole log range.
        let mut v = 0.05;
        for _ in 0..50 {
            v = wheel_step(v, 0.0, 10.0, log, 0.02);
        }
        assert!((v - 10.0).abs() < 1e-6, "{v}");
        let up = wheel_step(1.0, 0.0, 10.0, log, 0.02);
        assert!((wheel_step(up, 0.0, 10.0, log, -0.02) - 1.0).abs() < 1e-9);
    }
}

// ------------------------------------------------------------ param rows

/// Width of the name column of parameter rows.
pub const LABEL_W: f32 = 78.0;

/// A row's name in its LABEL_W box: cut with "…" when too long (full name on hover), so it
/// never widens the panel.
/// `name · anything` row aligned with [`param_row`]s; returns what `body` returns.
pub fn label_row<R>(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.allocate_ui_with_layout(
                vec2(LABEL_W, ROW_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(LABEL_W);
                    row_label(ui, label);
                },
            );
            body(ui)
        })
        .inner;
    row_separator(ui);
    r
}

pub fn row_label(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.set_max_width(LABEL_W);
    ui.add(egui::Label::new(label).truncate().selectable(false))
}
const VALUE_W: f32 = 56.0;
const BUTTON_W: f32 = 18.0;
use super::theme::ROW_H;

/// Extras of a [`param_row`].
#[derive(Default)]
pub struct RowOpts<'a> {
    /// Show a B (bypass) toggle bound to this flag.
    pub bypass: Option<&'a mut bool>,
    /// Show an X (remove) button.
    pub remove: bool,
    /// Show the P (parameter animation) button.
    pub animate: bool,
    /// Wheel/slider scale (default linear).
    pub wheel: Option<Wheel>,
    /// Colour of the slider's value line (default: accent).
    pub line: Option<Color32>,
    /// Practical slider/drag range for params with a huge one (e.g. pixel
    /// anchors ±32768): the slider spans this, typed values may go further.
    pub soft: Option<(f64, f64)>,
    pub hover: &'a str,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RowResult {
    pub changed: bool,
    pub remove: bool,
}

/// Value as shown, per the param's display units.
pub fn fmt_value(p: &lavmfx_core::RangeParam) -> String {
    use lavmfx_core::DisplayUnits as U;
    let v = p.value;
    match p.view.as_ref().and_then(|w| w.display_units) {
        Some(U::Percent) => format!("{:.0} %", v * percent_multiplier(p)),
        Some(U::Degrees) => format!("{v:.0} °"),
        Some(U::Decibels) => format!("{v:.1} dB"),
        Some(U::Seconds) => format!("{v:.2} s"),
        Some(U::Milliseconds) => format!("{v:.0} ms"),
        Some(U::FramesPerSecond) => format!("{v:.2} fps"),
        Some(U::Integer) => format!("{v:.0}"),
        _ if (p.max - p.min).abs() >= 100.0 => format!("{v:.0}"),
        _ => format!("{v:.2}"),
    }
}

/// Parse typed text back to a value (percent params take "50" as 50 %).
fn parse_value(p: &lavmfx_core::RangeParam, text: &str) -> Option<f64> {
    let num: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | ','))
        .collect();
    let v: f64 = num.replace(',', ".").parse().ok()?;
    let percent = matches!(
        p.view.as_ref().and_then(|w| w.display_units),
        Some(lavmfx_core::DisplayUnits::Percent)
    );
    Some(if percent { v / percent_multiplier(p) } else { v })
}

/// Shown % per stored unit: the view's multiplier (100 for 0..1
/// opacities, 1 for Transform's Scale stored as 100), else 100.
fn percent_multiplier(p: &lavmfx_core::RangeParam) -> f64 {
    p.view
        .as_ref()
        .and_then(|w| w.multiplier)
        .filter(|m| *m > 0.0)
        .unwrap_or(100.0)
}

/// The − / + step: the view's step, else 1 % of the range (Shift: 0.1 %).
fn button_step(p: &lavmfx_core::RangeParam, fine: bool) -> f64 {
    let base = p
        .view
        .as_ref()
        .and_then(|w| w.step)
        .filter(|s| *s > 0.0)
        .unwrap_or_else(|| ((p.max - p.min) / 100.0).clamp(1e-3, 1.0));
    if fine { base / 10.0 } else { base }
}

/// Move the mouse pointer (X11 / Windows / macOS directly; Wayland only lets
/// a locked pointer move, so lock → position hint → unlock).
fn warp_pointer(ctx: &egui::Context, at: egui::Pos2) {
    use egui::{CursorGrab, ViewportCommand};
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::Locked));
    ctx.send_viewport_cmd(ViewportCommand::CursorPosition(at));
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::None));
}

/// Value box under a slider, opened by clicking it (like the range handles'):
/// typed straight away; Enter, Esc or a click outside closes it.
fn slider_value_popup(r: &egui::Response, v: &mut f64, min: f64, max: f64) -> bool {
    let first = r.clicked();
    let open_cmd = first.then_some(egui::SetOpenCommand::Bool(true));
    let mut changed = false;
    egui::Popup::from_response(r)
        .id(r.id.with("value"))
        .open_memory(open_cmd)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let span = (max - min).abs();
            let f = ui.add_sized(
                [80.0, 18.0],
                egui::DragValue::new(v)
                    .range(min..=max)
                    .speed((span / 300.0).clamp(1e-4, 10.0))
                    .max_decimals(if span > 100.0 { 1 } else { 3 }),
            );
            changed = f.changed();
            if first {
                f.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                ui.close();
            }
        });
    changed
}

/// Take this frame's mouse-wheel events (they don't reach the panels then):
/// (notches, Shift held). Reads the raw events: [`guard_wheel`] has already
/// zeroed the smoothed delta while the pointer is over a slider / knob.
fn take_wheel(ui: &egui::Ui) -> (f64, bool) {
    ui.input_mut(|i| {
        let mut n = 0.0;
        let mut fine = i.modifiers.shift;
        i.events.retain(|e| match e {
            egui::Event::MouseWheel {
                unit, delta, modifiers, ..
            } => {
                // Shift+wheel often arrives as horizontal scroll: take both axes.
                let d = delta.y + delta.x;
                n += match unit {
                    egui::MouseWheelUnit::Point => d / WHEEL_POINTS_PER_NOTCH,
                    egui::MouseWheelUnit::Line => d,
                    egui::MouseWheelUnit::Page => d * 10.0,
                };
                fine |= modifiers.shift;
                false
            }
            _ => true,
        });
        if n != 0.0 {
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        }
        (n as f64, fine)
    })
}

/// Sliders and knobs take the mouse wheel: they register their rect each
/// frame, and [`guard_wheel`] (start of the next frame) keeps panels from
/// scrolling while the pointer is over one.
#[derive(Clone, Default)]
struct WheelZones {
    last: Vec<egui::Rect>,
    cur: Vec<egui::Rect>,
    /// Scroll latch: a panel that is scrolling keeps the wheel until this time,
    /// even when a slider / knob passes under the pointer (like a browser).
    latch_until: f64,
    latched: bool,
}

/// A wheel pause longer than this ends a panel scroll; the next wheel turn
/// over a slider / knob goes to it.
const WHEEL_LATCH_S: f64 = 0.4;

fn zones_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::unique(("wheel-zones", ctx.viewport_id()))
}

pub fn wheel_zone(ui: &egui::Ui, rect: egui::Rect) {
    let rect = rect.intersect(ui.clip_rect());
    if rect.is_positive() {
        let id = zones_id(ui.ctx());
        ui.ctx()
            .data_mut(|d| d.get_temp_mut_or_default::<WheelZones>(id).cur.push(rect));
    }
}

/// Call at the start of each viewport's frame, before any panel: if the
/// pointer is over a slider / knob (as laid out last frame), panels get no
/// scroll this frame; the widget reads the wheel itself. While a panel is
/// scrolling (wheel turned within [`WHEEL_LATCH_S`]), it keeps the wheel.
pub fn guard_wheel(ctx: &egui::Context) {
    let id = zones_id(ctx);
    let (now, wheel, p) = ctx.input(|i| {
        let wheel = i.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. }));
        (i.time, wheel, i.pointer.hover_pos())
    });
    let over = ctx.data_mut(|d| {
        let z = d.get_temp_mut_or_default::<WheelZones>(id);
        z.last = std::mem::take(&mut z.cur);
        let over = p.is_some_and(|p| z.last.iter().any(|r| r.contains(p)));
        let latched = now < z.latch_until;
        if wheel && (!over || latched) {
            z.latch_until = now + WHEEL_LATCH_S;
        }
        z.latched = now < z.latch_until && (!over || latched);
        over && !z.latched
    });
    if over {
        ctx.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
}

/// True while a panel scroll holds the wheel: sliders / knobs must not take it.
pub fn wheel_latched(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<WheelZones>(zones_id(ctx)).is_some_and(|z| z.latched))
}

/// − / + / P as plain glyphs (no box around them).
fn small_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add_sized([BUTTON_W, ROW_H], egui::Button::new(RichText::new(text)).frame(false))
}

/// Thin line under a param row.
fn row_separator(ui: &egui::Ui) {
    let y = ui.cursor().top() - 1.5;
    ui.painter().hline(
        ui.max_rect().x_range(),
        y,
        egui::Stroke::new(1.0, super::theme::pal(ui).ground),
    );
}

/// Parameter row: `name · value · − + · slider · P B X`.
///
/// Drag the value (or click it to type), − / + step (Shift: fine), slider
/// with mouse wheel; double-click the name or the slider resets to the
/// param's default.
pub fn param_row(ui: &mut egui::Ui, label: &str, p: &mut lavmfx_core::RangeParam, opts: RowOpts) -> RowResult {
    let mut res = RowResult::default();
    let before = p.value;
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let name = ui
            .allocate_ui_with_layout(
                vec2(LABEL_W, ROW_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(LABEL_W);
                    // The name gets what the cogwheel (14 px + spacing) leaves, so a long
                    // name is cut ("Modulatio…", full name on hover) instead of pushing the
                    // row out of the panel.
                    let room = LABEL_W - if opts.animate { 17.0 } else { 0.0 };
                    let name = ui
                        .allocate_ui_with_layout(
                            vec2(room, ROW_H),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.set_max_width(room);
                                ui.add(
                                    egui::Label::new(label)
                                        .truncate()
                                        .selectable(false)
                                        .sense(egui::Sense::click()),
                                )
                            },
                        )
                        .inner;
                    // The cogwheel left of the name opens the animation menu.
                    if opts.animate {
                        drive_menu(ui, p);
                    }
                    name
                },
            )
            .inner;
        let name = if opts.hover.is_empty() {
            name
        } else {
            name.on_hover_text(opts.hover)
        };
        let mut reset = name.double_clicked();

        let mut v = p.value;
        // Driven params show the animated value; edits go to the drive's handles.
        let driven = p.is_driven();
        // Spinners (pixel positions) have no slider and drag 1 step per pixel.
        let spinner = p
            .view
            .as_ref()
            .is_some_and(|w| w.control_type == Some(lavmfx_core::ControlType::Spinner));
        let (lo, hi) = opts.soft.unwrap_or((p.min, p.max));
        let speed = if spinner {
            p.view.as_ref().and_then(|w| w.step).unwrap_or(1.0)
        } else {
            (hi - lo).max(1e-9) / 300.0
        };
        let shown = p.clone();
        let drag = egui::DragValue::new(&mut v)
            .range(p.min..=p.max)
            .speed(speed)
            .custom_formatter(move |x, _| {
                let mut q = shown.clone();
                q.value = x;
                fmt_value(&q)
            })
            .custom_parser({
                let q = p.clone();
                move |t| parse_value(&q, t)
            });
        if ui.add_sized([VALUE_W, ROW_H], drag).changed() && !driven {
            p.set(v);
        }

        let fine = ui.input(|i| i.modifiers.shift);
        if small_button(ui, "−").on_hover_text("Decrease (Shift: fine)").clicked() && !driven {
            p.set(p.value - button_step(p, fine));
        }
        if small_button(ui, "+").on_hover_text("Increase (Shift: fine)").clicked() && !driven {
            p.set(p.value + button_step(p, fine));
        }

        let buttons = [opts.bypass.is_some(), opts.remove].iter().filter(|b| **b).count() as f32;
        let w = (ui.available_width() - buttons * (BUTTON_W + 3.0) - 4.0).max(30.0);
        if spinner {
            ui.add_space(w);
        } else {
            ui.spacing_mut().slider_width = w;
            let mut v = p.value.clamp(lo, hi);
            let r = wheel_slider_line(
                ui,
                &mut v,
                lo..=hi,
                opts.wheel.unwrap_or(Wheel::Linear),
                false,
                opts.line,
            );
            if r.changed() && !driven {
                p.set(v);
            }
            reset |= r.double_clicked();
            if driven {
                let units = p.clone();
                if let Some(d) = p.drive.as_mut() {
                    range_handles(ui, r.rect, (lo, hi), d, &units);
                }
            }
        }
        if reset
            && !driven
            && let Some(d) = p.default
        {
            p.set(d);
        }

        if let Some(b) = opts.bypass {
            let r = ui.add_sized(
                [BUTTON_W, ROW_H],
                egui::Button::new(RichText::new("B").small()).selected(*b),
            );
            if r.on_hover_text("Bypass").clicked() {
                *b = !*b;
                res.changed = true;
            }
        }
        if opts.remove && small_button(ui, "X").on_hover_text("Remove").clicked() {
            res.remove = true;
        }
    });
    super::shortcuts::learn_target(ui, row.response.rect, super::shortcuts::LearnTarget::Param(p.id));
    if p.is_driven() {
        drive_controls(ui, p);
    }
    row_separator(ui);
    res.changed |= p.value != before;
    res
}

// ------------------------------------------------------------ param drive

/// The P button and its source menu (FFT sources not yet).
/// Cogwheel drawn as a shape (body, 8 teeth, hole).
fn paint_gear(p: &egui::Painter, c: egui::Pos2, r: f32, col: Color32, hole: Color32) {
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::TAU / 8.0;
        let d = vec2(a.cos(), a.sin());
        p.line_segment([c + d * (r * 0.55), c + d * r], egui::Stroke::new(r * 0.42, col));
    }
    p.circle_filled(c, r * 0.68, col);
    p.circle_filled(c, r * 0.28, hole);
}

/// The cogwheel and its animation-source menu.
fn drive_menu(ui: &mut egui::Ui, p: &mut lavmfx_core::RangeParam) {
    use lavmfx_core::drive::Source;
    let pal = super::theme::pal(ui);
    let driven = p.is_driven();
    let (rect, r) = ui.allocate_exact_size(vec2(14.0, ROW_H), egui::Sense::click());
    let col = if driven {
        pal.accent
    } else if r.hovered() {
        pal.text_strong
    } else {
        pal.text_weak.gamma_multiply(0.6)
    };
    paint_gear(ui.painter(), rect.center(), 5.5, col, pal.panel);
    let r = match &p.drive {
        Some(d) if driven => r.on_hover_text(format!("Animated: {} (click: change)", d.source.name())),
        _ => r.on_hover_text("Parameter animation"),
    };
    let current = p.drive.as_ref().map_or(Source::Basic, |d| d.source);
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(150.0);
        for (src, name) in Source::ALL {
            let label = if src == current {
                format!("●  {name}")
            } else {
                format!("    {name}")
            };
            if ui.button(label).clicked() {
                p.set_source(src);
                ui.close();
            }
        }
        ui.separator();
        let on = p.drive.as_ref().is_some_and(|d| d.envelope.is_some());
        let label = if on { "●  Envelope" } else { "    Envelope" };
        let r = ui.add_enabled(driven, egui::Button::new(label));
        if r.on_disabled_hover_text("Choose a source first")
            .on_hover_text("Shape the animation with a curve")
            .clicked()
            && let Some(d) = p.drive.as_mut()
        {
            d.envelope = if on { None } else { Some(Default::default()) };
            ui.close();
        }
    });
}

/// Draggable from/to triangles under a driven param's slider.
fn range_handles(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    (lo, hi): (f64, f64),
    d: &mut lavmfx_core::drive::Drive,
    units: &lavmfx_core::RangeParam,
) {
    let pid = units.id;
    let span = (hi - lo).max(1e-9);
    let x_of = |v: f64| rect.left() + ((v - lo) / span).clamp(0.0, 1.0) as f32 * rect.width();
    let y = rect.bottom();
    for (which, up) in [(0, false), (1, true)] {
        let v = if which == 0 { d.from } else { d.to };
        let x = x_of(v);
        let hit = egui::Rect::from_center_size(egui::pos2(x, y - 3.0), vec2(10.0, 9.0));
        let id = ui.make_persistent_id(("drive-handle", pid, which));
        let r = ui.interact(hit, id, egui::Sense::click_and_drag());
        if r.dragged() {
            let nv = (v + r.drag_delta().x as f64 / rect.width() as f64 * span).clamp(lo, hi);
            if which == 0 {
                d.from = nv;
            } else {
                d.to = nv;
            }
        }
        // Click (no drag): type the value.
        let target = if which == 0 { &mut d.from } else { &mut d.to };
        handle_value_popup(&r, id, which == 0, target, units);
        let col = if r.hovered() || r.dragged() {
            super::theme::cur().text_strong
        } else {
            super::theme::cur().accent
        };
        // "from" points right (▶ base on the left), "to" points left.
        let s = 4.0;
        let pts = if up {
            vec![egui::pos2(x, y - 6.0), egui::pos2(x - s, y), egui::pos2(x + s, y)]
        } else {
            vec![egui::pos2(x - s, y - 6.0), egui::pos2(x + s, y - 6.0), egui::pos2(x, y)]
        };
        ui.painter()
            .add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
        r.on_hover_text(if which == 0 {
            "Range start (phase 0)"
        } else {
            "Range end (phase 1)"
        });
    }
    // Phase marker.
    let x = x_of(d.value());
    ui.painter().line_segment(
        [egui::pos2(x, rect.top() + 2.0), egui::pos2(x, rect.bottom() - 2.0)],
        (1.5, super::theme::cur().accent),
    );
}

/// Popup with a value field for a range handle, opened by clicking it.
/// Enter, Esc or a click outside closes it.
fn handle_value_popup(r: &egui::Response, id: egui::Id, from: bool, v: &mut f64, units: &lavmfx_core::RangeParam) {
    let open_cmd = r.clicked().then_some(egui::SetOpenCommand::Bool(true));
    let first = r.clicked();
    egui::Popup::from_response(r)
        .id(id.with("value"))
        .open_memory(open_cmd)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.horizontal(|ui| {
                ui.label(if from { "From" } else { "To" });
                let shown = units.clone();
                let parse = units.clone();
                let f = ui.add_sized(
                    [80.0, 18.0],
                    egui::DragValue::new(v)
                        .range(units.min..=units.max)
                        .speed(((units.max - units.min) / 300.0).clamp(1e-4, 1.0))
                        .custom_formatter(move |x, _| {
                            let mut q = shown.clone();
                            q.value = x;
                            fmt_value(&q)
                        })
                        .custom_parser(move |t| parse_value(&parse, t)),
                );
                if first {
                    // Straight into typing.
                    f.request_focus();
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                    ui.close();
                }
            });
        });
}

/// Settings of the active source, under the param row.
fn drive_controls(ui: &mut egui::Ui, p: &mut lavmfx_core::RangeParam) {
    use lavmfx_core::drive::{Direction, LINKS, PlayMode, Source};
    let (min, max) = (p.min, p.max);
    let id = p.id;
    let units = p.clone();
    let Some(d) = p.drive.as_mut() else { return };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_space(LABEL_W + 3.0);
        match d.source {
            Source::Timeline | Source::BpmSync => {
                for (dir, icon, hover) in [
                    (Direction::Backward, Icon::Back, "Backward"),
                    (Direction::Pause, Icon::Pause, "Pause"),
                    (Direction::Forward, Icon::Forward, "Forward"),
                ] {
                    let r = icon_button(ui, icon, d.direction == dir, vec2(22.0, 18.0), true);
                    if r.on_hover_text(hover).clicked() {
                        d.direction = dir;
                    }
                }
                let (val, suffix, range) = if d.source == Source::Timeline {
                    (&mut d.seconds, " s", 0.01..=3600.0)
                } else {
                    (&mut d.beats, " beats", 0.0625..=1024.0)
                };
                ui.add_sized(
                    [64.0, 18.0],
                    egui::DragValue::new(val).range(range).speed(0.05).suffix(suffix),
                )
                .on_hover_text("Length of one pass");
                if ui
                    .add(egui::Button::new(RichText::new("/2")).min_size(vec2(26.0, 18.0)))
                    .clicked()
                {
                    *val /= 2.0;
                }
                if ui
                    .add(egui::Button::new(RichText::new("*2")).min_size(vec2(26.0, 18.0)))
                    .clicked()
                {
                    *val *= 2.0;
                }
                let mode = PlayMode::ALL.iter().find(|(m, _)| *m == d.mode).map_or("", |(_, n)| n);
                egui::ComboBox::from_id_salt(("drive-mode", id))
                    .width(64.0)
                    .selected_text(mode)
                    .show_ui(ui, |ui| {
                        for (m, n) in PlayMode::ALL {
                            if ui.selectable_label(d.mode == m, n).clicked() {
                                d.mode = m;
                                d.restart();
                            }
                        }
                    });
                if d.mode == PlayMode::Once && ui.small_button("Restart").clicked() {
                    d.restart();
                }
            }
            Source::Dashboard => {
                ui.label(RichText::new("Link").weak());
                egui::ComboBox::from_id_salt(("drive-link", id))
                    .width(64.0)
                    .selected_text(lavmfx_core::drive::link_key(d.link))
                    .show_ui(ui, |ui| {
                        for i in 0..LINKS {
                            if ui
                                .selectable_label(d.link == i, lavmfx_core::drive::link_key(i))
                                .clicked()
                            {
                                d.link = i;
                            }
                        }
                    });
            }
            Source::TransitionPhase => {
                ui.label(RichText::new("follows the layer's transition").weak().small());
            }
            Source::CrossFaderPhase => {
                ui.label(RichText::new("follows the crossfader (A → B)").weak().small());
            }
            Source::ExternalFft | Source::CompositionFft | Source::LayerFft => fft_controls(ui, d, id),
            Source::Basic => {}
        }
    });
    // Range as numbers (also for params without a slider).
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.add_space(LABEL_W + 3.0);
        let speed = ((max - min) / 300.0).clamp(1e-4, 1.0);
        for (label, v) in [("from", &mut d.from), ("to", &mut d.to)] {
            ui.label(RichText::new(label).weak().small());
            let shown = units.clone();
            ui.add_sized(
                [VALUE_W, ROW_H],
                egui::DragValue::new(v)
                    .range(min..=max)
                    .speed(speed)
                    .custom_formatter(move |x, _| {
                        let mut q = shown.clone();
                        q.value = x;
                        fmt_value(&q)
                    })
                    .custom_parser({
                        let q = units.clone();
                        move |t| parse_value(&q, t)
                    }),
            );
        }
        if ui
            .add(egui::Button::new(RichText::new("swap").small()))
            .on_hover_text("Swap from/to")
            .clicked()
        {
            std::mem::swap(&mut d.from, &mut d.to);
        }
    });
    if d.envelope.is_some() {
        envelope_editor(ui, d, id);
    }
}

/// Knob on a plain number (wraps `knob`); double-click resets to `default`.
pub fn value_knob(ui: &mut egui::Ui, label: &str, v: &mut f64, range: (f64, f64), default: f64) -> egui::Response {
    let mut p = lavmfx_core::RangeParam::new(0, range.0, range.1, default);
    p.value = v.clamp(range.0, range.1);
    let r = knob(ui, label, &mut p);
    *v = p.value;
    r
}

/// FFT drive (External / Composition / Layer FFT): L·M·H band buttons, the
/// spectrum this param sees with its band range (drag the edges, or drag
/// inside to move it), the output meter, Gain and Fall knobs.
fn fft_controls(ui: &mut egui::Ui, d: &mut lavmfx_core::drive::Drive, pid: lavmfx_core::Id) {
    use lavmfx_core::fft::{self, GAIN_MAX, Preset};
    let pal = super::theme::cur();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let current = d.fft.preset();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (p, name, hover) in Preset::ALL {
                let on = current == Some(p);
                let b =
                    egui::Button::new(
                        RichText::new(name)
                            .small()
                            .color(if on { pal.text_strong } else { pal.text }),
                    )
                    .fill(if on { pal.accent_dim } else { pal.button })
                    .min_size(vec2(18.0, 16.0));
                if ui.add(b).on_hover_text(hover).clicked() {
                    d.fft.set_preset(p);
                }
            }
        });
        // Spectrum strip.
        let w = (ui.available_width() - 4.0).clamp(80.0, 200.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), egui::Sense::hover());
        let painter = ui.painter_at(rect.expand(1.0));
        painter.rect_filled(rect, 2.0, pal.field);
        let x_of = |t: f64| rect.left() + t.clamp(0.0, 1.0) as f32 * rect.width();
        let (lo, hi) = (d.fft.lo.min(d.fft.hi), d.fft.lo.max(d.fft.hi));
        let band = egui::Rect::from_x_y_ranges(x_of(lo)..=x_of(hi).max(x_of(lo) + 2.0), rect.y_range());
        painter.rect_filled(band, 0.0, pal.accent.gamma_multiply(0.18));
        let bw = rect.width() / fft::BANDS as f32;
        let inside = d.fft.bands();
        let hgt = rect.height() - 2.0;
        let thr = d.fft.threshold.clamp(0.0, 0.99) as f32;
        let y_thr = rect.bottom() - 1.0 - thr * hgt;
        for (i, v) in d.seen.iter().enumerate() {
            let v = v.clamp(0.0, 1.0);
            let x = rect.left() + i as f32 * bw;
            let bar = |lo: f32, hi: f32, col| {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x + 0.5, rect.bottom() - 1.0 - hi * hgt),
                        egui::pos2(x + bw - 0.5, rect.bottom() - 1.0 - lo * hgt),
                    ),
                    0.0,
                    col,
                )
            };
            // Below the threshold = cut (dim); above it = what drives the param.
            let (on, cut) = if inside.contains(&i) {
                (pal.accent, pal.accent.gamma_multiply(0.3))
            } else {
                (pal.text_weak.gamma_multiply(0.6), pal.text_weak.gamma_multiply(0.25))
            };
            bar(0.0, v.min(thr), cut);
            if v > thr {
                bar(thr, v, on);
            }
        }
        if thr > 0.0 {
            // Dashed threshold line.
            let mut x = rect.left();
            while x < rect.right() {
                let x2 = (x + 4.0).min(rect.right());
                painter.line_segment(
                    [egui::pos2(x, y_thr), egui::pos2(x2, y_thr)],
                    (1.0, pal.text_strong.gamma_multiply(0.8)),
                );
                x += 7.0;
            }
        }
        // Output level (after gain and fall) as a line across the band.
        let y = rect.bottom() - 1.0 - d.phase.clamp(0.0, 1.0) as f32 * (rect.height() - 2.0);
        painter.line_segment(
            [egui::pos2(band.left(), y), egui::pos2(band.right(), y)],
            (1.5, pal.text_strong),
        );
        // Edges and body drag.
        let span = rect.width() as f64;
        for which in 0..3 {
            let hit = match which {
                0 => egui::Rect::from_center_size(egui::pos2(band.left(), rect.center().y), vec2(8.0, rect.height())),
                1 => egui::Rect::from_center_size(egui::pos2(band.right(), rect.center().y), vec2(8.0, rect.height())),
                _ => band.shrink2(vec2(4.0, 0.0)),
            };
            if hit.width() <= 0.0 {
                continue;
            }
            let r = ui.interact(
                hit,
                ui.make_persistent_id(("fft-band", pid, which)),
                egui::Sense::drag(),
            );
            let dx = r.drag_delta().x as f64 / span;
            if r.dragged() && dx != 0.0 {
                match which {
                    0 => d.fft.lo = (lo + dx).clamp(0.0, hi),
                    1 => d.fft.hi = (hi + dx).clamp(lo, 1.0),
                    _ => {
                        let dx = dx.clamp(-lo, 1.0 - hi);
                        (d.fft.lo, d.fft.hi) = (lo + dx, hi + dx);
                    }
                }
            }
            if r.hovered() || r.dragged() {
                ui.ctx().set_cursor_icon(if which == 2 {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::ResizeHorizontal
                });
            }
            if which < 2 {
                let x = if which == 0 { band.left() } else { band.right() };
                let col = if r.hovered() || r.dragged() {
                    pal.text_strong
                } else {
                    pal.accent
                };
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], (1.5, col));
            }
            r.on_hover_text(format!(
                "Band {:.0} Hz – {:.0} Hz (drag the edges or the band)",
                fft::freq_at(lo),
                fft::freq_at(hi)
            ));
        }
        ui.ctx().request_repaint();
    });
    value_knob(ui, "Gain", &mut d.fft.gain, (0.0, GAIN_MAX), 1.0).on_hover_text(format!("Gain {:.2}×", d.fft.gain));
    value_knob(ui, "Fall", &mut d.fft.fall, (0.0, 1.0), 0.3).on_hover_text(format!(
        "Fall: {:.2} s over the full range (right = slower, longer tail)",
        d.fft.fall_time()
    ));
    value_knob(ui, "Thresh", &mut d.fft.threshold, (0.0, 0.95), 0.0).on_hover_text(format!(
        "Threshold {:.0} dB: quieter sound (ambient noise) is ignored (dashed line on the meter)",
        lavmfx_core::fft::DB_FLOOR * (1.0 - d.fft.threshold)
    ));
}

/// The global FFT threshold: the same Thresh knob as the FFT controls.
pub fn thresh_knob(ui: &mut egui::Ui, t: &mut f64, label: &str) -> egui::Response {
    let db = lavmfx_core::fft::DB_FLOOR * (1.0 - *t);
    value_knob(ui, label, t, (0.0, 0.95), 0.0).on_hover_text(format!(
        "Global FFT threshold {db:.0} dB: quieter sound (ambient noise) is ignored by every FFT-driven parameter (each parameter's own Thresh comes on top). Double-click: off"
    ))
}

/// Envelope: curve from phase (x) to output (y, 0 = from, 1 = to).
/// Drag points; double-click empty space adds one, double-click or
/// right-click a point removes it (the ends stay); click selects for the
/// readout row (Phase · Value · Curve of the segment after it).
fn envelope_editor(ui: &mut egui::Ui, d: &mut lavmfx_core::drive::Drive, pid: lavmfx_core::Id) {
    use lavmfx_core::drive::{Curve, EnvPoint, Envelope};
    let phase = d.phase.clamp(0.0, 1.0);
    let Some(env) = d.envelope.as_mut() else { return };
    let sel_id = ui.make_persistent_id(("env-sel", pid));
    let drag_id = ui.make_persistent_id(("env-drag", pid));
    let mut sel: Option<usize> = ui.data(|m| m.get_temp(sel_id)).flatten();
    let mut dragging: Option<usize> = ui.data(|m| m.get_temp(drag_id)).flatten();

    ui.horizontal(|ui| {
        ui.add_space(LABEL_W + 3.0);
        ui.vertical(|ui| {
            let w = (ui.available_width() - 4.0).max(120.0);
            let h = (w * 0.4).clamp(70.0, 150.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(w, h), egui::Sense::click_and_drag());
            let to_screen = |x: f64, y: f64| {
                egui::pos2(
                    rect.left() + x as f32 * rect.width(),
                    rect.bottom() - y as f32 * rect.height(),
                )
            };
            let from_screen = |p: egui::Pos2| {
                (
                    ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64,
                    ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0) as f64,
                )
            };
            let near = |env: &Envelope, p: egui::Pos2| {
                env.points
                    .iter()
                    .enumerate()
                    .map(|(i, q)| (i, to_screen(q.x, q.y).distance(p)))
                    .filter(|(_, d)| *d < 9.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
            };

            // Interaction.
            if let Some(pos) = resp.interact_pointer_pos() {
                if resp.drag_started() || resp.clicked() {
                    sel = near(env, pos);
                    dragging = if resp.drag_started() { sel } else { None };
                }
                if resp.dragged()
                    && let Some(i) = dragging
                {
                    let (x, y) = from_screen(pos);
                    env.move_point(i, x, y);
                }
                if resp.double_clicked() {
                    match near(env, pos) {
                        Some(i) => {
                            env.remove(i);
                            sel = None;
                        }
                        None => {
                            let (x, y) = from_screen(pos);
                            sel = Some(env.insert(x, y));
                        }
                    }
                }
                if resp.secondary_clicked()
                    && let Some(i) = near(env, pos)
                    && env.remove(i)
                {
                    sel = None;
                }
            }
            if resp.drag_stopped() {
                dragging = None;
            }
            let hover = resp.hover_pos().and_then(|p| near(env, p));
            resp.on_hover_text("Drag points · double-click: add / remove · right-click: remove");

            // Drawing.
            let painter = ui.painter_at(rect.expand(5.0));
            painter.rect_filled(rect, 2.0, super::theme::cur().field);
            let grid = super::theme::cur().border;
            for k in 1..4 {
                let f = k as f32 / 4.0;
                let x = rect.left() + f * rect.width();
                let y = rect.top() + f * rect.height();
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], (1.0, grid));
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], (1.0, grid));
            }
            let line: Vec<egui::Pos2> = (0..=128)
                .map(|k| {
                    let x = k as f64 / 128.0;
                    to_screen(x, env.eval(x))
                })
                .collect();
            painter.add(egui::Shape::line(line, (2.0, super::theme::cur().accent)));
            // Live phase.
            let px = to_screen(phase, 0.0).x;
            painter.line_segment(
                [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                (1.0, super::theme::cur().text_weak),
            );
            painter.circle_filled(to_screen(phase, env.eval(phase)), 3.0, super::theme::cur().text_strong);
            for (i, q) in env.points.iter().enumerate() {
                let c = to_screen(q.x, q.y);
                let active = sel == Some(i) || hover == Some(i);
                painter.circle_filled(
                    c,
                    if active { 5.0 } else { 4.0 },
                    if sel == Some(i) {
                        super::theme::cur().text_strong
                    } else {
                        super::theme::cur().accent
                    },
                );
                painter.circle_stroke(c, 5.0, (1.0, super::theme::cur().ground));
            }

            // Readout of the selected point (else the first).
            let i = sel.filter(|&i| i < env.points.len()).unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let n = env.points.len();
                let EnvPoint {
                    mut x,
                    mut y,
                    mut curve,
                } = env.points[i];
                ui.label(RichText::new("Phase").weak().small());
                let r = ui.add_enabled(
                    i > 0 && i + 1 < n,
                    egui::DragValue::new(&mut x)
                        .range(0.0..=1.0)
                        .speed(0.005)
                        .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                        .custom_parser(|t| {
                            t.trim_end_matches(['%', ' '])
                                .trim()
                                .parse::<f64>()
                                .ok()
                                .map(|v| v / 100.0)
                        }),
                );
                ui.label(RichText::new("Value").weak().small());
                let r2 = ui.add(
                    egui::DragValue::new(&mut y)
                        .range(0.0..=1.0)
                        .speed(0.005)
                        .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                        .custom_parser(|t| {
                            t.trim_end_matches(['%', ' '])
                                .trim()
                                .parse::<f64>()
                                .ok()
                                .map(|v| v / 100.0)
                        }),
                );
                if r.changed() || r2.changed() {
                    env.move_point(i, x, y);
                }
                if i + 1 < n {
                    egui::ComboBox::from_id_salt(("env-curve", pid))
                        .width(72.0)
                        .selected_text(curve.name())
                        .show_ui(ui, |ui| {
                            for (c, name) in Curve::ALL {
                                ui.selectable_value(&mut curve, c, name);
                            }
                        })
                        .response
                        .on_hover_text("Shape of the segment after this point");
                    env.points[i].curve = curve;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                let small = |t: &str| egui::Button::new(RichText::new(t).small());
                if ui.add(small("Reset")).on_hover_text("Straight line 0 → 1").clicked() {
                    *env = Envelope::default();
                    sel = None;
                }
                if ui.add(small("Invert")).on_hover_text("Flip upside down").clicked() {
                    for q in &mut env.points {
                        q.y = 1.0 - q.y;
                    }
                }
                if ui.add(small("Triangle")).on_hover_text("0 → 1 → 0").clicked() {
                    *env = Envelope::default();
                    env.move_point(1, 1.0, 0.0);
                    env.insert(0.5, 1.0);
                    sel = None;
                }
                if ui.add(small("Smooth")).on_hover_text("Every segment smooth").clicked() {
                    for q in &mut env.points {
                        q.curve = Curve::Smooth;
                    }
                }
            });
        });
    });
    ui.data_mut(|m| {
        m.insert_temp(sel_id, sel);
        m.insert_temp(drag_id, dragging);
    });
}

/// `name · combo` row aligned with [`param_row`]s. Returns true on change.
pub fn choice_row(
    ui: &mut egui::Ui,
    label: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
    p: &mut lavmfx_core::ChoiceParam,
    ready: impl Fn(&str) -> bool,
) -> bool {
    let r = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, ROW_H),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_W);
                row_label(ui, label);
            },
        );
        choice_combo_with(ui, id, label, p, ready)
    });
    super::shortcuts::learn_target(ui, r.response.rect, super::shortcuts::LearnTarget::Param(p.id));
    row_separator(ui);
    r.inner
}

/// `name · checkbox` row aligned with [`param_row`]s.
pub fn bool_row(ui: &mut egui::Ui, label: &str, value: &mut bool, hover: &str) -> bool {
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.allocate_ui_with_layout(
                vec2(LABEL_W, ROW_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(LABEL_W);
                    row_label(ui, label);
                },
            );
            ui.checkbox(value, "").on_hover_text(hover).changed()
        })
        .inner;
    row_separator(ui);
    r
}

/// Header of a collapsible section, drawn as a full-width band.
pub struct Section {
    pub open: bool,
    /// The band (put controls at its right with [`band_right`]).
    pub header: egui::Rect,
    /// The band's title part (click folds; drags can start here).
    pub title: egui::Response,
}

/// Section band: grey full-width strip, drawn fold arrow, title;
/// click toggles. `accent` = mint band (Transform header).
pub fn section_header(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    default_open: bool,
    accent: bool,
) -> Section {
    section_header_dim(ui, id_salt, title, default_open, accent, false)
}

/// [`section_header`] with an optionally dimmed title (bypassed effects).
pub fn section_header_dim(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    default_open: bool,
    accent: bool,
    dim: bool,
) -> Section {
    let id = ui.make_persistent_id(("section", id_salt));
    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    let mut open = st.is_open();
    let s = section_band(ui, id, title, &mut open, accent, dim);
    st.set_open(open);
    st.store(ui.ctx());
    s
}

/// [`section_header_dim`] whose open state is the caller's `open` (kept in
/// the model, e.g. an effect's fold saved with the project).
pub fn section_header_state(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    open: &mut bool,
    accent: bool,
    dim: bool,
) -> Section {
    let id = ui.make_persistent_id(("section", id_salt));
    section_band(ui, id, title, open, accent, dim)
}

/// The section band itself; a click on its title part toggles `open`.
fn section_band(ui: &mut egui::Ui, id: egui::Id, title: &str, open: &mut bool, accent: bool, dim: bool) -> Section {
    let pal = super::theme::pal(ui);
    let x = ui.max_rect().x_range();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), egui::Sense::hover());
    let band = egui::Rect::from_x_y_ranges(x, rect.y_range());
    // Only the left part toggles, so controls in the band stay clickable.
    let hit = egui::Rect::from_min_max(
        band.min,
        egui::pos2(band.left() + (LABEL_W + 24.0).min(band.width()), band.bottom()),
    );
    let r = ui.interact(hit, id.with("toggle"), egui::Sense::click_and_drag());
    if r.clicked() {
        *open = !*open;
    }
    let fill = match (accent, r.hovered()) {
        (true, _) => pal.accent_dim,
        (false, true) => pal.button,
        (false, false) => pal.band,
    };
    let painter = ui.painter();
    painter.rect_filled(band, 0.0, fill);
    // Fold arrow.
    let c = egui::pos2(band.left() + 9.0, band.center().y);
    let s = 3.5;
    let pts = if *open {
        vec![
            egui::pos2(c.x - s, c.y - s * 0.6),
            egui::pos2(c.x + s, c.y - s * 0.6),
            egui::pos2(c.x, c.y + s * 0.8),
        ]
    } else {
        vec![
            egui::pos2(c.x - s * 0.6, c.y - s),
            egui::pos2(c.x + s * 0.8, c.y),
            egui::pos2(c.x - s * 0.6, c.y + s),
        ]
    };
    painter.add(egui::Shape::convex_polygon(pts, pal.text_weak, egui::Stroke::NONE));
    painter.text(
        egui::pos2(band.left() + 19.0, band.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.0),
        if dim { pal.text_weak } else { pal.text_strong },
    );
    Section {
        open: *open,
        header: band,
        title: r,
    }
}

/// A right-to-left child UI inside a section band (`width` from the right).
pub fn band_right(ui: &mut egui::Ui, header: egui::Rect, width: f32) -> egui::Ui {
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                egui::pos2(header.right() - width, header.top()),
                egui::pos2(header.right() - 3.0, header.bottom()),
            ))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    )
}

/// Collapsible section like the panel groups ("Composition", "Video", …).
pub fn section<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let s = section_header(ui, title, title, true, false);
    s.open.then(|| {
        ui.add_space(2.0);
        let r = body(ui);
        ui.add_space(2.0);
        r
    })
}

// ------------------------------------------------------------ dashboard

/// Rotary knob (dashboard links): drag up/down or wheel, double-click = default.
pub fn knob(ui: &mut egui::Ui, label: &str, p: &mut lavmfx_core::RangeParam) -> egui::Response {
    let size = vec2(44.0, 46.0);
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    super::shortcuts::learn_target(ui, rect, super::shortcuts::LearnTarget::Param(p.id));
    wheel_zone(ui, rect);
    let span = (p.max - p.min).max(1e-9);
    if r.dragged() && !p.is_driven() {
        let fine = ui.input(|i| i.modifiers.shift);
        let k = if fine { 1500.0 } else { 150.0 };
        p.set(p.value - r.drag_delta().y as f64 / k * span);
    }
    // Wheel: one notch = WHEEL_STEP of the range (Shift = fine), like sliders.
    if r.contains_pointer() && !p.is_driven() && !wheel_latched(ui.ctx()) {
        let (notches, fine) = take_wheel(ui);
        if notches != 0.0 {
            p.set(p.value + notches * if fine { WHEEL_FINE } else { WHEEL_STEP } * span);
        }
    }
    if r.double_clicked()
        && let Some(d) = p.default
    {
        p.set(d);
    }
    // Knob: dark ring, thick accent arc for the value, short pointer.
    let pal = super::theme::cur();
    let c = egui::pos2(rect.center().x, rect.top() + 16.0);
    let rad = 12.0;
    let painter = ui.painter_at(rect);
    let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
        (0..=32)
            .map(|k| {
                let a = from + (to - from) * k as f32 / 32.0;
                egui::pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
            })
            .collect()
    };
    // 270° sweep from bottom-left to bottom-right.
    let (a0, a1) = (std::f32::consts::PI * 0.75, std::f32::consts::PI * 2.25);
    let t = p.normalized() as f32;
    painter.circle_filled(c, rad + 2.5, pal.field);
    // Track: darker than the knob face, independent of the (customisable) background.
    painter.add(egui::Shape::line(
        arc(a0, a1),
        (4.0, pal.field.lerp_to_gamma(Color32::BLACK, 0.55)),
    ));
    let col = if r.hovered() || r.dragged() {
        pal.accent
    } else {
        pal.accent.gamma_multiply(0.85)
    };
    if t > 0.0 {
        painter.add(egui::Shape::line(arc(a0, a0 + (a1 - a0) * t), (4.0, col)));
    }
    let a = a0 + (a1 - a0) * t;
    painter.line_segment(
        [
            egui::pos2(c.x + (rad - 7.0) * a.cos(), c.y + (rad - 7.0) * a.sin()),
            egui::pos2(c.x + (rad - 1.0) * a.cos(), c.y + (rad - 1.0) * a.sin()),
        ],
        (2.0, col),
    );
    // Name under the knob, cut with "…" to the knob's width (imported knob
    // names such as "Brightness" are wider; full name on hover).
    let mut job =
        egui::text::LayoutJob::simple_singleline(label.to_owned(), egui::FontId::proportional(11.0), pal.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 2.0);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.bottom() - 6.0 - galley.size().y / 2.0,
        ),
        galley,
        pal.text,
    );
    r.on_hover_text(format!(
        "{label}: {:.0} % (drag / wheel; double-click: reset)",
        t * 100.0
    ))
}

/// Volume (dB) and Pan rows of an audio track (clip, layer, group, composition).
pub fn audio_rows(ui: &mut egui::Ui, t: &mut lavmfx_core::AudioTrack, with_pan: bool) {
    param_row(
        ui,
        "Volume",
        &mut t.volume,
        RowOpts {
            animate: true,
            soft: Some((-60.0, 12.0)),
            hover: "Volume in dB (type −192 for silence)",
            ..Default::default()
        },
    );
    if with_pan {
        param_row(
            ui,
            "Pan",
            &mut t.pan,
            RowOpts {
                animate: true,
                hover: "Balance: −1 left … 1 right",
                ..Default::default()
            },
        );
    }
}

/// Dashboard: 8 link knobs any param of this owner can follow (P › Dashboard).
pub fn dashboard_section(
    ui: &mut egui::Ui,
    dash: &mut lavmfx_core::ParameterCollection,
    ids: &mut lavmfx_core::IdGen,
    salt: impl std::hash::Hash + std::fmt::Debug,
    open: bool,
) {
    use lavmfx_core::drive::{LINKS, ensure_links, link_key, link_label};
    if section_header(ui, ("dashboard", salt), "Dashboard", open, false).open {
        ensure_links(dash, ids);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for i in 0..LINKS {
                let label = link_label(dash, i);
                if let Some(lavmfx_core::Param::Range(p)) = dash.get_mut(&link_key(i)) {
                    knob(ui, &label, p);
                }
            }
        });
    }
}

/// Plain-number row (Advanced Output properties): `name · value · − + ·
/// slider`, aligned with [`param_row`]. `slider: false` = spinner only
/// (pixel positions). Double-click the name = `default`. Returns true on change.
pub fn value_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
    default: f32,
    slider: bool,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let before = *v;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let name = ui
            .allocate_ui_with_layout(
                vec2(LABEL_W, ROW_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(LABEL_W);
                    row_label(ui, label)
                },
            )
            .inner;
        if name.double_clicked() {
            *v = default;
        }
        let decimals = if step >= 1.0 { 0 } else { 2 };
        let speed = if slider { (hi - lo) / 300.0 } else { step };
        ui.add_sized(
            [VALUE_W, ROW_H],
            egui::DragValue::new(v)
                .range(lo..=hi)
                .speed(speed)
                .max_decimals(decimals),
        );
        let fine = ui.input(|i| i.modifiers.shift);
        let s = if fine { step / 10.0 } else { step };
        if small_button(ui, "−").on_hover_text("Decrease (Shift: fine)").clicked() {
            *v = (*v - s).clamp(lo, hi);
        }
        if small_button(ui, "+").on_hover_text("Increase (Shift: fine)").clicked() {
            *v = (*v + s).clamp(lo, hi);
        }
        if slider {
            ui.spacing_mut().slider_width = (ui.available_width() - 4.0).max(30.0);
            let mut x = *v as f64;
            if wheel_slider(ui, &mut x, lo as f64..=hi as f64, Wheel::Linear, false).changed() {
                *v = x as f32;
            }
        }
    });
    row_separator(ui);
    *v != before
}

/// Search box with a clear button (✕, shown while there is text; Esc in the
/// box clears too). Returns the text box's response.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let clear_w = if text.is_empty() {
            0.0
        } else {
            ROW_H + ui.spacing().item_spacing.x
        };
        let r = ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width((ui.available_width() - clear_w).max(40.0)),
        );
        if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            text.clear();
        }
        if !text.is_empty()
            && icon_button(ui, Icon::Close, false, vec2(ROW_H, ROW_H), true)
                .on_hover_text("Clear the search")
                .clicked()
        {
            text.clear();
            r.request_focus();
        }
        r
    })
    .inner
}

/// Collapsible browser category: green caps title with a triangle; click the
/// row to fold. Always open while `searching`. `add` runs when open.
pub fn category<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    searching: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let pal = super::theme::pal(ui);
    let id = ui.make_persistent_id(("browser-category", id_salt));
    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
    ui.add_space(4.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), egui::Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() && !searching {
        st.toggle(ui);
    }
    let open = st.is_open() || searching;
    let openness = if searching { 1.0 } else { st.openness(ui.ctx()) };
    let icon = egui::Rect::from_center_size(rect.left_center() + vec2(6.0, 0.0), vec2(10.0, 10.0));
    egui::collapsing_header::paint_default_icon(ui, openness, &resp.clone().with_new_rect(icon));
    let color = if resp.hovered() { pal.text_strong } else { pal.accent };
    ui.painter().text(
        rect.left_center() + vec2(16.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::TextStyle::Small.resolve(ui.style()),
        color,
    );
    st.store(ui.ctx());
    open.then(|| add(ui))
}
