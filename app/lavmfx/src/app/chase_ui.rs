//! Slice Chase: the running chases of the composition's Slice Chase effects
//! (values per slice for the renderer, each frame) and the Slice Chase
//! window, where sequences are made, from a rule or by hand on a map of
//! the slices.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Color32, Pos2, RichText, Stroke, pos2, vec2};
use lavmfx_core::Id;
use lavmfx_core::chase::{self, Chaser, Order, Params, Presses, Rule, Sequence, SliceInfo, SliceRef, Tick};

use super::widgets::{bool_row, label_row, section_header, value_row};
use super::{LavmfxApp, theme};

/// Shader kinds of the Slice Chase effects (lavmfx-core `SHADER_EFFECTS`).
fn is_chase_kind(kind: u32) -> bool {
    chase::EFFECTS
        .iter()
        .any(|n| lavmfx_core::effects::shader_kind(n) == Some(kind))
}

/// How a chase effect draws on the outputs: None = the Sequencer (dims the
/// whole slice), Some(look) = a `chase.wgsl` look (1 Wipe … 4 Sample),
/// Some(DELAY_LOOK) = Delay (older composition frames per slice).
fn output_look(e: &lavmfx_core::VideoEffect) -> Option<u32> {
    let kind = lavmfx_core::effects::shader_kind(&e.name)?;
    match kind.checked_sub(chase::FIRST_KIND)? {
        0 => None,
        k => Some(k),
    }
}

const DELAY_LOOK: u32 = 5;

/// Runtime and window state (nothing here is saved).
#[derive(Default)]
pub struct ChaseState {
    runners: HashMap<Id, Chaser>,
    /// Slices fingerprint the sequences were last relinked to (None: the
    /// composition was just opened, so names decide first).
    link: Option<u64>,
    /// Slice Chase window open.
    pub open: bool,
    selected: usize,
    /// Hand-made step being edited.
    step: usize,
    /// Name being typed (committed on Enter / focus loss).
    name_edit: Option<String>,
    /// Last frame's levels per sequence name (the map lights them).
    live: HashMap<String, Vec<f32>>,
    /// Debug log: slices each effect lit last (logged when it changes).
    traced: HashMap<Id, Vec<usize>>,
}

const HELP: &str = "The Slice Chase effects (Sequencer, Wipe, Runner, Tiles, Sample, Delay) pick a \
sequence in their Sequence param (All Slices = every slice, one per step). The map shows the slices \
where they are on the outputs. Space Output draws on the outputs, slice by slice (use it when \
slices read the same part of the composition, e.g. all full frame); Space Input works in the \
layer, inside each slice's input selection. Auto picks Output when the inputs overlap. A rule \
sequence follows the slices as they are now: new slices join by themselves. A hand-made sequence \
remembers its slices by id and name, so renamed or moved slices stay in it.";

impl LavmfxApp {
    /// A composition was opened: point its hand-made sequences at this
    /// setup's slices (names first: another setup numbers slices differently).
    pub(super) fn chase_loaded(&mut self) {
        let infos = chase::slice_infos(&self.outputs.setup);
        chase::relink(&mut self.comp.chase, &infos, false);
        self.comp.sync_chase(None);
        self.chase.link = Some(chase::link_key(&infos));
        self.chase.runners.clear();
        self.chase.selected = 0;
        self.chase.step = 0;
        self.chase.name_edit = None;
    }

    /// Run every Slice Chase effect drawn this frame: input-space ones get
    /// their per-slice values in `f.chase`; output-space ones leave the
    /// layer alone (their draws are taken out of `f`) and dim whole slices
    /// on the outputs instead (`Outputs::slice_levels`).
    pub(super) fn apply_chase(&mut self, f: &mut lavmfx_render::FxFrame) {
        let fired = lavmfx_core::trigger::take();
        let infos = chase::slice_infos(&self.outputs.setup);
        let key = chase::link_key(&infos);
        if self.chase.link != Some(key) {
            // The setup changed (edited, or another one loaded): ids lead.
            let prefer_id = self.chase.link.is_some();
            chase::relink(&mut self.comp.chase, &infos, prefer_id);
            self.chase.link = Some(key);
        }
        self.comp.sync_chase(None);
        let drawn: HashSet<i64> = f
            .layer
            .iter()
            .chain(&f.outgoing)
            .chain(&f.group)
            .flatten()
            .chain(&f.composition)
            .filter(|e| is_chase_kind(e.kind))
            .map(|e| e.id)
            .collect();
        let mut out = HashMap::new();
        let mut on_outputs: HashSet<i64> = HashSet::new();
        let mut levels: HashMap<u64, f32> = HashMap::new();
        let mut looks: HashMap<u64, Vec<lavmfx_render::slices::SliceLook>> = HashMap::new();
        let mut delays: HashMap<u64, u32> = HashMap::new();
        let mut live: HashMap<String, Vec<f32>> = HashMap::new();
        if !drawn.is_empty() {
            let tick = Tick {
                seconds: self.clock.seconds(),
                beats: self.clock.beats(),
                fft: chase::fft_levels(&self.isf_fft()),
            };
            let n = infos.len();
            let len = f.slices.len().max(n);
            for e in self.comp.chase_effects().filter(|e| drawn.contains(&e.id)) {
                if out.contains_key(&e.id) || on_outputs.contains(&e.id) {
                    continue;
                }
                let p = Params::of(e);
                let steps = chase::effect_steps(&self.comp.chase, e, &infos);
                let runner = self.chase.runners.entry(e.id).or_default();
                let mut v = runner.frame(&p, &steps, n, &tick, Presses::of(e, &fired));
                live.entry(chase::effect_sequence(e).to_owned())
                    .or_insert_with(|| v.iter().map(|x| x[0]).collect());
                if log::log_enabled!(log::Level::Debug) {
                    // `RUST_LOG=lavmfx::app::chase_ui=debug`: what each chase lights, when it changes.
                    let lit: Vec<usize> = (0..n).filter(|&i| v[i][0] > 0.5).collect();
                    if self.chase.traced.get(&e.id) != Some(&lit) {
                        let names: Vec<&str> = lit.iter().map(|&i| infos[i].name.as_str()).collect();
                        log::debug!(
                            "chase {} ({}, {}): beat {:.2} · {:.2} s · lit {:?}",
                            e.id,
                            chase::effect_sequence(e),
                            if p.on_outputs(&infos) { "output" } else { "input" },
                            tick.beats,
                            tick.seconds,
                            names
                        );
                        self.chase.traced.insert(e.id, lit);
                    }
                }
                let look = output_look(e);
                if p.on_outputs(&infos) {
                    match look {
                        // Sequencer: several output chases multiply.
                        None => {
                            for (sl, x) in infos.iter().zip(&v) {
                                *levels.entry(sl.id).or_insert(1.0) *= p.output_level(x, e.opacity());
                            }
                        }
                        // Delay: each step an older frame; dims like the Sequencer.
                        Some(DELAY_LOOK) => {
                            let per_step = match e.params.get("Delay") {
                                Some(lavmfx_core::Param::Range(r)) => r.value.max(0.0),
                                _ => 0.0,
                            };
                            for (sl, x) in infos.iter().zip(&v) {
                                *levels.entry(sl.id).or_insert(1.0) *= p.output_level(x, e.opacity());
                                let d = (f64::from(x[2]) * per_step).round() as u32;
                                if x[3] > 0.5 && d > 0 {
                                    delays.insert(sl.id, d);
                                }
                            }
                        }
                        Some(k) => {
                            let fx = lavmfx_core::effects::shader_fx(e).map_or([0.0; 32], |f| f.params);
                            let mut pp = [[0.0f32; 4]; 3];
                            for (j, c) in fx.chunks(4).take(3).enumerate() {
                                pp[j].copy_from_slice(c);
                            }
                            let outside = match p.outside {
                                chase::Outside::Clear => 0.0,
                                chase::Outside::Black => 1.0,
                                chase::Outside::Keep => 2.0,
                            };
                            for (sl, x) in infos.iter().zip(&v) {
                                if x[3] < 0.5 && p.outside == chase::Outside::Keep {
                                    continue; // left as it is
                                }
                                let lvl = p.unlit + (1.0 - p.unlit) * f64::from(x[0]).clamp(0.0, 1.0);
                                looks.entry(sl.id).or_default().push(lavmfx_render::slices::SliceLook {
                                    a: [k as f32, x[0], x[1], x[2]],
                                    b: [lvl as f32, e.opacity() as f32, outside, x[3]],
                                    p: pp,
                                });
                            }
                        }
                    }
                    on_outputs.insert(e.id);
                } else {
                    v.resize(len, [0.0; 4]);
                    out.insert(e.id, v);
                }
            }
        }
        if !on_outputs.is_empty() {
            for chain in f.layer.iter_mut().chain(&mut f.outgoing).chain(&mut f.group) {
                chain.retain(|e| !on_outputs.contains(&e.id));
            }
            f.composition.retain(|e| !on_outputs.contains(&e.id));
        }
        self.chase
            .runners
            .retain(|id, _| out.contains_key(id) || on_outputs.contains(id));
        self.chase.live = live;
        self.outputs.slice_levels = levels;
        self.outputs.slice_looks = looks;
        self.outputs.slice_delays = delays;
        f.chase = out;
    }

    /// Debug hook (`LAVMFX_SCREENSHOT_CHASE=demo`): a rule and a hand-made
    /// sequence over the current slices, the hand-made one shown.
    pub(super) fn chase_demo(&mut self) {
        let infos = chase::slice_infos(&self.outputs.setup);
        let rule = |order, bands| Rule {
            order,
            bands,
            ..Default::default()
        };
        let mut hand = Sequence {
            name: "Clockwise".into(),
            rule: Some(rule(Order::Clockwise, false)),
            steps: Vec::new(),
        };
        hand.bake(&infos);
        self.comp.chase.sequences = vec![
            Sequence {
                name: "Columns".into(),
                rule: Some(rule(Order::LeftToRight, true)),
                steps: Vec::new(),
            },
            hand,
        ];
        self.comp.sync_chase(None);
        self.chase.selected = 1;
        self.chase.step = 2;
    }

    /// Debug hook (`LAVMFX_SCREENSHOT_CHASE_PARAMS="Space=Output;Trail=2"`): set
    /// params of every Slice Chase effect (choices by option name, ranges by value,
    /// booleans 1 / 0).
    pub(super) fn chase_debug_params(&mut self, spec: &str) {
        use lavmfx_core::Param;
        self.comp.sync_chase(None);
        self.comp.for_each_chase_mut(|e| {
            for item in spec.split(';') {
                let Some((k, v)) = item.split_once('=') else { continue };
                let (k, v) = (k.trim(), v.trim());
                match e.params.get_mut(k) {
                    Some(Param::Choice(c)) => {
                        if !c.select_value(v) {
                            log::warn!("chase params: {k} has no option {v}");
                        }
                    }
                    Some(Param::Range(r)) => {
                        if let Ok(x) = v.parse::<f64>() {
                            r.value = x.clamp(r.min, r.max);
                        }
                    }
                    Some(Param::Bool(b)) => b.value = matches!(v, "1" | "true" | "on"),
                    _ => log::warn!("chase params: no param {k}"),
                }
            }
        });
    }

    pub(super) fn open_slice_chase(&mut self) {
        self.chase.open = true;
    }

    pub(super) fn slice_chase_window(&mut self, ctx: &egui::Context) {
        if !self.chase.open {
            return;
        }
        let id = egui::ViewportId::from_hash_of("lavmfx-slice-chase");
        let b = egui::ViewportBuilder::default()
            .with_title("Slice Chase")
            .with_app_id("lavmfx-slice-chase")
            .with_inner_size([1000.0, 640.0]);
        ctx.show_viewport_immediate(id, b, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                self.chase.open = false;
                return;
            }
            self.slice_chase_ui(ui);
            // Live levels on the map.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        });
    }

    fn slice_chase_ui(&mut self, ui: &mut egui::Ui) {
        let infos = chase::slice_infos(&self.outputs.setup);
        let screens: Vec<String> = self.outputs.setup.screens.iter().map(|s| s.name.clone()).collect();
        let wall = chase::wall_screens(&self.outputs.setup);
        let st = &mut self.chase;
        let setup = &mut self.comp.chase;
        let mut renamed: Option<(String, String)> = None;

        egui::Panel::left("chase-list")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                egui::Panel::bottom("chase-help")
                    .resizable(false)
                    .default_size(150.0)
                    .show(ui, |ui| {
                        section_header(ui, "chase-help-band", "Help", true, false);
                        ui.add(egui::Label::new(RichText::new(HELP).weak()).wrap());
                    });
                section_header(ui, "chase-seq-band", "Sequences", true, false);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("+ Rule")
                        .on_hover_text("A sequence worked out from the slices (order, grouping, filters)")
                        .clicked()
                    {
                        let name = setup.unique_name("Sequence");
                        setup.sequences.push(Sequence {
                            name,
                            ..Default::default()
                        });
                        select(st, setup.sequences.len() - 1);
                    }
                    if ui
                        .button("+ Hand-made")
                        .on_hover_text("Click slices on the map into each step")
                        .clicked()
                    {
                        let name = setup.unique_name("Steps");
                        setup.sequences.push(Sequence {
                            name,
                            rule: None,
                            steps: vec![Vec::new()],
                        });
                        select(st, setup.sequences.len() - 1);
                    }
                });
                let has = st.selected < setup.sequences.len();
                ui.horizontal(|ui| {
                    if ui.add_enabled(has, egui::Button::new("Duplicate")).clicked() {
                        let mut s = setup.sequences[st.selected].clone();
                        s.name = setup.unique_name(&s.name);
                        setup.sequences.insert(st.selected + 1, s);
                        select(st, st.selected + 1);
                    }
                    if ui
                        .add_enabled(has, egui::Button::new("Delete"))
                        .on_hover_text("Effects playing it fall back to All Slices")
                        .clicked()
                    {
                        setup.sequences.remove(st.selected);
                        select(st, st.selected.saturating_sub(1));
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, s) in setup.sequences.iter().enumerate() {
                        let kind = if s.rule.is_some() { "rule" } else { "hand-made" };
                        let r = ui
                            .add_sized(
                                [ui.available_width(), theme::ROW_H],
                                egui::Button::selectable(st.selected == i, &s.name).truncate(),
                            )
                            .on_hover_text(format!("{} ({kind})", s.name));
                        if r.clicked() {
                            select(st, i);
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let pal = theme::pal(ui);
            if setup.sequences.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "No sequences yet. Slice Chase effects play All Slices (every slice, one per step, \
                             in Advanced Output order) until you make one with + Rule or + Hand-made.",
                        )
                        .weak(),
                    )
                    .wrap(),
                );
                slice_map(ui, &pal, &infos, &wall, &[], None, st.live.get(chase::ALL_SLICES));
                return;
            }
            st.selected = st.selected.min(setup.sequences.len() - 1);
            let sel = st.selected;

            // Name (renaming updates the effects that play it).
            let old = setup.sequences[sel].name.clone();
            let mut name = st.name_edit.clone().unwrap_or_else(|| old.clone());
            let r = label_row(ui, "Name", |ui| {
                ui.add(egui::TextEdit::singleline(&mut name).desired_width(260.0))
            });
            if r.changed() {
                st.name_edit = Some(name.clone());
            }
            let enter = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (r.lost_focus() || enter) && st.name_edit.is_some() {
                st.name_edit = None;
                let wanted = name.trim();
                if !wanted.is_empty() && wanted != old {
                    let mut n = setup.unique_name(wanted);
                    if n == chase::ALL_SLICES {
                        n = setup.unique_name(&format!("{n} 2"));
                    }
                    setup.sequences[sel].name = n.clone();
                    renamed = Some((old.clone(), n));
                }
            }

            // Rule or hand-made.
            let is_rule = setup.sequences[sel].rule.is_some();
            label_row(ui, "Steps", |ui| {
                if ui
                    .selectable_label(is_rule, "From a rule")
                    .on_hover_text("Worked out from the slices each frame (replaces hand-made steps)")
                    .clicked()
                    && !is_rule
                {
                    setup.sequences[sel].rule = Some(Rule::default());
                }
                if ui
                    .selectable_label(!is_rule, "Hand-made")
                    .on_hover_text("Starts from the rule's current steps; click slices on the map to change them")
                    .clicked()
                    && is_rule
                {
                    setup.sequences[sel].bake(&infos);
                    st.step = 0;
                }
            });

            let seq = &mut setup.sequences[sel];
            if let Some(rule) = &mut seq.rule {
                rule_ui(ui, rule, &screens);
            } else {
                steps_ui(ui, &pal, seq, st, &infos);
            }
            let steps = seq.resolve(&infos);
            let used: usize = steps.iter().map(Vec::len).sum();
            ui.label(RichText::new(format!("{} steps · {used} slice entries", steps.len())).weak());
            ui.separator();

            let edit_step = seq.rule.is_none().then_some(st.step);
            if let Some(i) = slice_map(ui, &pal, &infos, &wall, &steps, edit_step, st.live.get(&seq.name))
                && let Some(k) = edit_step
            {
                // Click: the slice in / out of the step being edited.
                if seq.steps.is_empty() {
                    seq.steps.push(Vec::new());
                }
                let k = k.min(seq.steps.len() - 1);
                let step = &mut seq.steps[k];
                match step.iter().position(|r| chase::find(r, &infos, true) == Some(i)) {
                    Some(at) => {
                        step.remove(at);
                    }
                    None => step.push(SliceRef::of(&infos[i])),
                }
            }
        });

        if let Some((old, new)) = &renamed {
            self.comp.sync_chase(Some((old, new)));
        }
    }
}

fn select(st: &mut ChaseState, i: usize) {
    st.selected = i;
    st.step = 0;
    st.name_edit = None;
}

fn rule_ui(ui: &mut egui::Ui, rule: &mut Rule, screens: &[String]) {
    label_row(ui, "Order", |ui| {
        egui::ComboBox::from_id_salt("chase-order")
            .selected_text(rule.order.name())
            .show_ui(ui, |ui| {
                for (o, n) in Order::ALL {
                    ui.selectable_value(&mut rule.order, o, n);
                }
            });
    });
    let mut per = rule.per_step as f32;
    if value_row(ui, "Per step", &mut per, 0.0..=64.0, 1.0, 1.0, false) {
        rule.per_step = per.round() as u32;
    }
    ui.add_enabled_ui(rule.order.spatial(), |ui| {
        bool_row(
            ui,
            "Bands",
            &mut rule.bands,
            "Slices at about the same spot share a step: columns (left → right), rows (top → bottom), \
             rings (centre → out), spokes (clockwise). Per step is unused then.",
        );
    });
    if rule.per_step == 0 && !(rule.bands && rule.order.spatial()) {
        ui.label(RichText::new("Per step 0: every slice in one step").weak());
    }
    label_row(ui, "Screen", |ui| {
        let shown = if rule.screen.is_empty() {
            "All screens".to_owned()
        } else {
            rule.screen.clone()
        };
        egui::ComboBox::from_id_salt("chase-screen")
            .selected_text(shown)
            .truncate()
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut rule.screen, String::new(), "All screens");
                for s in screens {
                    ui.selectable_value(&mut rule.screen, s.clone(), s);
                }
            });
    });
    label_row(ui, "Name has", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut rule.filter)
                .hint_text("any name")
                .desired_width(200.0),
        )
    });
    if rule.order == Order::Shuffle {
        let mut seed = rule.seed as f32;
        if value_row(ui, "Seed", &mut seed, 0.0..=999.0, 1.0, 0.0, false) {
            rule.seed = seed.round() as u32;
        }
    }
}

fn steps_ui(ui: &mut egui::Ui, pal: &theme::Palette, seq: &mut Sequence, st: &mut ChaseState, infos: &[SliceInfo]) {
    if seq.steps.is_empty() {
        seq.steps.push(Vec::new());
    }
    st.step = st.step.min(seq.steps.len() - 1);
    egui::ScrollArea::horizontal().id_salt("chase-steps").show(ui, |ui| {
        ui.horizontal(|ui| {
            for (k, step) in seq.steps.iter().enumerate() {
                let n = step.len();
                let names: Vec<String> = step
                    .iter()
                    .map(|r| match chase::find(r, infos, false) {
                        Some(i) => infos[i].name.clone(),
                        None => format!("{} (missing)", r.slice_name),
                    })
                    .collect();
                let hover = if names.is_empty() {
                    format!("Step {}: rest (nothing lit)", k + 1)
                } else {
                    format!("Step {} · {n} slice(s): {}", k + 1, names.join(", "))
                };
                // Rests are dimmed.
                let mut text = RichText::new((k + 1).to_string());
                if n == 0 {
                    text = text.color(pal.text_weak);
                }
                let b = egui::Button::selectable(st.step == k, text).min_size(vec2(30.0, theme::ROW_H));
                if ui.add(b).on_hover_text(hover).clicked() {
                    st.step = k;
                }
            }
        });
    });
    let k = st.step;
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("+ Step")
            .on_hover_text("New empty step after this one")
            .clicked()
        {
            seq.steps.insert(k + 1, Vec::new());
            st.step = k + 1;
        }
        if ui
            .button("Copy step")
            .on_hover_text("Same slices in a new step after this one")
            .clicked()
        {
            let s = seq.steps[k].clone();
            seq.steps.insert(k + 1, s);
            st.step = k + 1;
        }
        if ui
            .add_enabled(seq.steps.len() > 1, egui::Button::new("Delete step"))
            .clicked()
        {
            seq.steps.remove(k);
            st.step = k.saturating_sub(1);
        }
        if ui
            .add_enabled(k > 0, egui::Button::new("Earlier"))
            .on_hover_text("Move the step earlier")
            .clicked()
        {
            seq.steps.swap(k, k - 1);
            st.step = k - 1;
        }
        if ui
            .add_enabled(k + 1 < seq.steps.len(), egui::Button::new("Later"))
            .on_hover_text("Move the step later")
            .clicked()
        {
            seq.steps.swap(k, k + 1);
            st.step = k + 1;
        }
        if ui
            .button("Clear")
            .on_hover_text("No slices in this step (a rest)")
            .clicked()
        {
            seq.steps[k].clear();
        }
        if ui
            .button("Reverse")
            .on_hover_text("Play the steps in reverse order")
            .clicked()
        {
            seq.steps.reverse();
            st.step = seq.steps.len() - 1 - k;
        }
    });
    let missing = seq
        .steps
        .iter()
        .flatten()
        .filter(|r| chase::find(r, infos, false).is_none())
        .count();
    if missing > 0 {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{missing} slice(s) not in the Advanced Output")).color(pal.danger))
                .on_hover_text("Kept: they come back when the slice does (same name or id)");
            if ui.button("Remove missing").clicked() {
                for s in &mut seq.steps {
                    s.retain(|r| chase::find(r, infos, false).is_some());
                }
            }
        });
    }
    ui.label(RichText::new("Click slices on the map to put them in / take them out of the step.").weak());
}

/// The slices in composition space: steps numbered, the edited step
/// highlighted, live levels lit. Returns a clicked slice.
fn slice_map(
    ui: &mut egui::Ui,
    pal: &theme::Palette,
    infos: &[SliceInfo],
    wall: &[(String, [f32; 4])],
    steps: &[Vec<usize>],
    edit_step: Option<usize>,
    live: Option<&Vec<f32>>,
) -> Option<usize> {
    let avail = ui.available_size();
    let (rect, resp) = ui.allocate_exact_size(vec2(avail.x, avail.y.max(160.0)), egui::Sense::click());
    let painter = ui.painter_at(rect);
    // The wall: the screens side by side, slices at their output places.
    let (cw, ch) = wall.iter().fold((1.0f32, 1.0f32), |(w, h), (_, r)| {
        (w.max(r[0] + r[2]), h.max(r[1] + r[3]))
    });
    let s = ((rect.width() - 16.0) / cw).min((rect.height() - 34.0) / ch).max(0.001);
    let origin = rect.center() - vec2(cw * s, ch * s) * 0.5 + vec2(0.0, 9.0);
    let to = |p: [f32; 2]| pos2(origin.x + p[0] * s, origin.y + p[1] * s);
    let frame = egui::Rect::from_min_size(origin, vec2(cw * s, ch * s));
    for (name, r) in wall {
        let sr = egui::Rect::from_min_max(to([r[0], r[1]]), to([r[0] + r[2], r[1] + r[3]]));
        painter.rect_filled(sr, 0.0, pal.video_empty);
        painter.rect_stroke(sr, 0.0, Stroke::new(1.0, pal.border), egui::StrokeKind::Outside);
        // Screen name above its frame, cut to the frame's width.
        let mut job =
            egui::text::LayoutJob::simple_singleline(name.clone(), egui::FontId::proportional(11.0), pal.text_weak);
        job.wrap = egui::text::TextWrapping::truncate_at_width(sr.width().max(10.0));
        let g = painter.ctx().fonts_mut(|f| f.layout_job(job));
        painter.galley(sr.left_top() - vec2(0.0, g.size().y + 2.0), g, pal.text_weak);
    }
    if wall.is_empty() {
        painter.rect_filled(frame, 0.0, pal.video_empty);
    }
    if infos.is_empty() {
        painter.text(
            frame.center(),
            egui::Align2::CENTER_CENTER,
            "No slices: make some in Output › Advanced Output",
            egui::FontId::proportional(13.0),
            pal.text_weak,
        );
        return None;
    }
    let mut steps_of: Vec<Vec<usize>> = vec![Vec::new(); infos.len()];
    for (k, step) in steps.iter().enumerate() {
        for &i in step {
            if let Some(v) = steps_of.get_mut(i) {
                v.push(k);
            }
        }
    }
    let pointer = resp.hover_pos();
    let mut hovered = None;
    for (i, sl) in infos.iter().enumerate() {
        if pointer.is_some_and(|p| inside(p, &sl.shape.iter().map(|q| to(*q)).collect::<Vec<_>>())) {
            hovered = Some(i);
        }
    }
    for (i, sl) in infos.iter().enumerate() {
        let pts: Vec<Pos2> = sl.shape.iter().map(|q| to(*q)).collect();
        let in_edit = edit_step.is_some_and(|k| steps_of[i].contains(&k));
        let member = !steps_of[i].is_empty();
        let mut fill = if in_edit {
            pal.accent.gamma_multiply(0.55)
        } else if member {
            pal.accent_dim.gamma_multiply(0.35)
        } else {
            Color32::TRANSPARENT
        };
        if let Some(l) = live.and_then(|v| v.get(i)).copied().filter(|l| *l > 0.01) {
            fill = lerp(fill, pal.text_strong, l * 0.8);
        }
        fill_polygon(&painter, &pts, fill);
        let stroke = if hovered == Some(i) {
            Stroke::new(2.0, pal.accent)
        } else {
            Stroke::new(1.0, if member { pal.text_weak } else { pal.border })
        };
        painter.add(egui::Shape::closed_line(pts, stroke));
        if member {
            let label = step_label(&steps_of[i]);
            painter.text(
                to(sl.center),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                if in_edit { pal.on_accent } else { pal.text },
            );
        }
    }
    if let Some(i) = hovered {
        let sl = &infos[i];
        let steps = if steps_of[i].is_empty() {
            "not in this sequence".to_owned()
        } else {
            format!("step {}", step_list(&steps_of[i]))
        };
        resp.clone()
            .on_hover_text(format!("{} · {} · {steps}", sl.name, sl.screen));
    }
    if resp.clicked() { hovered } else { None }
}

/// Short step numbers for a slice's centre ("3", "1,4", "2,5,8…").
fn step_label(v: &[usize]) -> String {
    let mut s: Vec<String> = v.iter().take(3).map(|k| (k + 1).to_string()).collect();
    if v.len() > 3 {
        s.push("…".into());
    }
    s.join(",")
}

fn step_list(v: &[usize]) -> String {
    v.iter().map(|k| (k + 1).to_string()).collect::<Vec<_>>().join(", ")
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    let alpha = if a.a() == 0 { (t * 255.0) as u8 } else { m(a.a(), b.a()) };
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), alpha)
}

/// Even-odd point in polygon.
fn inside(p: Pos2, poly: &[Pos2]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

/// Fill any simple polygon (slices may be concave).
fn fill_polygon(painter: &egui::Painter, pts: &[Pos2], fill: Color32) {
    if fill.a() == 0 || pts.len() < 3 {
        return;
    }
    let flat: Vec<[f32; 2]> = pts.iter().map(|p| [p.x, p.y]).collect();
    let mut mesh = egui::Mesh::default();
    for p in pts {
        mesh.colored_vertex(*p, fill);
    }
    mesh.indices = lavmfx_core::output::triangulate(&flat);
    painter.add(egui::Shape::mesh(mesh));
}
