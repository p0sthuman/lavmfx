//! DMX in the Advanced Output editor: lumiverses
//! and their fixtures in the tree, fixtures placed over the composition on
//! the Input Selection tab, the DMX Output tab (channel map: drag fixtures to
//! other start channels, overlaps in red) and their properties with a small
//! personality editor (Fixture Editor) and the fixture library.

use lavmfx_core::dmx::{Block, ColorSpace, Corner, Fixture, Lumiverse, Personality, PixelBlock, Protocol, UNIVERSE};

use super::*;

/// A fixture drag on the Input Selection tab.
#[derive(Debug, Clone, Copy)]
pub(super) enum DmxGrab {
    /// Move (start quad, start pointer in composition px).
    Move([Pt; 4], Pt),
    /// Resize by a corner (TL, TR, BR, BL) against the opposite one.
    Corner(usize, [Pt; 4]),
    Pan,
    /// DMX Output tab: (start channel when grabbed, channel under the pointer).
    Channel(u32, i64),
}

/// Where the fixture library lives: an installed app's (read only), then ours.
fn library_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(prefs) = import_prefs_dir()
        && let Some(docs) = prefs.parent()
    {
        v.push(docs.join("Fixture Library"));
        // …/drive_c/users/<u>/Documents/Resolume Arena → …/drive_c/Program Files/…
        if let Some(c) = docs.ancestors().nth(4) {
            v.push(c.join("Program Files/Resolume Arena/default/Fixture Library"));
        }
    }
    v.push(user_library());
    v
}

fn user_library() -> std::path::PathBuf {
    super::super::config_dir().join("fixtures")
}

fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1]]
}
fn mul(a: Pt, k: f32) -> Pt {
    [a[0] * k, a[1] * k]
}
fn dot(a: Pt, b: Pt) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}
fn norm(a: Pt) -> Pt {
    let l = dot(a, a).sqrt();
    if l > 1e-6 { mul(a, 1.0 / l) } else { [1.0, 0.0] }
}

/// Quad resized by dragging corner `i` to `p`, the opposite corner fixed,
/// keeping the quad's rotation (at least 1 px each way).
fn resize_corner(start: &[Pt; 4], i: usize, p: Pt) -> [Pt; 4] {
    let ux = norm(sub(start[1], start[0]));
    let uy = norm(sub(start[3], start[0]));
    let o = start[(i + 2) % 4];
    let d = sub(p, o);
    let (mut a, mut b) = (dot(d, ux), dot(d, uy));
    // The dragged corner stays on its side of the opposite one.
    let (sx, sy) = match i {
        0 => (-1.0, -1.0),
        1 => (1.0, -1.0),
        2 => (1.0, 1.0),
        _ => (-1.0, 1.0),
    };
    a = sx * (a * sx).max(1.0);
    b = sy * (b * sy).max(1.0);
    let (x0, x1) = (a.min(0.0), a.max(0.0));
    let (y0, y1) = (b.min(0.0), b.max(0.0));
    let at = |x: f32, y: f32| add(o, add(mul(ux, x), mul(uy, y)));
    [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]
}

/// RGB of each pixel of `block` (grid order) from sent channels starting at
/// `ch` (0-based) — the Fixture Output Preview.
fn unpack(block: &PixelBlock, channels: &[u8], ch: usize) -> Vec<Color32> {
    let (w, h) = (block.width.clamp(1, 512) as usize, block.height.clamp(1, 512) as usize);
    let n = block.color.channels();
    let mut out = vec![Color32::BLACK; w * h];
    for (k, (x, y)) in block.distribution.order(w, h).into_iter().enumerate() {
        let at = ch + k * n;
        let Some(v) = channels.get(at..at + n) else { break };
        let c = match block.color {
            ColorSpace::Mono => [v[0]; 3],
            ColorSpace::Rgb => [v[0], v[1], v[2]],
            ColorSpace::Rbg => [v[0], v[2], v[1]],
            ColorSpace::Grb => [v[1], v[0], v[2]],
            ColorSpace::Gbr => [v[2], v[0], v[1]],
            ColorSpace::Brg => [v[1], v[2], v[0]],
            ColorSpace::Bgr => [v[2], v[1], v[0]],
            ColorSpace::Rgbw => [
                v[0].saturating_add(v[3]),
                v[1].saturating_add(v[3]),
                v[2].saturating_add(v[3]),
            ],
            ColorSpace::Grbw => [
                v[1].saturating_add(v[3]),
                v[0].saturating_add(v[3]),
                v[2].saturating_add(v[3]),
            ],
        };
        out[y * w + x] = Color32::from_rgb(c[0], c[1], c[2]);
    }
    out
}

/// A colour per fixture on the channel map.
fn fixture_color(i: usize) -> Color32 {
    let h = (i as f32 * 0.618_034).fract();
    egui::epaint::Hsva::new(h, 0.55, 0.75, 1.0).into()
}

pub(super) fn help(ed: &Editor) -> String {
    match (ed.help, ed.tab) {
        (HelpArea::Tree, _) => "Lumiverses (DMX universes sent to an Art-Net / sACN node) with their fixtures. Checkbox = on/off (an off lumiverse sends black); the arrow folds it (also hides its fixtures on Input Selection). Right-click: add fixtures, duplicate, delete.".into(),
        (HelpArea::Properties, _) => "Lumiverse: protocol, node IP and universe, frame rate, colour over all its fixtures. Fixture: its personality (pick one from the library or edit it here, then Save to Library), start channel, input area, colour; the preview shows what it sends.".into(),
        (_, Tab::Input) => "Input Selection: the part of the composition each fixture samples (one point per pixel, at the pixel's centre). Drag inside to move, squares to resize; Delete removes the selected fixture; arrows nudge (Shift = 10 px). Middle-drag pans, Ctrl+wheel zooms.".into(),
        (_, Tab::Output) => "DMX Output: the lumiverse's channels (32 per row, 512 per universe). Each fixture is a coloured run starting at its start channel; red = overlapping fixtures. Drag a run to move the fixture to other channels; hover a channel for its value.".into(),
    }
}

impl LavmfxApp {
    /// Keep the DMX sender in step with the setup (runs while lumiverses exist).
    pub(in crate::app) fn tick_dmx(&mut self) {
        let lumis = &self.outputs.setup.lumiverses;
        if crate::dmx::DmxOut::wanted(lumis) {
            let d = self.live_out.dmx.get_or_insert_with(crate::dmx::DmxOut::new);
            d.configure(lumis, self.prefs.dmx.adapter_ip(), &self.prefs.dmx.node_name);
        } else {
            self.live_out.dmx = None;
        }
        // Preferences › DMX status + monitor.
        if self.prefs_win.open && self.prefs_win.page == crate::app::prefs::Page::Dmx {
            use lavmfx_media::live::FrameSink;
            let pg = &mut self.prefs_win.dmx;
            pg.out_status = match &self.live_out.dmx {
                Some(d) => d.status(),
                None => "No lumiverses yet".into(),
            };
            let st = &self.dmx_in;
            pg.status = match (&st.rx, &st.error) {
                (_, Some(e)) => format!("Error: {e}"),
                (Some(r), None) => format!("Listening on port {}", r.port),
                (None, None) => String::new(),
            };
            pg.heard = st
                .rx
                .as_ref()
                .map(|r| {
                    r.active()
                        .into_iter()
                        .filter_map(|(u, at)| Some((u, at.elapsed().as_secs_f32(), Box::new(r.universe(u)?.data))))
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    fn library(ed: &mut Editor) -> &[Personality] {
        ed.library
            .get_or_insert_with(|| lavmfx_project::fixtures::load_library(&library_dirs()))
    }

    /// "+" menu rows: add a lumiverse / a fixture of a library personality.
    pub(super) fn dmx_add_menu(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let comp = self.outputs.comp_size();
        if ui
            .button("DMX Lumiverse")
            .on_hover_text("A new lumiverse: one DMX universe (or more) sent to an Art-Net / sACN node")
            .clicked()
        {
            let li = self.outputs.setup.add_lumiverse();
            ed.dmx = Some((li, None));
            ed.tab = Tab::Input;
            self.outputs.touch();
            ui.close();
        }
        ui.menu_button("DMX Fixture", |ui| {
            let lib = Self::library(ed).to_vec();
            for p in lib {
                let label = format!("{}  ({} ch)", p.name, p.channels());
                if ui.button(label).clicked() {
                    let setup = &mut self.outputs.setup;
                    let li = match ed.dmx {
                        Some((li, _)) if li < setup.lumiverses.len() => li,
                        _ if !setup.lumiverses.is_empty() => 0,
                        _ => setup.add_lumiverse(),
                    };
                    let fi = setup.add_fixture(li, comp, p);
                    setup.lumiverses[li].folded = false;
                    ed.dmx = Some((li, Some(fi)));
                    ed.tab = Tab::Input;
                    self.outputs.touch();
                    ui.close();
                }
            }
            ui.separator();
            if ui
                .button("Reload Library")
                .on_hover_text(format!("Read the fixture files again ({})", user_library().display()))
                .clicked()
            {
                ed.library = None;
            }
        })
        .response
        .on_hover_text("Add a fixture to the selected lumiverse");
    }

    /// Lumiverses + fixtures below the screens in the tree.
    pub(super) fn dmx_tree(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let pal = theme::pal(ui);
        let mut action: Option<(usize, Option<usize>, &str)> = None;
        for (li, l) in self.outputs.setup.lumiverses.iter_mut().enumerate() {
            let overlaps = l.overlaps();
            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut l.enabled, "")
                    .on_hover_text("Lumiverse on / off (off sends black)")
                    .changed()
                {
                    action = Some((li, None, "touch"));
                }
                let arrow = if l.folded { Icon::Forward } else { Icon::Down };
                if icon_button(ui, arrow, false, vec2(18.0, 18.0), true)
                    .on_hover_text("Fold: hide its fixtures here and on Input Selection")
                    .clicked()
                {
                    l.folded = !l.folded;
                    action = Some((li, None, "touch"));
                }
                let sel = ed.dmx == Some((li, None));
                let r = ui.selectable_label(sel, RichText::new(&l.name).strong());
                if r.clicked() {
                    ed.dmx = Some((li, None));
                }
                r.context_menu(|ui| {
                    if ui.button("Duplicate Lumiverse").clicked() {
                        action = Some((li, None, "dup"));
                        ui.close();
                    }
                    if ui.button("Delete Lumiverse").clicked() {
                        action = Some((li, None, "delete"));
                        ui.close();
                    }
                });
            });
            let addr = match l.protocol {
                Protocol::ArtNet => {
                    let (n, s, u) = lavmfx_core::dmx::artnet_split(l.universe);
                    format!("Art-Net {} · {n}.{s}.{u}", l.target_ip)
                }
                Protocol::Sacn => {
                    let ip = if l.target_ip.trim().is_empty() {
                        "multicast"
                    } else {
                        l.target_ip.as_str()
                    };
                    format!("sACN {ip} · universe {}", l.universe)
                }
            };
            ui.label(RichText::new(format!("    {addr}")).small().weak());
            if l.folded {
                continue;
            }
            for (fi, f) in l.fixtures.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    if ui
                        .checkbox(&mut f.enabled, "")
                        .on_hover_text("Fixture on / off")
                        .changed()
                    {
                        action = Some((li, Some(fi), "touch"));
                    }
                    let sel = ed.dmx == Some((li, Some(fi)));
                    let bad = overlaps.contains(&fi);
                    let text = match (sel, bad) {
                        (true, _) => RichText::new(&f.name).color(pal.on_accent),
                        (false, true) => RichText::new(&f.name).color(pal.danger),
                        _ => RichText::new(&f.name),
                    };
                    let r = ui
                        .add(egui::Button::selectable(sel, text).fill(if sel {
                            pal.accent
                        } else {
                            Color32::TRANSPARENT
                        }))
                        .on_hover_text(format!(
                            "Channels {}–{}{}",
                            f.start_channel,
                            f.end_channel(),
                            if bad { " (overlaps another fixture)" } else { "" }
                        ));
                    if r.clicked() {
                        ed.dmx = Some((li, Some(fi)));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Duplicate").clicked() {
                            action = Some((li, Some(fi), "dup"));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            action = Some((li, Some(fi), "delete"));
                            ui.close();
                        }
                    });
                });
            }
            ui.add_space(4.0);
        }
        let Some((li, fi, a)) = action else { return };
        let setup = &mut self.outputs.setup;
        match (fi, a) {
            (_, "touch") => {}
            (None, "dup") => {
                let mut copy = setup.lumiverses[li].clone();
                copy.id = setup.new_id();
                copy.name = format!("{} copy", copy.name);
                for f in &mut copy.fixtures {
                    f.id = setup.new_id();
                }
                setup.lumiverses.insert(li + 1, copy);
                ed.dmx = Some((li + 1, None));
            }
            (None, "delete") => {
                setup.lumiverses.remove(li);
                ed.dmx = None;
            }
            (Some(fi), "dup") => {
                let id = setup.new_id();
                let l = &mut setup.lumiverses[li];
                let mut copy = l.fixtures[fi].clone();
                copy.id = id;
                copy.name = format!("{} copy", copy.name);
                copy.start_channel = l.next_channel();
                let dy = quad::size(&copy.input)[1] + 2.0;
                quad::translate(&mut copy.input, [0.0, dy]);
                l.fixtures.insert(fi + 1, copy);
                ed.dmx = Some((li, Some(fi + 1)));
            }
            (Some(fi), "delete") => {
                setup.lumiverses[li].fixtures.remove(fi);
                ed.dmx = Some((li, None));
            }
            _ => {}
        }
        self.outputs.touch();
    }

    // ------------------------------------------------------------ canvas

    pub(super) fn dmx_canvas(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        match ed.tab {
            Tab::Input => self.dmx_input_canvas(ui, ed),
            Tab::Output => self.dmx_channel_map(ui, ed),
        }
    }

    fn dmx_input_canvas(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let pal = theme::pal(ui);
        let comp = self.outputs.comp_size();
        let Some((sel_l, sel_f)) = ed.dmx else { return };
        let (area, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let p = ui.painter_at(area);
        p.rect_filled(area, 0.0, pal.ground);
        let view = canvas_view(ui, ed, area, &resp, comp);
        let k = view.width() / comp.0.max(1) as f32;
        let to = |q: Pt| pos2(view.min.x + q[0] * k, view.min.y + q[1] * k);
        let from = |s: Pos2| -> Pt { [(s.x - view.min.x) / k, (s.y - view.min.y) / k] };
        p.rect_filled(view, 0.0, Color32::BLACK);
        p.image(
            self.comp_tex,
            view,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        p.rect_stroke(view, 0.0, Stroke::new(1.0, pal.accent_dim), egui::StrokeKind::Outside);
        let (cw, ch) = (comp.0 as f32, comp.1 as f32);
        if ed.grid > 0 {
            let st = Stroke::new(1.0, Color32::from_white_alpha(40));
            for i in 1..ed.grid {
                let t = i as f32 / ed.grid as f32;
                p.line_segment([to([cw * t, 0.0]), to([cw * t, ch])], st);
                p.line_segment([to([0.0, ch * t]), to([cw, ch * t])], st);
            }
        }

        let mut touched = false;
        let lumis = &mut self.outputs.setup.lumiverses;
        // Fixtures: other lumiverses dim, folded ones hidden.
        for (li, l) in lumis.iter().enumerate() {
            if l.folded && li != sel_l {
                continue;
            }
            let overlaps = l.overlaps();
            for (fi, f) in l.fixtures.iter().enumerate() {
                let sel = li == sel_l && sel_f == Some(fi);
                let col = if sel {
                    pal.accent
                } else if !f.enabled || !l.enabled {
                    pal.text_weak
                } else if overlaps.contains(&fi) {
                    pal.danger
                } else if li == sel_l {
                    Color32::from_white_alpha(170)
                } else {
                    Color32::from_white_alpha(70)
                };
                p.add(egui::Shape::closed_line(
                    f.input.iter().map(|&q| to(q)).collect(),
                    Stroke::new(if sel { 2.0 } else { 1.0 }, col),
                ));
                // Sample points (one per pixel), when they're far enough apart.
                for b in &f.personality.blocks {
                    let Block::Pixels(px) = b else { continue };
                    let sz = quad::size(&f.input);
                    let cell = (sz[0] / px.width.max(1) as f32).min(sz[1] / px.height.max(1) as f32) * k;
                    if cell < 4.0 || px.pixels() > 4096 {
                        continue;
                    }
                    let r = (cell * 0.18).clamp(1.0, 4.0);
                    for c in f.pixel_centers(px) {
                        p.circle_stroke(to(c), r, Stroke::new(1.0, col));
                    }
                }
                if sel || li == sel_l {
                    let (lo, _) = lavmfx_core::output::bounds(&f.input);
                    p.text(
                        to(lo) - vec2(0.0, 2.0),
                        egui::Align2::LEFT_BOTTOM,
                        &f.name,
                        egui::FontId::proportional(12.0),
                        col,
                    );
                }
            }
        }
        // Handles of the selected fixture.
        let selected = sel_f.and_then(|fi| lumis.get(sel_l)?.fixtures.get(fi).map(|f| (fi, f.input)));
        if let Some((_, q)) = selected {
            for c in q {
                p.rect_filled(
                    Rect::from_center_size(to(c), Vec2::splat(HANDLE_SMALL)),
                    2.0,
                    pal.accent,
                );
            }
        }

        // Pointer.
        let ptr = resp.interact_pointer_pos().or(resp.hover_pos());
        let middle = ui.input(|i| i.pointer.middle_down());
        if resp.drag_started() {
            let at = ptr.map(from).unwrap_or_default();
            ed.dmx_grab = if middle || ed.pan_tool {
                Some(DmxGrab::Pan)
            } else if let Some((_, q)) = selected
                && let Some(c) = (0..4).find(|&c| ptr.is_some_and(|m| m.distance(to(q[c])) <= GRAB_R))
            {
                Some(DmxGrab::Corner(c, q))
            } else {
                // Topmost fixture under the pointer: the selected lumiverse first.
                let hit = std::iter::once(sel_l)
                    .chain((0..lumis.len()).filter(|&l| l != sel_l))
                    .filter_map(|li| {
                        let l = lumis.get(li)?;
                        if l.folded && li != sel_l {
                            return None;
                        }
                        let fi = (0..l.fixtures.len())
                            .rev()
                            .find(|&fi| quad::contains(&l.fixtures[fi].input, at))?;
                        Some((li, fi))
                    })
                    .next();
                hit.map(|(li, fi)| {
                    ed.dmx = Some((li, Some(fi)));
                    DmxGrab::Move(lumis[li].fixtures[fi].input, at)
                })
            };
        }
        if resp.dragged()
            && let Some(g) = ed.dmx_grab
        {
            let fx = ed.dmx.and_then(|(li, fi)| lumis.get_mut(li)?.fixtures.get_mut(fi?));
            match (g, fx, ptr) {
                (DmxGrab::Pan, ..) => ed.pan += resp.drag_delta(),
                (DmxGrab::Move(q, start), Some(f), Some(m)) => {
                    let d = sub(from(m), start);
                    f.input = q;
                    quad::translate(&mut f.input, d);
                    touched = true;
                }
                (DmxGrab::Corner(c, q), Some(f), Some(m)) => {
                    f.input = resize_corner(&q, c, from(m));
                    touched = true;
                }
                _ => {}
            }
        }
        if resp.drag_stopped() {
            ed.dmx_grab = None;
        }
        if resp.clicked()
            && let Some(m) = ptr
        {
            let at = from(m);
            let l = &lumis[sel_l.min(lumis.len().saturating_sub(1))];
            let fi = (0..l.fixtures.len())
                .rev()
                .find(|&fi| quad::contains(&l.fixtures[fi].input, at));
            ed.dmx = Some((sel_l, fi));
        }
        // Keys (canvas hovered): Delete, arrows.
        if resp.hovered() || resp.has_focus() {
            let (del, dx, dy, big) = ui.input(|i| {
                let k = |key| i.key_pressed(key);
                (
                    k(Key::Delete) || k(Key::Backspace),
                    k(Key::ArrowRight) as i32 - k(Key::ArrowLeft) as i32,
                    k(Key::ArrowDown) as i32 - k(Key::ArrowUp) as i32,
                    i.modifiers.shift,
                )
            });
            if let Some((li, Some(fi))) = ed.dmx
                && li < lumis.len()
                && fi < lumis[li].fixtures.len()
            {
                if del {
                    lumis[li].fixtures.remove(fi);
                    ed.dmx = Some((li, None));
                    touched = true;
                } else if dx != 0 || dy != 0 {
                    let s = if big { 10.0 } else { 1.0 };
                    quad::translate(&mut lumis[li].fixtures[fi].input, [dx as f32 * s, dy as f32 * s]);
                    touched = true;
                }
            }
        }
        if touched {
            self.outputs.touch();
        }
    }

    /// DMX Output tab: channel grid of the selected lumiverse.
    fn dmx_channel_map(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let pal = theme::pal(ui);
        let Some((li, sel_f)) = ed.dmx else { return };
        let Some(l) = self.outputs.setup.lumiverses.get(li) else {
            return;
        };
        let state = self.live_out.dmx.as_ref().and_then(|d| d.state(l.id));
        let values = state.map(|s| s.channels).unwrap_or_default();
        const COLS: usize = 32;
        const ROWS: usize = UNIVERSE / COLS;
        let universes = l.universes();
        let header = 22.0;
        let cell = ((ui.available_width() - 8.0) / COLS as f32).clamp(10.0, 40.0);
        let block_h = header + ROWS as f32 * cell + 10.0;
        // Owner of each channel (last fixture wins; overlaps marked).
        let total = universes * UNIVERSE;
        let mut owner: Vec<Option<usize>> = vec![None; total];
        let mut clash = vec![false; total];
        for (fi, f) in l.fixtures.iter().enumerate().filter(|(_, f)| f.enabled) {
            for c in f.start_channel.max(1) as usize - 1..(f.end_channel() as usize).min(total) {
                clash[c] |= owner[c].is_some();
                owner[c] = Some(fi);
            }
        }
        let disabled: Vec<(usize, u32, u32)> = l
            .fixtures
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.enabled)
            .map(|(i, f)| (i, f.start_channel, f.end_channel()))
            .collect();
        let first = l.universe;
        let protocol = l.protocol;
        let names: Vec<String> = l.fixtures.iter().map(|f| f.name.clone()).collect();
        let mut grab_to: Option<i64> = None;
        let mut picked: Option<Option<usize>> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let (area, resp) = ui.allocate_exact_size(
                vec2(cell * COLS as f32 + 8.0, block_h * universes as f32),
                egui::Sense::click_and_drag(),
            );
            let p = ui.painter_at(area);
            let origin = area.min + vec2(4.0, 0.0);
            let cell_rect = |c: usize| {
                let (u, w) = (c / UNIVERSE, c % UNIVERSE);
                let y = origin.y + u as f32 * block_h + header + (w / COLS) as f32 * cell;
                Rect::from_min_size(pos2(origin.x + (w % COLS) as f32 * cell, y), Vec2::splat(cell))
            };
            let at = |m: Pos2| -> Option<usize> {
                let rel = m - origin;
                let u = (rel.y / block_h).floor();
                let y = rel.y - u * block_h - header;
                if u < 0.0 || y < 0.0 || rel.x < 0.0 {
                    return None;
                }
                let (row, col) = ((y / cell) as usize, (rel.x / cell) as usize);
                (row < ROWS && col < COLS)
                    .then(|| u as usize * UNIVERSE + row * COLS + col)
                    .filter(|&c| c < total)
            };
            for u in 0..universes {
                let label = match protocol {
                    Protocol::ArtNet => {
                        let (n, s, x) = lavmfx_core::dmx::artnet_split(first.wrapping_add(u as u16));
                        format!("Universe {n}.{s}.{x}")
                    }
                    Protocol::Sacn => format!("Universe {}", first.wrapping_add(u as u16)),
                };
                p.text(
                    pos2(origin.x, origin.y + u as f32 * block_h + header / 2.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(13.0),
                    pal.text_strong,
                );
            }
            let font = egui::FontId::monospace((cell * 0.36).clamp(7.0, 11.0));
            for c in 0..total {
                let r = cell_rect(c).shrink(1.0);
                let fill = match owner[c] {
                    _ if clash[c] => pal.danger,
                    Some(fi) if sel_f == Some(fi) => pal.accent,
                    Some(fi) => fixture_color(fi).gamma_multiply(0.7),
                    None if disabled
                        .iter()
                        .any(|&(_, s, e)| (s as usize..=e as usize).contains(&(c + 1))) =>
                    {
                        pal.button
                    }
                    None => pal.field,
                };
                p.rect_filled(r, 2.0, fill);
                let v = values.get(c).copied().unwrap_or(0);
                if v > 0 {
                    // Value as a bar along the bottom.
                    let h = (r.height() * 0.22).max(2.0);
                    let bar = Rect::from_min_size(pos2(r.min.x, r.max.y - h), vec2(r.width() * v as f32 / 255.0, h));
                    p.rect_filled(bar, 0.0, Color32::from_white_alpha(200));
                }
                if cell >= 24.0 {
                    p.text(
                        r.center() - vec2(0.0, r.height() * 0.1),
                        egui::Align2::CENTER_CENTER,
                        (c % UNIVERSE + 1).to_string(),
                        font.clone(),
                        Color32::from_white_alpha(140),
                    );
                }
            }
            if let Some(m) = resp.hover_pos()
                && let Some(c) = at(m)
            {
                let who = match owner[c] {
                    Some(fi) => names[fi].clone(),
                    None => "free".into(),
                };
                let v = values.get(c).copied().unwrap_or(0);
                resp.clone().on_hover_text_at_pointer(format!(
                    "Channel {} (universe +{}, channel {}): {who}{}\nValue {v}",
                    c + 1,
                    c / UNIVERSE,
                    c % UNIVERSE + 1,
                    if clash[c] { " — overlap!" } else { "" }
                ));
            }
            if resp.drag_started()
                && let Some(c) = resp.interact_pointer_pos().and_then(at)
            {
                picked = Some(owner[c]);
                ed.dmx_grab = owner[c].map(|_| DmxGrab::Channel(0, c as i64));
            }
            if resp.clicked()
                && let Some(c) = resp.interact_pointer_pos().and_then(at)
            {
                picked = Some(owner[c]);
            }
            if resp.dragged()
                && let Some(DmxGrab::Channel(..)) = ed.dmx_grab
                && let Some(m) = resp.interact_pointer_pos()
            {
                // Pointer channel, even past the grid edges (clamped into it).
                let rel = m - origin;
                let u = (rel.y / block_h).floor().max(0.0);
                let y = (rel.y - u * block_h - header).clamp(0.0, ROWS as f32 * cell - 1.0);
                let col = (rel.x / cell).clamp(0.0, COLS as f32 - 1.0) as i64;
                grab_to = Some(u as i64 * UNIVERSE as i64 + (y / cell) as i64 * COLS as i64 + col);
            }
            if resp.drag_stopped() {
                ed.dmx_grab = None;
            }
        });
        let l = &mut self.outputs.setup.lumiverses[li];
        if let Some(fi) = picked {
            ed.dmx = Some((li, fi));
            if let (Some(fi), Some(DmxGrab::Channel(_, c))) = (fi, ed.dmx_grab) {
                ed.dmx_grab = Some(DmxGrab::Channel(l.fixtures[fi].start_channel, c));
            }
        }
        if let (Some(to), Some(DmxGrab::Channel(start, from)), Some((_, Some(fi)))) = (grab_to, ed.dmx_grab, ed.dmx)
            && let Some(f) = l.fixtures.get_mut(fi)
        {
            let new = (start as i64 + to - from).clamp(1, 63 * UNIVERSE as i64) as u32;
            if new != f.start_channel {
                f.start_channel = new;
                self.outputs.touch();
            }
        }
    }

    // ------------------------------------------------------------ properties

    pub(super) fn dmx_props(&mut self, ui: &mut egui::Ui, ed: &mut Editor) {
        let comp = self.outputs.comp_size();
        let Some((li, fi)) = ed.dmx else { return };
        let state = self
            .outputs
            .setup
            .lumiverses
            .get(li)
            .and_then(|l| self.live_out.dmx.as_ref()?.state(l.id));
        let library = Self::library(ed).to_vec();
        let mut save: Option<Personality> = None;
        let Some(l) = self.outputs.setup.lumiverses.get_mut(li) else {
            return;
        };
        let changed = match fi {
            None => lumiverse_props(ui, l, state.as_ref()),
            Some(fi) => {
                let channels = state.as_ref().map(|s| s.channels.as_slice()).unwrap_or(&[]);
                fixture_props(ui, &mut l.fixtures[fi], comp, &library, channels, &mut save)
            }
        };
        if let Some(p) = save {
            match lavmfx_project::fixtures::save(&user_library(), &p) {
                Ok(path) => self.status = format!("Saved fixture \"{}\" to {}", p.name, path.display()),
                Err(e) => self.status = format!("Couldn't save the fixture: {e}"),
            }
            ed.library = None;
        }
        if changed {
            self.outputs.touch();
        }
    }
}

fn lumiverse_props(ui: &mut egui::Ui, l: &mut Lumiverse, state: Option<&crate::dmx::LumiState>) -> bool {
    let pal = theme::pal(ui);
    let mut ch = false;
    ui.label(RichText::new("Lumiverse").strong());
    ch |= name_row(ui, &mut l.name);
    ch |= ui.checkbox(&mut l.enabled, "Enabled (off sends black)").changed();
    section(ui, "DMX Output", |ui| {
        ui.horizontal(|ui| {
            ui.label("Protocol");
            egui::ComboBox::from_id_salt("dmx-proto")
                .selected_text(l.protocol.name())
                .show_ui(ui, |ui| {
                    for pr in [Protocol::ArtNet, Protocol::Sacn] {
                        if ui.selectable_label(l.protocol == pr, pr.name()).clicked() && l.protocol != pr {
                            l.protocol = pr;
                            if pr == Protocol::Sacn {
                                l.universe = l.universe.max(1);
                            }
                            ch = true;
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Target IP");
            ch |= ui
                .add(egui::TextEdit::singleline(&mut l.target_ip).desired_width(140.0))
                .on_hover_text("The Art-Net / sACN node's address")
                .changed();
        });
        let ok = l.target_ip.trim().parse::<std::net::Ipv4Addr>().is_ok()
            || (l.protocol == Protocol::Sacn && l.target_ip.trim().is_empty());
        if !ok {
            ui.label(RichText::new("Not an IPv4 address").color(pal.danger).small());
        }
        let hint = match l.protocol {
            Protocol::ArtNet => {
                "Art-Net: the node's IP, or the network's broadcast address (e.g. 192.168.1.255 / 2.255.255.255) for every node."
            }
            Protocol::Sacn => "sACN: the node's IP, or empty for the universe's multicast group (239.255.x.y).",
        };
        ui.add(egui::Label::new(RichText::new(hint).small().weak()).wrap());
        match l.protocol {
            Protocol::ArtNet => {
                let (n, s, u) = lavmfx_core::dmx::artnet_split(l.universe);
                let (mut n, mut s, mut u) = (n as f32, s as f32, u as f32);
                let a = value_row(ui, "Net", &mut n, 0.0..=127.0, 1.0, 0.0, false);
                let b = value_row(ui, "Subnet", &mut s, 0.0..=15.0, 1.0, 0.0, false);
                let c = value_row(ui, "Universe", &mut u, 0.0..=15.0, 1.0, 0.0, false);
                if a || b || c {
                    l.universe = lavmfx_core::dmx::artnet_join(n as u8, s as u8, u as u8);
                    ch = true;
                }
            }
            Protocol::Sacn => {
                let mut u = l.universe as f32;
                if value_row(ui, "Universe", &mut u, 1.0..=63999.0, 1.0, 1.0, false) {
                    l.universe = u as u16;
                    ch = true;
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label("Framerate");
            egui::ComboBox::from_id_salt("dmx-fps")
                .selected_text(format!("{} fps", l.framerate))
                .show_ui(ui, |ui| {
                    for f in [10.0, 15.0, 20.0, 25.0, 30.0, 40.0, 44.0, 50.0, 60.0] {
                        if ui.selectable_label(l.framerate == f, format!("{f} fps")).clicked() {
                            l.framerate = f;
                            ch = true;
                        }
                    }
                })
                .response
                .on_hover_text("Packets per second (DMX hardware tops out at 44)");
        });
    });
    section(ui, "Color", |ui| ch |= color_rows(ui, &mut l.color, true));
    let used = l.used_channels();
    ui.label(format!(
        "{} fixtures, {used} channels, {} universe(s)",
        l.fixtures.len(),
        l.universes()
    ));
    let overlaps = l.overlaps();
    if !overlaps.is_empty() {
        let names: Vec<&str> = overlaps.iter().map(|&i| l.fixtures[i].name.as_str()).collect();
        ui.label(RichText::new(format!("Overlapping channels: {}", names.join(", "))).color(pal.danger));
    }
    match state {
        Some(s) => match &s.error {
            Some(e) => {
                ui.label(RichText::new(e).color(pal.danger));
            }
            None => {
                ui.label(RichText::new(format!("Sent {} frames", s.sent)).color(pal.accent));
            }
        },
        None => {
            ui.label(RichText::new("Not sending yet").weak());
        }
    }
    ch
}

fn fixture_props(
    ui: &mut egui::Ui,
    f: &mut Fixture,
    comp: (u32, u32),
    library: &[Personality],
    channels: &[u8],
    save: &mut Option<Personality>,
) -> bool {
    let mut ch = false;
    ui.label(RichText::new("Fixture").strong());
    ch |= name_row(ui, &mut f.name);
    ch |= ui.checkbox(&mut f.enabled, "Enabled").changed();
    ui.horizontal(|ui| {
        ui.label("Personality");
        egui::ComboBox::from_id_salt("dmx-pers")
            .selected_text(&f.personality.name)
            .width(170.0)
            .truncate()
            .show_ui(ui, |ui| {
                for p in library {
                    if ui
                        .selectable_label(f.personality == *p, format!("{}  ({} ch)", p.name, p.channels()))
                        .clicked()
                    {
                        f.personality = p.clone();
                        ch = true;
                    }
                }
            })
            .response
            .on_hover_text("Select the personality (fixture type) from the library");
    });
    let mut start = f.start_channel as f32;
    if value_row(
        ui,
        "Start Channel",
        &mut start,
        1.0..=(63 * UNIVERSE) as f32,
        1.0,
        1.0,
        false,
    ) {
        f.start_channel = start as u32;
        ch = true;
    }
    ui.label(
        RichText::new(format!(
            "Channels {}–{} ({} ch)",
            f.start_channel,
            f.end_channel(),
            f.personality.channels()
        ))
        .small()
        .weak(),
    );
    section(ui, "Input", |ui| {
        ch |= quad_rows(ui, &mut f.input, (comp.0 as f32, comp.1 as f32));
        ui.horizontal(|ui| {
            ui.label("Flip");
            ch |= ui.checkbox(&mut f.flip.h, "Horizontal").changed();
            ch |= ui.checkbox(&mut f.flip.v, "Vertical").changed();
        });
    });
    section(ui, "Color", |ui| ch |= color_rows(ui, &mut f.color, false));
    section(ui, "Fixture Output Preview", |ui| {
        let mut c0 = f.start_channel.max(1) as usize - 1;
        for b in &f.personality.blocks {
            match b {
                Block::Param { .. } => c0 += 1,
                Block::Pixels(px) => {
                    let cols = unpack(px, channels, c0);
                    let (w, h) = (px.width.clamp(1, 512) as usize, px.height.clamp(1, 512) as usize);
                    let max_w = ui.available_width().min(300.0);
                    let cell = (max_w / w as f32).min(120.0 / h as f32).clamp(1.0, 24.0);
                    let (r, _) = ui.allocate_exact_size(vec2(cell * w as f32, cell * h as f32), egui::Sense::hover());
                    let p = ui.painter_at(r);
                    for y in 0..h {
                        for x in 0..w {
                            let cr = Rect::from_min_size(r.min + vec2(x as f32, y as f32) * cell, Vec2::splat(cell));
                            p.rect_filled(if cell > 4.0 { cr.shrink(0.5) } else { cr }, 0.0, cols[y * w + x]);
                        }
                    }
                    c0 += px.channels();
                }
            }
        }
        if channels.is_empty() {
            ui.label(RichText::new("Nothing sent yet").small().weak());
        }
    });
    section(ui, "Personality (Fixture Editor)", |ui| {
        ch |= personality_editor(ui, &mut f.personality);
        ui.horizontal(|ui| {
            if ui
                .button("Save to Library")
                .on_hover_text(format!(
                    "Write \"{}\" to {}",
                    f.personality.name,
                    user_library().display()
                ))
                .clicked()
            {
                *save = Some(f.personality.clone());
            }
        });
    });
    ch
}

/// Fixture Editor, in the properties: name + ordered blocks.
fn personality_editor(ui: &mut egui::Ui, p: &mut Personality) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.label("Name");
        ch |= ui.text_edit_singleline(&mut p.name).changed();
    });
    let mut op: Option<(usize, i32)> = None; // (block, -1 up / 1 down / 0 delete)
    let n = p.blocks.len();
    for (bi, b) in p.blocks.iter_mut().enumerate() {
        ui.push_id(("dmx-block", bi), |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(match b {
                        Block::Pixels(_) => "Pixel Block",
                        Block::Param { .. } => "DMX Parameter",
                    })
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let sz = vec2(18.0, 18.0);
                    if icon_button(ui, Icon::Close, false, sz, n > 1)
                        .on_hover_text("Delete")
                        .clicked()
                        && n > 1
                    {
                        op = Some((bi, 0));
                    }
                    if icon_button(ui, Icon::Down, false, sz, bi + 1 < n)
                        .on_hover_text("Later channels")
                        .clicked()
                        && bi + 1 < n
                    {
                        op = Some((bi, 1));
                    }
                    if icon_button(ui, Icon::Up, false, sz, bi > 0)
                        .on_hover_text("Earlier channels")
                        .clicked()
                        && bi > 0
                    {
                        op = Some((bi, -1));
                    }
                });
            });
            match b {
                Block::Param { name, value } => {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ch |= ui.text_edit_singleline(name).changed();
                    });
                    let mut v = *value as f32;
                    if value_row(ui, "Default Value", &mut v, 0.0..=255.0, 1.0, 0.0, true) {
                        *value = v as u8;
                        ch = true;
                    }
                }
                Block::Pixels(px) => ch |= pixel_block_rows(ui, px),
            }
        });
    }
    if let Some((bi, d)) = op {
        match d {
            0 => {
                p.blocks.remove(bi);
            }
            -1 => p.blocks.swap(bi, bi - 1),
            _ => p.blocks.swap(bi, bi + 1),
        }
        ch = true;
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("+ Pixel Block").clicked() {
            p.blocks.push(Block::Pixels(PixelBlock::default()));
            ch = true;
        }
        if ui
            .button("+ DMX Parameter")
            .on_hover_text("One channel with a fixed value (dimmer, mode, strobe…)")
            .clicked()
        {
            p.blocks.push(Block::Param {
                name: "Parameter".into(),
                value: 0,
            });
            ch = true;
        }
    });
    ch
}

fn pixel_block_rows(ui: &mut egui::Ui, px: &mut PixelBlock) -> bool {
    let mut ch = false;
    let (mut w, mut h) = (px.width as f32, px.height as f32);
    if value_row(ui, "Width", &mut w, 1.0..=512.0, 1.0, 1.0, false) {
        px.width = w as u32;
        ch = true;
    }
    if value_row(ui, "Height", &mut h, 1.0..=512.0, 1.0, 1.0, false) {
        px.height = h as u32;
        ch = true;
    }
    ui.horizontal(|ui| {
        ui.label("Color Space");
        egui::ComboBox::from_id_salt("cs")
            .selected_text(px.color.name())
            .show_ui(ui, |ui| {
                for c in ColorSpace::ALL {
                    if ui.selectable_label(px.color == c, c.name()).clicked() {
                        px.color = c;
                        ch = true;
                    }
                }
            })
            .response
            .on_hover_text("Which channels each pixel has, in order");
    });
    ch |= value_row(ui, "Gamma", &mut px.gamma, 1.0..=3.0, 0.1, 2.5, true);
    ui.horizontal(|ui| {
        ui.label("Distribution");
        egui::ComboBox::from_id_salt("start")
            .selected_text(px.distribution.start.name())
            .show_ui(ui, |ui| {
                for c in Corner::ALL {
                    if ui.selectable_label(px.distribution.start == c, c.name()).clicked() {
                        px.distribution.start = c;
                        ch = true;
                    }
                }
            })
            .response
            .on_hover_text("Corner of the first channel");
    });
    ui.horizontal(|ui| {
        ch |= ui
            .checkbox(&mut px.distribution.columns, "Columns first")
            .on_hover_text("Run down each column instead of along each row")
            .changed();
        ch |= ui
            .checkbox(&mut px.distribution.snake, "Snake")
            .on_hover_text("Every other row / column runs back (zig-zag wiring)")
            .changed();
    });
    distribution_diagram(ui, px);
    ui.label(
        RichText::new(format!("{} pixels, {} channels", px.pixels(), px.channels()))
            .small()
            .weak(),
    );
    ch
}

/// Pixel Distribution picture: the channel path through the grid
/// (open circle = first channel).
fn distribution_diagram(ui: &mut egui::Ui, px: &PixelBlock) {
    let pal = theme::pal(ui);
    let (w, h) = (px.width.clamp(1, 512) as usize, px.height.clamp(1, 512) as usize);
    if w * h > 1024 {
        return;
    }
    let max_w = ui.available_width().min(240.0);
    let cell = (max_w / w as f32).min(140.0 / h as f32).clamp(3.0, 20.0);
    let (r, _) = ui.allocate_exact_size(vec2(cell * w as f32, cell * h as f32), egui::Sense::hover());
    let p = ui.painter_at(r);
    p.rect_filled(r, 2.0, pal.field);
    let c = |(x, y): (usize, usize)| r.min + vec2((x as f32 + 0.5) * cell, (y as f32 + 0.5) * cell);
    let order = px.distribution.order(w, h);
    let pts: Vec<Pos2> = order.iter().map(|&q| c(q)).collect();
    p.add(egui::Shape::line(pts.clone(), Stroke::new(1.5, pal.accent)));
    if let Some(first) = pts.first() {
        p.circle_stroke(*first, (cell * 0.3).max(2.5), Stroke::new(1.5, pal.text_strong));
    }
    if let (Some(a), Some(b)) = (pts.len().checked_sub(2).map(|i| pts[i]), pts.last()) {
        p.arrow(a, *b - a, Stroke::new(1.5, pal.accent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_resize_keeps_the_opposite_corner_and_rotation() {
        let q = lavmfx_core::output::rect(10.0, 10.0, 100.0, 20.0);
        let r = resize_corner(&q, 2, [150.0, 50.0]);
        assert_eq!(r, lavmfx_core::output::rect(10.0, 10.0, 140.0, 40.0));
        // Dragging past the opposite corner stops at 1 px.
        let r = resize_corner(&q, 2, [0.0, 0.0]);
        assert_eq!(r[0], [10.0, 10.0]);
        assert!((r[2][0] - 11.0).abs() < 1e-4 && (r[2][1] - 11.0).abs() < 1e-4);
        let mut rot = q;
        quad::rotate(&mut rot, 30.0);
        let r = resize_corner(&rot, 0, rot[0]);
        for (a, b) in r.iter().zip(rot.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3);
        }
    }

    #[test]
    fn preview_unpacks_sent_channels() {
        let px = PixelBlock {
            width: 2,
            height: 1,
            color: ColorSpace::Grb,
            ..Default::default()
        };
        let cols = unpack(&px, &[0, 9, 0, 0, 200, 0, 100], 1);
        assert_eq!(cols, [Color32::from_rgb(0, 9, 0), Color32::from_rgb(0, 200, 100)]);
    }
}
