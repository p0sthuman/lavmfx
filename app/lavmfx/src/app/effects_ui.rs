//! Effect chains in the Composition / Group / Layer / Clip panels, and the
//! per-frame [`FxFrame`] the compositor runs.

use eframe::egui::{self, Color32, RichText};
use lavmfx_core::effects::{self, FxTarget, OPACITY_KEY, format_color, parse_color, shader_chain};
use lavmfx_core::mix::shader_blend;
use lavmfx_core::{Composition, Param, VideoEffect};
use lavmfx_render::EffectDraw;

use super::widgets::{
    LABEL_W, RowOpts, Wheel, band_right, bool_row, category, choice_row, param_row, search_field, section_header,
    section_header_state, wheel_slider,
};

/// Renderer view of a clip: a source's generator first (on a clear layer), then the
/// clip's own effects.
pub fn clip_draws(clip: Option<&lavmfx_core::Clip>) -> Vec<EffectDraw> {
    let Some(v) = clip.and_then(|c| c.video.as_ref()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(lavmfx_core::sources::ClipSource::Generator(name)) = &v.source {
        let empty = lavmfx_core::ParameterCollection::new();
        let params = v.sourceparams.as_ref().unwrap_or(&empty);
        let id = clip.map_or(0, |c| c.id);
        if let Some(f) = lavmfx_core::sources::source_fx(name, params, id) {
            // Text sources rasterise like the Text Block / Text Animator effects.
            let image = lavmfx_core::sources::source_effect_name(name).map_or(0, |fx_name| {
                super::text::image_key(&VideoEffect {
                    id,
                    name: fx_name.into(),
                    display_name: name.clone(),
                    bypassed: None,
                    mixer: Default::default(),
                    params: params.clone(),
                    effect: Default::default(),
                    collapsed: false,
                })
            });
            out.push(EffectDraw {
                kind: f.kind,
                params: f.params,
                opacity: f.opacity,
                blend: f.blend,
                id: f.id,
                feedback: f.feedback,
                passes: f.passes,
                history: f.history,
                image,
            });
        }
    }
    out.extend(draws(Some(&v.track.effects)));
    out
}

/// Renderer view of a model chain.
pub fn draws(chain: Option<&Vec<VideoEffect>>) -> Vec<EffectDraw> {
    chain.map_or_else(Vec::new, |c| {
        shader_chain(c)
            .into_iter()
            .map(|f| EffectDraw {
                image: c.iter().find(|e| e.id == f.id).map_or(0, super::text::image_key),
                kind: f.kind,
                params: f.params,
                opacity: f.opacity,
                blend: f.blend,
                id: f.id,
                feedback: f.feedback,
                passes: f.passes,
                history: f.history,
            })
            .collect()
    })
}

/// What the user did in an effects section (applied after the UI pass).
enum Action {
    Add(String),
    Remove(usize),
    Move(usize, usize),
}

/// An effect band being dragged (reorder, move to another chain; Alt/Ctrl = copy).
#[derive(Debug, Clone, PartialEq)]
pub struct FxMove {
    pub from: FxTarget,
    pub index: usize,
    pub name: String,
}

/// Take the drag payload if it is a `T`. (egui's `take_payload` removes any payload and
/// only then checks the type: trying `FxMove` first used to destroy a browser `FxDrop`.)
pub fn take<T: std::any::Any + Send + Sync>(ctx: &egui::Context) -> Option<std::sync::Arc<T>> {
    if egui::DragAndDrop::has_payload_of_type::<T>(ctx) {
        egui::DragAndDrop::take_payload::<T>(ctx)
    } else {
        None
    }
}

/// Alt or Ctrl held = copy, as in the clip grid.
pub fn copy_held(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.modifiers.alt || i.modifiers.command)
}

/// Label next to the pointer while something is dragged.
pub fn drag_label(ctx: &egui::Context, text: &str) {
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    let pal = super::theme::cur();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("fx-drag-label"),
    ));
    let g = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(13.0), pal.overlay_text);
    let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, 10.0), g.size()).expand(4.0);
    painter.rect_filled(r, 3.0, pal.overlay);
    painter.galley(r.min + egui::vec2(4.0, 4.0), g, pal.overlay_text);
}

/// Layout of one drawn effects section, for drop targeting.
struct SectionLayout {
    header: egui::Rect,
    /// (chain index, top, bottom) of each effect block (Transform excluded).
    rows: Vec<(usize, f32, f32)>,
    /// Chain length.
    len: usize,
}

impl SectionLayout {
    /// Insertion index for a pointer at `y` and where to draw the line.
    fn drop_at(&self, y: f32) -> (usize, f32) {
        for &(i, top, bottom) in &self.rows {
            if y < (top + bottom) * 0.5 {
                return (i, top);
            }
        }
        let y = self.rows.last().map_or(self.header.bottom(), |r| r.2);
        (self.len, y)
    }
}

/// "Effects" section: one collapsible block per effect plus an Add menu.
/// Accepts effect bands (reorder / move / Alt-copy) and browser items
/// dropped between its effects; a line shows where they land.
/// Whole panel as a drop zone: an effect dragged anywhere over the panel `rect`
/// (and not onto its Effects section's drop line) goes to the end of the panel's chain.
pub fn panel_drop(ui: &egui::Ui, comp: &mut Composition, target: FxTarget, rect: egui::Rect) {
    let ctx = ui.ctx().clone();
    let moving = egui::DragAndDrop::payload::<FxMove>(&ctx);
    if moving.is_none() && !egui::DragAndDrop::has_payload_of_type::<FxDrop>(&ctx) {
        return;
    }
    if comp.effects(target).is_none() {
        return; // e.g. a clip without video
    }
    if !ctx.pointer_latest_pos().is_some_and(|p| rect.contains(p)) {
        return;
    }
    let copy = moving.is_some() && copy_held(&ctx);
    let colour = if copy {
        super::theme::cur().accent
    } else {
        super::theme::cur().drop
    };
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("fx-panel-drop"),
    ))
    .rect_stroke(
        rect.shrink(2.0),
        4.0,
        egui::Stroke::new(2.0, colour),
        egui::StrokeKind::Inside,
    );
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    let end = comp.effects(target).map_or(0, Vec::len);
    if let Some(m) = take::<FxMove>(&ctx) {
        comp.transfer_effect(m.from, m.index, target, end, copy);
    } else if let Some(d) = take::<FxDrop>(&ctx) {
        apply_drop(comp, target, &d, None);
    }
}

pub fn effects_section(ui: &mut egui::Ui, comp: &mut Composition, target: FxTarget) {
    let layout = effects_section_inner(ui, comp, target);
    let ctx = ui.ctx().clone();
    let moving = egui::DragAndDrop::payload::<FxMove>(&ctx);
    let browsing = egui::DragAndDrop::has_payload_of_type::<FxDrop>(&ctx);
    if moving.is_none() && !browsing {
        return;
    }
    let area = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), layout.header.top()..=ui.cursor().top())
        .intersect(ui.clip_rect());
    let Some(pos) = ctx.pointer_latest_pos().filter(|p| area.contains(*p)) else {
        return;
    };
    let (at, y) = layout.drop_at(pos.y);
    let copy = moving.is_some() && copy_held(&ctx);
    let colour = if copy {
        super::theme::cur().accent
    } else {
        super::theme::cur().drop
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("fx-drop-line"),
    ));
    let x = area.x_range();
    painter.line_segment(
        [egui::pos2(x.min + 2.0, y), egui::pos2(x.max - 2.0, y)],
        egui::Stroke::new(3.0, colour),
    );
    for cx in [x.min + 2.0, x.max - 2.0] {
        painter.circle_filled(egui::pos2(cx, y), 3.5, colour);
    }
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    if let Some(m) = take::<FxMove>(&ctx) {
        comp.transfer_effect(m.from, m.index, target, at, copy);
    } else if let Some(d) = take::<FxDrop>(&ctx) {
        apply_drop(comp, target, &d, Some(at));
    }
}

fn effects_section_inner(ui: &mut egui::Ui, comp: &mut Composition, target: FxTarget) -> SectionLayout {
    let salt = format!("{target:?}");
    let res = comp.resolution();
    let mut action = None;
    let sec = section_header(ui, ("fx-section", &salt), "Effects", true, false);
    {
        let mut right = band_right(ui, sec.header, 130.0);
        if let Some(a) = add_menu(&mut right, &salt) {
            action = Some(Action::Add(a));
        }
    }
    let mut layout = SectionLayout {
        header: sec.header,
        rows: Vec::new(),
        len: comp.effects(target).map_or(0, Vec::len),
    };
    let mut body = |ui: &mut egui::Ui| {
        let Some(chain) = comp.effects_mut(target) else {
            ui.label(RichText::new("No video here.").weak());
            return;
        };
        let n = chain.len();
        layout.len = n;
        // Transform has its own block (Video section); it stays first.
        let first = chain.first().is_some_and(VideoEffect::is_transform) as usize;
        if n == first {
            ui.label(RichText::new("No effects: + Add effect in the band above, or drop one here.").weak());
        }
        for (i, fx) in chain.iter_mut().enumerate().skip(first) {
            let top = ui.cursor().top();
            if let Some(a) = effect_block(ui, fx, i, (first, n), &salt, res, target) {
                action = Some(a);
            }
            layout.rows.push((i, top, ui.cursor().top()));
        }
    };
    if sec.open {
        body(ui);
    }
    match action {
        Some(Action::Add(name)) => {
            comp.add_effect(target, &name, None);
        }
        Some(Action::Remove(i)) => {
            comp.remove_effect(target, i);
        }
        Some(Action::Move(from, to)) => {
            comp.move_effect(target, from, to);
        }
        None => {}
    }
    layout
}

/// "+ Add effect" menu with a search field; returns the chosen internal name.
fn add_menu(ui: &mut egui::Ui, salt: &str) -> Option<String> {
    let mut chosen = None;
    let filter_id = ui.make_persistent_id(("fx-filter", salt));
    ui.menu_button("+ Add effect", |ui| {
        let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id).unwrap_or_default());
        ui.set_min_width(220.0);
        let r = search_field(ui, &mut filter, "Search…");
        r.request_focus();
        ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
        let f = filter.to_lowercase();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            let mut first = None;
            for s in effects::catalog() {
                if s.name == effects::TRANSFORM || !s.display_name.to_lowercase().contains(&f) {
                    continue;
                }
                first.get_or_insert(&s.name);
                let ready = effects::shader_kind(&s.name).is_some();
                let text = if ready {
                    RichText::new(&s.display_name)
                } else {
                    RichText::new(&s.display_name).weak()
                };
                let b = ui.button(text);
                let b = if ready {
                    b
                } else {
                    b.on_hover_text("Not rendered yet (kept in the file)")
                };
                if b.clicked() {
                    chosen = Some(s.name.clone());
                }
            }
            // ISF effects (Preferences › Video folders + built-ins).
            let isf: Vec<_> = lavmfx_core::isf::shaders()
                .into_iter()
                .filter(|s| s.kind == lavmfx_core::isf::IsfKind::Effect && s.title.to_lowercase().contains(&f))
                .collect();
            if !isf.is_empty() {
                ui.separator();
                ui.label(RichText::new("ISF").small().strong());
            }
            let mut first_isf = None;
            for s in &isf {
                let name = s.name();
                first_isf.get_or_insert(name.clone());
                let b = ui.button(if s.runs() {
                    RichText::new(&s.title)
                } else {
                    RichText::new(&s.title).weak()
                });
                let b = b.on_hover_text(isf_hover(s));
                if b.clicked() && s.runs() {
                    chosen = Some(name);
                }
            }
            let ffgl: Vec<_> = lavmfx_core::ffgl::plugins()
                .into_iter()
                .filter(|p| p.kind == lavmfx_core::ffgl::FfglKind::Effect && p.title.to_lowercase().contains(&f))
                .collect();
            if !ffgl.is_empty() {
                ui.separator();
                ui.label(RichText::new("FFGL").small().strong());
            }
            for p in &ffgl {
                let b = ui.button(if p.runs() {
                    RichText::new(&p.title)
                } else {
                    RichText::new(&p.title).weak()
                });
                let b = b.on_hover_text(ffgl_hover(p));
                if b.clicked() && p.runs() {
                    chosen = Some(p.name());
                }
            }
            if enter && chosen.is_none() {
                chosen = first.cloned().or(first_isf);
            }
        });
        if chosen.is_some() {
            ui.data_mut(|d| d.remove::<String>(filter_id));
            ui.close();
        }
    });
    chosen
}

/// One effect: header (fold, name, opacity slider, ▲ ▼, B, X) and its params.
fn effect_block(
    ui: &mut egui::Ui,
    fx: &mut VideoEffect,
    i: usize,
    (first, n): (usize, usize),
    salt: &str,
    res: (u32, u32),
    target: FxTarget,
) -> Option<Action> {
    let mut action = None;
    let rendered = fx.is_rendered();
    // Effect row: teal band, name, inline opacity rail, ▲▼ B ✕.
    // Open / folded is the effect's own (saved with the project).
    let mut open = !fx.collapsed;
    let sec = section_header_state(
        ui,
        ("fx", salt, fx.id),
        &fx.display_name,
        &mut open,
        true,
        fx.is_bypassed() || !rendered,
    );
    fx.collapsed = !open;
    // Drag the band by its name: reorder, move to another Effects section, Alt/Ctrl = copy.
    sec.title.dnd_set_drag_payload(FxMove {
        from: target,
        index: i,
        name: fx.display_name.clone(),
    });
    if sec.title.dragged() {
        let verb = if copy_held(ui.ctx()) { "Copy" } else { "Move" };
        drag_label(ui.ctx(), &format!("{verb} {}", fx.display_name));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    {
        let w = (sec.header.width() - LABEL_W - 26.0).max(60.0);
        let ui = &mut band_right(ui, sec.header, w);
        ui.spacing_mut().item_spacing.x = 1.0;
        ui.spacing_mut().interact_size.y = 18.0;
        // Cells at the band's right: B · P · ✕ (equal squares).
        let cell = egui::vec2(24.0, sec.header.height() - 2.0);
        if super::widgets::icon_button(ui, super::widgets::Icon::Close, false, cell, true)
            .on_hover_text("Remove effect")
            .clicked()
        {
            action = Some(Action::Remove(i));
        }
        presets_menu(ui, fx, cell);
        if let Some(b) = &mut fx.bypassed {
            super::widgets::toggle_box(ui, &mut b.value, "B", "Bypass", cell);
        }
        ui.add_space(3.0);
        if i + 1 < n && arrow_button(ui, false).on_hover_text("Move down").clicked() {
            action = Some(Action::Move(i, i + 1));
        }
        if i > first && arrow_button(ui, true).on_hover_text("Move up").clicked() {
            action = Some(Action::Move(i, i - 1));
        }
        // Dry/wet opacity, always visible (also when folded).
        if let Some(Param::Range(o)) = fx.params.get_mut(OPACITY_KEY) {
            ui.spacing_mut().slider_width = (ui.available_width() - 4.0).max(30.0);
            let mut v = o.value;
            let r = wheel_slider(ui, &mut v, o.min..=o.max, Wheel::Linear, false);
            if r.changed() {
                o.set(v);
            }
            if r.double_clicked()
                && let Some(d) = o.default
            {
                o.set(d);
            }
            let tip = if rendered {
                format!("Opacity {:.0} % (double-click: reset)", o.value * 100.0)
            } else {
                "Not rendered yet; settings are kept".to_owned()
            };
            r.on_hover_text(tip);
        }
    }
    if sec.open {
        params(ui, fx, salt, res);
    }
    action
}

fn params(ui: &mut egui::Ui, fx: &mut VideoEffect, salt: &str, res: (u32, u32)) {
    let fid = fx.id;
    // Opacity is in the header; then the mixer, then the effect's own params.
    for (k, p) in fx.mixer.iter_mut() {
        if let Param::Choice(c) = p {
            choice_row(ui, k, ("fx-mix", salt, fid, k.as_str()), c, |m| {
                shader_blend(m).is_some_and(|i| i <= 9 && i != 5)
            });
        }
    }
    for (k, p) in fx.params.iter_mut() {
        if k == OPACITY_KEY {
            continue;
        }
        param_widget(ui, k, p, (salt, fid), soft_range(k, res));
    }
}

/// Practical slider range for pixel params with a huge range.
pub fn soft_range(name: &str, (w, h): (u32, u32)) -> Option<(f64, f64)> {
    let (w, h) = (w as f64, h as f64);
    match name {
        "Anchor X" | "Position X" => Some((-w, w)),
        "Anchor Y" | "Position Y" => Some((-h, h)),
        "Anchor Z" => Some((-1000.0, 1000.0)),
        "Left" | "Right" => Some((0.0, w)),
        "Top" | "Bottom" => Some((0.0, h)),
        _ => None,
    }
}

/// Transform block: Position X/Y fields, then Scale, Rotation and
/// Anchor groups (folded = one row for the main param, open = all).
pub fn transform_block(ui: &mut egui::Ui, comp: &mut Composition, target: FxTarget) {
    let res = comp.resolution();
    let Some(t) = comp
        .effects_mut(target)
        .and_then(|c| c.iter_mut().find(|e| e.is_transform()))
    else {
        return;
    };
    let salt = format!("{target:?}");
    let sec = section_header(ui, ("transform", &salt), "Transform", true, true);
    {
        let ui = &mut band_right(ui, sec.header, 110.0);
        ui.spacing_mut().item_spacing.x = 1.0;
        presets_menu(ui, t, egui::vec2(24.0, sec.header.height() - 2.0));
        ui.add_space(3.0);
        {
            if ui
                .add(egui::Button::new(RichText::new("Reset").small()))
                .on_hover_text("All transform values back to default")
                .clicked()
            {
                for p in t.params.values_mut() {
                    if let Param::Range(r) = p
                        && let Some(d) = r.default
                    {
                        r.set(d);
                    }
                }
            }
        }
    }
    let _ = sec.open.then(|| {
        for k in ["Position X", "Position Y"] {
            if let Some(p) = t.params.get_mut(k) {
                param_widget(ui, k, p, &salt, soft_range(k, res));
            }
        }
        let groups: [(&str, &[&str], &str); 3] = [
            ("Scale", &["Scale", "Scale W", "Scale H"], "Scale"),
            ("Rotation", &["Rotation X", "Rotation Y", "Rotation Z"], "Rotation Z"),
            ("Anchor", &["Anchor X", "Anchor Y", "Anchor Z"], "Anchor X"),
        ];
        for (group, keys, main) in groups {
            let gid = ui.make_persistent_id(("transform-group", &salt, group));
            let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), gid, false);
            let open = st.is_open();
            ui.horizontal(|ui| {
                st.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
                if open {
                    ui.label(group);
                } else if let Some(p) = t.params.get_mut(main) {
                    // Vertical, so a drive's controls go under the row, not beside it.
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        param_widget(ui, group, p, (&salt, group), soft_range(main, res));
                    });
                }
            });
            st.store(ui.ctx());
            if open {
                ui.indent(gid, |ui| {
                    for k in keys {
                        if let Some(p) = t.params.get_mut(*k) {
                            param_widget(ui, k, p, &salt, soft_range(k, res));
                        }
                    }
                });
            }
        }
    });
}

/// Row for any parameter type.
pub fn param_widget(
    ui: &mut egui::Ui,
    k: &str,
    p: &mut Param,
    salt: impl std::hash::Hash + std::fmt::Debug,
    soft: Option<(f64, f64)>,
) {
    match p {
        Param::Range(r) => {
            // The R / G / B sliders' value lines are coloured.
            let line = match k {
                "R" | "Red" => Some(egui::Color32::from_rgb(214, 64, 64)),
                "G" | "Green" => Some(egui::Color32::from_rgb(64, 170, 72)),
                "B" | "Blue" => Some(egui::Color32::from_rgb(70, 110, 220)),
                _ => None,
            };
            param_row(
                ui,
                k,
                r,
                RowOpts {
                    animate: true,
                    soft,
                    line,
                    ..Default::default()
                },
            );
        }
        Param::Choice(c) => {
            let buttons = c
                .view
                .as_ref()
                .is_some_and(|v| v.control_type == Some(lavmfx_core::ControlType::ChoiceButtons));
            if buttons && c.options.len() <= 8 {
                // A row of segmented buttons (e.g. Shift RGB › Mode).
                labelled(ui, k, |ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    let w = ((ui.available_width() - 4.0) / c.options.len() as f32).max(24.0);
                    for i in 0..c.options.len() {
                        let b = egui::Button::new(RichText::new(&c.options[i]).small())
                            .selected(c.index == i as i32)
                            .min_size(egui::vec2(w, 20.0));
                        if ui.add(b).clicked() {
                            c.select(i);
                        }
                    }
                });
            } else {
                choice_row(ui, k, ("fx-choice", &salt, k), c, |_| true);
            }
        }
        Param::Bool(b) => {
            bool_row(ui, k, &mut b.value, "");
        }
        Param::Color(c) => {
            labelled(ui, k, |ui| {
                let [r, g, b, a] = parse_color(&c.value).map(|v| (v * 255.0).round() as u8);
                let mut col = Color32::from_rgba_unmultiplied(r, g, b, a);
                if egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::OnlyBlend)
                    .changed()
                {
                    let [r, g, b, a] = col.to_srgba_unmultiplied();
                    c.value = format_color([r, g, b, a].map(|v| v as f32 / 255.0));
                }
                ui.label(RichText::new(&c.value).weak().small());
            });
        }
        Param::Int(i) => {
            labelled(ui, k, |ui| {
                ui.add(egui::DragValue::new(&mut i.value));
            });
        }
        Param::String(s) => {
            labelled(ui, k, |ui| {
                ui.text_edit_singleline(&mut s.value);
            });
        }
        Param::Text(t) if k == lavmfx_core::effects::MASK_FILE_KEY => {
            // Mask effect: the image file.
            labelled(ui, k, |ui| {
                if ui.button("Load…").on_hover_text("Image used as the mask").clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tif", "tiff", "webp", "gif"])
                        .pick_file()
                {
                    t.value = p.to_string_lossy().into();
                }
                if !t.value.is_empty() && ui.small_button("×").on_hover_text("No image (no masking)").clicked() {
                    t.value.clear();
                }
                let name = std::path::Path::new(&t.value)
                    .file_name()
                    .map_or_else(|| "No image".to_string(), |n| n.to_string_lossy().to_string());
                ui.add(egui::Label::new(RichText::new(name).weak()).truncate())
                    .on_hover_text(&t.value);
            });
        }
        Param::Text(t) => {
            labelled(ui, k, |ui| {
                ui.text_edit_multiline(&mut t.value);
            });
        }
        Param::Event(ev) => {
            // A button: queued until the frame reads it (Slice Chase's Next / Reset…).
            let wired = lavmfx_core::chase::EVENT_KEYS.contains(&k);
            labelled(ui, k, |ui| {
                let r = ui.button(k);
                let r = if wired { r } else { r.on_hover_text("Not wired yet") };
                if r.clicked() {
                    lavmfx_core::trigger::fire(ev.id);
                }
            });
        }
    }
}

/// Small button with a drawn up/down triangle (the default fonts lack ▲▼).
fn arrow_button(ui: &mut egui::Ui, up: bool) -> egui::Response {
    let r = ui.add(egui::Button::new("").min_size(egui::vec2(16.0, 16.0)));
    let c = r.rect.center();
    let (dy, s) = (if up { -1.0 } else { 1.0 }, 3.5);
    let pts = vec![
        egui::pos2(c.x - s, c.y - dy * s * 0.6),
        egui::pos2(c.x + s, c.y - dy * s * 0.6),
        egui::pos2(c.x, c.y + dy * s * 0.8),
    ];
    let col = ui.style().interact(&r).fg_stroke.color;
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
    r
}

/// `name · widget` row aligned with param rows.
fn labelled(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_W, 18.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.set_width(LABEL_W);
                super::widgets::row_label(ui, label);
            },
        );
        body(ui);
    });
}

/// The P cell on an effect band: "Factory Presets" (Default + the shipped
/// presets), "My Presets", then Manage… and Save.
fn presets_menu(ui: &mut egui::Ui, fx: &mut VideoEffect, size: egui::Vec2) {
    use super::presets;
    let pal = super::theme::pal(ui);
    let mine = presets::list(fx);
    let builtin = lavmfx_core::effects::builtin_presets(&fx.name);
    let text = if mine.is_empty() {
        RichText::new("P")
    } else {
        RichText::new("P").color(pal.accent)
    };
    let r = ui
        .add(egui::Button::new(text).fill(pal.button).min_size(size))
        .on_hover_text(format!("{} presets", fx.display_name));
    let save_id = egui::Id::unique("fx-preset-save");
    let manage_id = egui::Id::unique("fx-preset-manage");
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(170.0);
        let heading = |ui: &mut egui::Ui, t: &str| {
            ui.label(RichText::new(t).small().color(pal.text_weak));
        };
        heading(ui, "Factory Presets");
        if ui.button("Default").clicked() {
            fx.reset_defaults();
            ui.close();
        }
        for bp in builtin {
            if ui.button(&bp.name).clicked() {
                fx.apply_builtin(bp);
                ui.close();
            }
        }
        if !mine.is_empty() {
            ui.separator();
            heading(ui, "My Presets");
            for pr in &mine {
                if ui.button(&pr.name).clicked() {
                    presets::apply(pr, fx);
                    ui.close();
                }
            }
        }
        ui.separator();
        if ui.add_enabled(!mine.is_empty(), egui::Button::new("Manage…")).clicked() {
            ui.data_mut(|d| d.insert_temp(manage_id, Some(fx.id)));
            ui.close();
        }
        if ui.button("Save").clicked() {
            ui.data_mut(|d| d.insert_temp(save_id, Some((fx.id, String::new()))));
            ui.close();
        }
    });

    // Save dialog (for this effect instance).
    let save: Option<(lavmfx_core::Id, String)> = ui.data(|d| d.get_temp(save_id)).flatten();
    if let Some((id, mut name)) = save.filter(|(id, _)| *id == fx.id) {
        let mut keep = true;
        let m = egui::Modal::new(egui::Id::unique("fx-preset-save-modal")).show(ui.ctx(), |ui| {
            ui.set_width(300.0);
            ui.heading(format!("Save {} preset", fx.display_name));
            let t = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .hint_text("Preset name")
                    .desired_width(280.0),
            );
            t.request_focus();
            ui.label(
                RichText::new("Saves every parameter of this effect, incl. animations.")
                    .small()
                    .weak(),
            );
            ui.horizontal(|ui| {
                let ok = !name.trim().is_empty();
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ok && (ui.button("Save").clicked() || enter) {
                    presets::save(fx, name.trim());
                    keep = false;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
        });
        if m.should_close() {
            keep = false;
        }
        ui.data_mut(|d| d.insert_temp(save_id, keep.then_some((id, name))));
    }

    // Manage dialog: rename / delete my presets.
    let manage: Option<lavmfx_core::Id> = ui.data(|d| d.get_temp(manage_id)).flatten();
    if manage == Some(fx.id) {
        let mut keep = true;
        let m = egui::Modal::new(egui::Id::unique("fx-preset-manage-modal")).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.heading(format!("{} presets", fx.display_name));
            let list = presets::list(fx);
            if list.is_empty() {
                ui.label(RichText::new("No presets saved.").weak());
            }
            for pr in &list {
                ui.horizontal(|ui| {
                    let edit_id = ui.make_persistent_id(("rename", &pr.name));
                    let mut text: String = ui.data(|d| d.get_temp(edit_id)).unwrap_or_else(|| pr.name.clone());
                    let t = ui.add(egui::TextEdit::singleline(&mut text).desired_width(200.0));
                    if t.lost_focus() && text != pr.name {
                        presets::rename(fx, &pr.name, &text);
                        ui.data_mut(|d| d.remove::<String>(edit_id));
                    } else {
                        ui.data_mut(|d| d.insert_temp(edit_id, text));
                    }
                    if super::widgets::icon_button(ui, super::widgets::Icon::Close, false, egui::vec2(18.0, 18.0), true)
                        .on_hover_text("Delete")
                        .clicked()
                    {
                        presets::delete(fx, &pr.name);
                    }
                });
            }
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                keep = false;
            }
        });
        if m.should_close() {
            keep = false;
        }
        if !keep {
            ui.data_mut(|d| d.insert_temp::<Option<lavmfx_core::Id>>(manage_id, None));
        }
    }
}

// ------------------------------------------------------------ browser

/// Which preset a browser item carries.
#[derive(Debug, Clone, PartialEq)]
pub enum PresetRef {
    Builtin(String),
    Mine(String),
}

/// Drag payload of the Effects browser: an effect, optionally with a preset.
/// A source (generator) dragged from the Sources browser.
#[derive(Debug, Clone)]
pub struct SourceDrop {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxDrop {
    /// Internal effect name.
    pub effect: String,
    pub preset: Option<PresetRef>,
}

/// A stand-alone effect for a browser item (preset recalled), e.g. to try
/// on the Preview Monitor.
pub fn make_effect(ids: &mut lavmfx_core::IdGen, d: &FxDrop) -> Option<VideoEffect> {
    let mut fx = VideoEffect::from_catalog(ids, &d.effect)?;
    match &d.preset {
        None => {}
        Some(PresetRef::Builtin(n)) => {
            if let Some(p) = effects::builtin_presets(&fx.name).iter().find(|p| &p.name == n) {
                fx.apply_builtin(p);
            }
        }
        Some(PresetRef::Mine(n)) => {
            if let Some(p) = super::presets::list(&fx).iter().find(|p| &p.name == n) {
                super::presets::apply(p, &mut fx);
            }
        }
    }
    Some(fx)
}

/// Add the dropped effect (and recall its preset) to `target`'s chain,
/// at `at` (None = the end).
pub fn apply_drop(comp: &mut Composition, target: FxTarget, d: &FxDrop, at: Option<usize>) -> bool {
    let at = at.map(|a| {
        let first = comp
            .effects(target)
            .and_then(|c| c.first())
            .is_some_and(VideoEffect::is_transform) as usize;
        a.max(first)
    });
    let Some(id) = comp.add_effect(target, &d.effect, at) else {
        return false;
    };
    let Some(fx) = comp.effects_mut(target).and_then(|c| c.iter_mut().find(|e| e.id == id)) else {
        return false;
    };
    match &d.preset {
        None => {}
        Some(PresetRef::Builtin(n)) => {
            if let Some(p) = effects::builtin_presets(&fx.name).iter().find(|p| &p.name == n) {
                fx.apply_builtin(p);
            }
        }
        Some(PresetRef::Mine(n)) => {
            if let Some(p) = super::presets::list(fx).iter().find(|p| &p.name == n) {
                super::presets::apply(p, fx);
            }
        }
    }
    true
}

/// Sources browser: search, then collapsible groups (video sources,
/// NDI, shared video, ISF). Drag an item onto a clip slot; double-click returns
/// it (the caller loads it into the selected slot).
pub fn sources_browser(ui: &mut egui::Ui) -> Option<String> {
    let pal = super::theme::pal(ui);
    let mut picked = None;
    let filter_id = ui.make_persistent_id("src-browser-filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    search_field(ui, &mut filter, "Search sources");
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    let f = filter.to_lowercase();
    let searching = !f.is_empty();
    let hit = |t: &str| !searching || t.to_lowercase().contains(&f);
    // One draggable row; Some(payload) when double-clicked.
    let item =
        |ui: &mut egui::Ui, label: &str, ready: bool, hover: &str, drag: &str, payload: &str| -> Option<String> {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let text = RichText::new(label).color(if ready { pal.text_strong } else { pal.text_weak });
                let r = ui
                    .add(
                        egui::Label::new(text)
                            .selectable(false)
                            .sense(egui::Sense::click_and_drag()),
                    )
                    .on_hover_text(hover);
                if r.hovered() {
                    ui.painter()
                        .rect_filled(r.rect.expand(1.0), 2.0, pal.button.gamma_multiply(0.5));
                }
                if !ready {
                    return None;
                }
                r.dnd_set_drag_payload(SourceDrop {
                    name: payload.to_owned(),
                });
                if r.dragged() {
                    drag_label(ui.ctx(), drag);
                }
                r.double_clicked().then(|| payload.to_owned())
            })
            .inner
        };
    let note = |ui: &mut egui::Ui, t: &str| {
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.add(egui::Label::new(RichText::new(t).small().color(pal.text_weak)).wrap());
        });
    };
    let load_hint = "Drag onto a clip slot · double-click: load into the selected slot";
    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    // Debug: `LAVMFX_SCREENSHOT_SOURCES=end` shows the end of the list.
    if std::env::var("LAVMFX_SCREENSHOT_SOURCES").is_ok_and(|v| v == "end") {
        area = area.vertical_scroll_offset(470.0);
    }
    area.show(ui, |ui| {
        category(ui, "src-video", "VIDEO SOURCES", searching, |ui| {
            for s in lavmfx_core::sources::catalog().iter().filter(|s| hit(&s.name)) {
                let ready = s.is_rendered();
                let hover = if ready {
                    load_hint
                } else {
                    "Not rendered yet (needs slices / other layers)"
                };
                picked = item(ui, &s.name, ready, hover, &format!("Source {}", s.name), &s.name).or(picked.take());
            }
        });
        category(ui, "src-capture", "CAPTURE DEVICES", searching, |ui| {
            let list = lavmfx_media::capture::devices();
            if list.is_empty() {
                note(ui, "No camera or capture card found");
            }
            for d in list.iter().filter(|d| hit(&d.label)) {
                let payload = format!("{}{}\u{1f}{}", lavmfx_core::sources::CAPTURE_PREFIX, d.id, d.label);
                let hover = format!("Capture device {} · {load_hint}", d.id);
                picked = item(ui, &d.label, true, &hover, &format!("Camera {}", d.label), &payload).or(picked.take());
            }
        });
        category(
            ui,
            "src-ndi",
            "NDI",
            searching,
            |ui| match lavmfx_media::ndi::runtime_error() {
                Some(e) => note(ui, &e),
                None => {
                    let list = lavmfx_media::ndi::sources();
                    if list.is_empty() {
                        note(ui, "Searching the network…");
                    }
                    for n in list.iter().filter(|n| hit(n)) {
                        let payload = format!("{}{n}", lavmfx_core::sources::NDI_PREFIX);
                        let hover = format!("NDI source · {load_hint}");
                        picked = item(ui, n, true, &hover, &format!("NDI {n}"), &payload).or(picked.take());
                    }
                }
            },
        );
        let backend = lavmfx_media::share::backend();
        category(
            ui,
            "src-share",
            &backend.to_uppercase(),
            searching,
            |ui| match lavmfx_media::share::unavailable() {
                Some(e) => note(ui, &e),
                None => {
                    let list = lavmfx_media::share::sources();
                    if list.is_empty() {
                        note(ui, "No app is sharing video");
                    }
                    for src in list.iter().filter(|s| hit(&s.label)) {
                        let payload = format!("{}{}\u{1f}{}", lavmfx_core::sources::SHARE_PREFIX, src.name, src.label);
                        let hover = format!("{backend}: {} · {load_hint}", src.name);
                        let drag = format!("{backend} {}", src.label);
                        picked = item(ui, &src.label, true, &hover, &drag, &payload).or(picked.take());
                    }
                }
            },
        );
        let isf: Vec<_> = lavmfx_core::isf::shaders()
            .into_iter()
            .filter(|s| s.kind == lavmfx_core::isf::IsfKind::Source && hit(&s.title))
            .collect();
        if !isf.is_empty() {
            category(ui, "src-isf", "ISF", searching, |ui| {
                for s in isf {
                    let hover = format!("{}\n\n{load_hint}", isf_hover(&s));
                    picked = item(
                        ui,
                        &s.title,
                        s.runs(),
                        &hover,
                        &format!("Source {}", s.title),
                        &s.name(),
                    )
                    .or(picked.take());
                }
            });
        }
        let ffgl: Vec<_> = lavmfx_core::ffgl::plugins()
            .into_iter()
            .filter(|p| p.kind == lavmfx_core::ffgl::FfglKind::Source && hit(&p.title))
            .collect();
        if !ffgl.is_empty() {
            category(ui, "src-ffgl", "FFGL", searching, |ui| {
                for p in ffgl {
                    let hover = format!("{}\n\n{load_hint}", ffgl_hover(&p));
                    let drag = format!("Source {}", p.title);
                    picked = item(ui, &p.title, p.runs(), &hover, &drag, &p.name()).or(picked.take());
                }
            });
        }
    });
    picked
}

/// Hover text of an FFGL browser item: id, params, file, or why it doesn't run.
pub fn ffgl_hover(p: &lavmfx_core::ffgl::FfglPlugin) -> String {
    let mut t = String::new();
    if let Some(e) = &p.error {
        t += &format!("Doesn't run: {e}\n\n");
    }
    t += &format!("FFGL plugin {} · {} params\n{}", p.id, p.params.len(), p.path.display());
    t
}

/// Hover text of an ISF browser item: description, credit, file, or why it doesn't run.
pub fn isf_hover(s: &lavmfx_core::isf::IsfShader) -> String {
    let mut t = String::new();
    if let Some(e) = &s.error {
        t += &format!("Doesn't run: {e}\n\n");
    }
    if !s.description.is_empty() {
        t += &format!("{}\n", s.description);
    }
    if !s.credit.is_empty() {
        t += &format!("By {}\n", s.credit);
    }
    t += &s
        .path
        .as_ref()
        .map_or("Built-in ISF example".into(), |p| p.display().to_string());
    t
}

/// Effects browser: search, then
/// VIDEO EFFECTS with presets as children. Drag an item onto an Effects
/// section or a clip slot (double-click no longer adds it anywhere).
/// Double-click = try the item on the Preview Monitor (`auditioning` = the
/// one on it, highlighted). Returns (double-clicked item, an item clicked
/// this frame).
pub fn browser(ui: &mut egui::Ui, auditioning: Option<&FxDrop>) -> (Option<FxDrop>, Option<FxDrop>) {
    let pal = super::theme::pal(ui);
    let mut picked = None;
    let clicked: std::cell::RefCell<Option<FxDrop>> = Default::default();
    let filter_id = ui.make_persistent_id("fx-browser-filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    search_field(ui, &mut filter, "Search effects and presets");
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    let f = filter.to_lowercase();
    let searching = !f.is_empty();
    let hint = "Drag onto a panel, an Effects section or a clip · double-click: try it on the Preview Monitor";
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        category(ui, "fx-video", "VIDEO EFFECTS", searching, |ui| {
            for s in effects::catalog() {
                if s.name == effects::TRANSFORM {
                    continue;
                }
                let fx_probe = lavmfx_core::VideoEffect {
                    id: 0,
                    name: s.name.clone(),
                    display_name: s.display_name.clone(),
                    bypassed: None,
                    mixer: Default::default(),
                    params: Default::default(),
                    effect: Default::default(),
                    collapsed: false,
                };
                let builtin = effects::builtin_presets(&s.name);
                let mine = super::presets::list(&fx_probe);
                let name_hit = f.is_empty() || s.display_name.to_lowercase().contains(&f);
                let preset_hit = |n: &str| f.is_empty() || name_hit || n.to_lowercase().contains(&f);
                let any_preset =
                    builtin.iter().any(|p| preset_hit(&p.name)) || mine.iter().any(|p| preset_hit(&p.name));
                if !name_hit && !any_preset {
                    continue;
                }
                let ready = effects::shader_kind(&s.name).is_some();
                let item = |ui: &mut egui::Ui, text: RichText, payload: FxDrop, indent: f32| -> Option<FxDrop> {
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        let r = ui.add(
                            egui::Label::new(text)
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        );
                        let r = if ready {
                            r
                        } else {
                            r.on_hover_text("Not rendered yet (kept in the file)")
                        };
                        let r = r.on_hover_text(hint);
                        if auditioning == Some(&payload) {
                            // On the Preview Monitor now.
                            ui.painter().rect_stroke(
                                r.rect.expand(1.5),
                                2.0,
                                (1.5, pal.accent),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if r.hovered() {
                            ui.painter()
                                .rect_filled(r.rect.expand(1.0), 2.0, pal.button.gamma_multiply(0.5));
                        }
                        if r.clicked() || r.double_clicked() {
                            *clicked.borrow_mut() = Some(payload.clone());
                        }
                        r.dnd_set_drag_payload(payload.clone());
                        r.double_clicked().then_some(payload)
                    })
                    .inner
                };
                let text = |t: &str, weak: bool| {
                    if weak || !ready {
                        RichText::new(t).color(pal.text_weak)
                    } else {
                        RichText::new(t).color(pal.text_strong)
                    }
                };
                let base = FxDrop {
                    effect: s.name.clone(),
                    preset: None,
                };
                if builtin.is_empty() && mine.is_empty() {
                    if let Some(p) = item(ui, text(&s.display_name, false), base, 16.0) {
                        picked = Some(p);
                    }
                    continue;
                }
                let id = ui.make_persistent_id(("fx-browser", &s.name));
                let open = !f.is_empty() && any_preset;
                let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
                if open {
                    st.set_open(true);
                }
                st.show_header(ui, |ui| {
                    if let Some(p) = item(ui, text(&s.display_name, false), base.clone(), 0.0) {
                        picked = Some(p);
                    }
                })
                .body(|ui| {
                    for bp in builtin.iter().filter(|p| preset_hit(&p.name)) {
                        let d = FxDrop {
                            preset: Some(PresetRef::Builtin(bp.name.clone())),
                            ..base.clone()
                        };
                        if let Some(p) = item(ui, text(&bp.name, true), d, 0.0) {
                            picked = Some(p);
                        }
                    }
                    for mp in mine.iter().filter(|p| preset_hit(&p.name)) {
                        let d = FxDrop {
                            preset: Some(PresetRef::Mine(mp.name.clone())),
                            ..base.clone()
                        };
                        if let Some(p) = item(ui, RichText::new(&mp.name).color(pal.accent), d, 0.0) {
                            picked = Some(p);
                        }
                    }
                });
            }
        });
        // Plug-in effects: ISF shaders, FFGL plugins — (title, model name, runs, hover).
        let isf: Vec<(String, String, bool, String)> = lavmfx_core::isf::shaders()
            .into_iter()
            .filter(|s| s.kind == lavmfx_core::isf::IsfKind::Effect)
            .filter(|s| f.is_empty() || s.title.to_lowercase().contains(&f))
            .map(|s| (s.title.clone(), s.name(), s.runs(), isf_hover(&s)))
            .collect();
        let ffgl: Vec<(String, String, bool, String)> = lavmfx_core::ffgl::plugins()
            .into_iter()
            .filter(|p| p.kind == lavmfx_core::ffgl::FfglKind::Effect)
            .filter(|p| f.is_empty() || p.title.to_lowercase().contains(&f))
            .map(|p| (p.title.clone(), p.name(), p.runs(), ffgl_hover(&p)))
            .collect();
        for (salt, title, list) in [("fx-isf", "ISF", isf), ("fx-ffgl", "FFGL", ffgl)] {
            if list.is_empty() {
                continue;
            }
            category(ui, salt, title, searching, |ui| {
                for (label, name, runs, hover) in list {
                    let payload = FxDrop {
                        effect: name,
                        preset: None,
                    };
                    let text = RichText::new(&label).color(if runs { pal.text_strong } else { pal.text_weak });
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        let r = ui.add(
                            egui::Label::new(text)
                                .selectable(false)
                                .sense(egui::Sense::click_and_drag()),
                        );
                        let r = r.on_hover_text(format!("{hover}\n\n{hint}"));
                        if !runs {
                            return;
                        }
                        if auditioning == Some(&payload) {
                            ui.painter().rect_stroke(
                                r.rect.expand(1.5),
                                2.0,
                                (1.5, pal.accent),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if r.hovered() {
                            ui.painter()
                                .rect_filled(r.rect.expand(1.0), 2.0, pal.button.gamma_multiply(0.5));
                        }
                        if r.clicked() || r.double_clicked() {
                            *clicked.borrow_mut() = Some(payload.clone());
                        }
                        r.dnd_set_drag_payload(payload.clone());
                        if r.double_clicked() {
                            picked = Some(payload);
                        }
                    });
                }
            });
        }
    });
    // What is being dragged, next to the pointer.
    if let Some(d) = egui::DragAndDrop::payload::<FxDrop>(ui.ctx()) {
        let name = effects::spec(&d.effect).map_or_else(
            || {
                d.effect
                    .strip_prefix(lavmfx_core::isf::PREFIX)
                    .unwrap_or(&d.effect)
                    .to_owned()
            },
            |s| s.display_name.clone(),
        );
        let label = match &d.preset {
            Some(PresetRef::Builtin(n) | PresetRef::Mine(n)) => format!("{name} › {n}"),
            None => name,
        };
        drag_label(ui.ctx(), &label);
    }
    (picked, clicked.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(comp: &Composition) -> String {
        let fx = comp
            .effects(FxTarget::Composition)
            .unwrap()
            .iter()
            .find(|e| e.name == "TextBlock")
            .unwrap();
        match fx.params.get("Text") {
            Some(Param::Text(t)) => format!("Text:{}", t.value),
            Some(Param::String(t)) => format!("String:{}", t.value),
            p => format!("{p:?}"),
        }
    }

    /// Click down the value column, type after each click: the Text param must take the typing.
    #[test]
    fn text_param_accepts_typing() {
        let mut comp = Composition::new("T", 1, 1);
        comp.add_effect(FxTarget::Composition, "Text Block", None);
        let before = text_of(&comp);
        let ctx = egui::Context::default();
        let base = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 1400.0))),
            ..Default::default()
        };
        let run = |comp: &mut Composition, events: Vec<egui::Event>| {
            let mut i = base.clone();
            i.events = events;
            let mut out = ctx.run_ui(i, |ui| effects_section(ui, comp, FxTarget::Composition));
            out.textures_delta.clear();
        };
        run(&mut comp, vec![]);
        let mut hit = None;
        for y in (0..1400).step_by(6) {
            let pos = egui::pos2(LABEL_W + 40.0, y as f32);
            let btn = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            run(&mut comp, vec![egui::Event::PointerMoved(pos), btn(true)]);
            run(&mut comp, vec![btn(false)]);
            run(&mut comp, vec![egui::Event::Text("Z".into())]);
            if text_of(&comp).contains('Z') {
                hit = Some(y);
                break;
            }
        }
        assert!(
            hit.is_some(),
            "typing never reached the Text param (was {before}, now {})",
            text_of(&comp)
        );
    }

    /// Press on a browser item, drag across, release over the panel (outside its Effects
    /// section): the effect must land in the panel's chain.
    #[test]
    fn browser_drag_drops_on_panel() {
        let mut comp = Composition::new("T", 1, 1);
        let ctx = egui::Context::default();
        let base = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            ..Default::default()
        };
        let t = std::cell::Cell::new(0.0);
        let run = |comp: &mut Composition, events: Vec<egui::Event>| {
            let mut i = base.clone();
            t.set(t.get() + 1.0 / 60.0);
            i.time = Some(t.get());
            i.events = events;
            let mut out = ctx.run_ui(i, |ui| {
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(300.0, 600.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let _ = browser(ui, None);
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(480.0, 600.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            let rect = ui.max_rect();
                            effects_section(ui, comp, FxTarget::Composition);
                            panel_drop(ui, comp, FxTarget::Composition, rect);
                        },
                    );
                });
            });
            out.textures_delta.clear();
        };
        let ev = |pos, pressed: Option<bool>| {
            let mut v = vec![egui::Event::PointerMoved(pos)];
            if let Some(p) = pressed {
                v.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: p,
                    modifiers: Default::default(),
                });
            }
            v
        };
        run(&mut comp, vec![]);
        let n0 = comp.effects(FxTarget::Composition).map_or(0, Vec::len);
        let mut landed = false;
        for y in (60..400).step_by(8) {
            let start = egui::pos2(40.0, y as f32);
            run(&mut comp, ev(start, None));
            run(&mut comp, ev(start, Some(true)));
            for k in 1..=10 {
                let p = egui::pos2(40.0 + 60.0 * k as f32, y as f32 + 150.0);
                run(&mut comp, ev(p, None));
            }
            let end = egui::pos2(640.0, 500.0);
            run(&mut comp, ev(end, None));
            run(&mut comp, ev(end, Some(false)));
            run(&mut comp, vec![]);
            if comp.effects(FxTarget::Composition).map_or(0, Vec::len) > n0 {
                landed = true;
                break;
            }
        }
        assert!(landed, "no browser drag reached the panel");
    }

    /// Long names (Sine Wave's "Modulation Depth", "Modulator Speed") stay in their label box:
    /// no row reaches left of the panel (that shifted every later row out of view).
    #[test]
    fn long_param_names_stay_in_the_panel() {
        let mut ids = lavmfx_core::IdGen::new();
        let mut params = lavmfx_core::sources::spec("Sine Wave").unwrap().instantiate(&mut ids);
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(260.0, 900.0),
            )),
            ..Default::default()
        };
        let mut left = f32::MAX;
        let mut out = ctx.run_ui(input, |ui| {
            for (k, p) in params.iter_mut() {
                param_widget(ui, k, p, "t", None);
            }
            left = ui.min_rect().left();
        });
        out.textures_delta.clear();
        assert!(left >= 0.0, "rows reach {left} px left of the panel");
    }
}
