//! Layer / column / clip structure edits and their menus (menu bar and
//! right-click): the Layer, Column and Clip menus.
//!
//! Per-layer app state (playing, opening, outgoing clips, compositor
//! textures, selection) is indexed by layer, so inserting or removing a
//! layer shifts it; column edits shift slot columns in the active deck.

use eframe::egui;

use super::LavmfxApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GridAction {
    /// Insert an empty layer at this index (0 = bottom).
    InsertLayer(usize),
    RemoveLayer(usize),
    ClearLayer(usize),
    /// Insert an empty column at this index.
    InsertColumn(usize),
    RemoveColumn(usize),
    ClearColumn(usize),
    SelectAllClips,
    ClearClips,
    /// New group holding this layer.
    NewGroup(usize),
    /// Put layer (first) into group (second).
    AddToGroup(usize, usize),
    /// Take this layer out of its group.
    Ungroup(usize),
    /// Dissolve this group (layers stay).
    RemoveGroup(usize),
    /// Open the rename dialog.
    Rename(RenameTarget),
}

/// What the rename dialog renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenameTarget {
    Layer(usize),
    Group(usize),
    Deck(usize),
}

/// What the Layer menu needs to know about groups around a layer.
#[derive(Debug, Default, Clone)]
pub(super) struct LayerMenuCtx {
    pub layers: usize,
    /// The layer's group (index, name).
    pub group: Option<(usize, String)>,
    /// Groups of the layers right above/below (that it could join).
    pub neighbours: Vec<(usize, String)>,
}

/// Items of the Layer menu for layer `li` (`None`: no layer selected).
pub(super) fn layer_menu(ui: &mut egui::Ui, li: Option<usize>, ctx: &LayerMenuCtx) -> Option<GridAction> {
    let layers = ctx.layers;
    let mut hit = None;
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, a: GridAction| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            hit = Some(a);
            ui.close();
        }
    };
    if let Some(li) = li {
        ui.label(egui::RichText::new(format!("Layer {}", li + 1)).weak());
    }
    item(ui, true, "New    Ctrl+L", GridAction::InsertLayer(layers));
    let l = li.unwrap_or(0);
    item(ui, li.is_some(), "Insert Above", GridAction::InsertLayer(l + 1));
    item(ui, li.is_some(), "Insert Below", GridAction::InsertLayer(l));
    item(ui, li.is_some(), "Rename…", GridAction::Rename(RenameTarget::Layer(l)));
    ui.separator();
    let mut group_hit = None;
    ui.add_enabled_ui(li.is_some(), |ui| {
        ui.menu_button("Group", |ui| {
            if ui.button("New Group").clicked() {
                group_hit = Some(GridAction::NewGroup(l));
                ui.close();
            }
            for (gi, name) in &ctx.neighbours {
                if ui.button(format!("Add to {name}")).clicked() {
                    group_hit = Some(GridAction::AddToGroup(l, *gi));
                    ui.close();
                }
            }
            if let Some((_, name)) = &ctx.group
                && ui.button(format!("Remove from {name}")).clicked()
            {
                group_hit = Some(GridAction::Ungroup(l));
                ui.close();
            }
        });
    });
    ui.separator();
    item(ui, li.is_some(), "Clear Clips", GridAction::ClearLayer(l));
    item(ui, li.is_some() && layers > 1, "Remove", GridAction::RemoveLayer(l));
    hit.or(group_hit)
}

/// Items of the Column menu for column `ci`.
pub(super) fn column_menu(ui: &mut egui::Ui, ci: Option<usize>, columns: usize) -> Option<GridAction> {
    let mut hit = None;
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, a: GridAction| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            hit = Some(a);
            ui.close();
        }
    };
    if let Some(ci) = ci {
        ui.label(egui::RichText::new(format!("Column {}", ci + 1)).weak());
    }
    item(ui, true, "New    Ctrl+T", GridAction::InsertColumn(columns));
    let c = ci.unwrap_or(0);
    item(ui, ci.is_some(), "Insert Before", GridAction::InsertColumn(c));
    item(ui, ci.is_some(), "Insert After", GridAction::InsertColumn(c + 1));
    ui.separator();
    item(ui, ci.is_some(), "Clear Clips", GridAction::ClearColumn(c));
    item(ui, ci.is_some() && columns > 1, "Remove", GridAction::RemoveColumn(c));
    hit
}

/// Index of layer `l` after moving layer `from` to `to`.
pub(super) fn moved_index(l: usize, from: usize, to: usize) -> usize {
    if l == from {
        to
    } else if from < to && (from + 1..=to).contains(&l) {
        l - 1
    } else if to < from && (to..from).contains(&l) {
        l + 1
    } else {
        l
    }
}

/// Final index of a layer dragged from `from` and dropped in gap `gap`
/// (gap = number of layers below the drop line, counted before the move).
pub(super) fn drop_index(from: usize, gap: usize) -> usize {
    if gap > from { gap - 1 } else { gap }
}

/// Group a layer dragged from `from` belongs to after dropping it in `gap`
/// (gap = number of layers below the drop line, original order).
/// `groups[l]` = group of layer `l`. Between two members of a group = that
/// group. Right above a group's top member, i.e. on its band: inside unless
/// `above_band` (pointer on the band's upper half). Anywhere else: none.
pub(super) fn drop_group(groups: &[Option<usize>], from: usize, gap: usize, above_band: bool) -> Option<usize> {
    let below = (0..gap.min(groups.len()))
        .rev()
        .find(|&l| l != from)
        .and_then(|l| groups[l]);
    let above = (gap..groups.len()).find(|&l| l != from).and_then(|l| groups[l]);
    match (below, above) {
        (Some(b), Some(a)) if a == b => Some(b),
        (Some(b), _) if !above_band => Some(b),
        _ => None,
    }
}

/// Shift a layer index after inserting at `at` (`+1`) or removing `at` (`None` = gone).
fn shift_layer(l: usize, at: usize, insert: bool) -> Option<usize> {
    match (insert, l.cmp(&at)) {
        (_, std::cmp::Ordering::Less) => Some(l),
        (true, _) => Some(l + 1),
        (false, std::cmp::Ordering::Equal) => None,
        (false, std::cmp::Ordering::Greater) => Some(l - 1),
    }
}

impl LavmfxApp {
    pub(super) fn apply_grid(&mut self, a: GridAction) {
        match a {
            GridAction::InsertLayer(at) => self.insert_layer(at),
            GridAction::RemoveLayer(li) => self.remove_layer(li),
            GridAction::ClearLayer(li) => {
                self.stop_layer_now(li);
                self.comp.clear_layer_clips(li);
                self.selection.retain(|s| s.0 != li);
            }
            GridAction::InsertColumn(at) => self.shift_columns(at, true),
            GridAction::RemoveColumn(ci) => self.shift_columns(ci, false),
            GridAction::ClearColumn(ci) => {
                let deck = self.comp.active_deck();
                for li in 0..self.comp.layers.len() {
                    if self.playing_slot(li).is_some_and(|s| s.deck == deck && s.col == ci) {
                        self.stop_layer_now(li);
                    }
                }
                self.comp.clear_column_clips(ci);
                self.selection.retain(|s| s.1 != ci);
            }
            GridAction::SelectAllClips => {
                self.selection = (0..self.comp.layers.len())
                    .flat_map(|l| (0..self.comp.columns.len()).map(move |c| (l, c)))
                    .filter(|&(l, c)| !self.comp.layers[l].clips[c].is_empty())
                    .collect();
            }
            GridAction::ClearClips => self.eject_selection(),
            GridAction::NewGroup(li) => {
                let n = self.comp.layergroups.len() + 1;
                let gi = self.comp.new_group(&[li], &format!("Group {n}"));
                self.selected_group = Some(gi);
            }
            GridAction::AddToGroup(li, gi) => self.comp.add_to_group(li, gi),
            GridAction::Ungroup(li) => {
                self.comp.ungroup_layer(li);
                self.selected_group = None;
            }
            GridAction::Rename(t) => {
                let name = match t {
                    RenameTarget::Layer(l) => self.comp.layers.get(l).map(|x| x.name.value.clone()),
                    RenameTarget::Group(g) => self.comp.layergroups.get(g).map(|x| x.name.value.clone()),
                    RenameTarget::Deck(d) => self.comp.decks.get(d).map(|x| x.name.value.clone()),
                };
                if let Some(n) = name {
                    self.rename = Some((t, n));
                }
            }
            GridAction::RemoveGroup(gi) => {
                self.comp.remove_group(gi);
                self.selected_group = None;
            }
        }
    }

    fn playing_slot(&self, li: usize) -> Option<crate::playback::Slot> {
        self.playing.get(li)?.as_ref().map(|p| p.slot)
    }

    /// Re-index everything keyed by layer after inserting at / removing `at`.
    fn shift_layer_state(&mut self, at: usize, insert: bool) {
        self.remap_layers(|l| shift_layer(l, at, insert));
    }

    /// Re-index slots and selection by layer (`None` = the layer is gone).
    fn remap_layers(&mut self, fix: impl Fn(usize) -> Option<usize>) {
        for pb in self.playing.iter_mut().flatten() {
            pb.slot.layer = fix(pb.slot.layer).unwrap_or(pb.slot.layer);
        }
        for o in self.opening.iter_mut().flatten() {
            o.slot.layer = fix(o.slot.layer).unwrap_or(o.slot.layer);
        }
        for o in self.outgoing.iter_mut().flatten() {
            if let Some(pb) = &mut o.pb {
                pb.slot.layer = fix(pb.slot.layer).unwrap_or(pb.slot.layer);
            }
        }
        self.selected_layer = self.selected_layer.and_then(&fix);
        self.selected_clip = self.selected_clip.and_then(|(l, c)| Some((fix(l)?, c)));
        self.selection = self.selection.iter().filter_map(|&(l, c)| Some((fix(l)?, c))).collect();
    }

    fn insert_layer(&mut self, at: usize) {
        self.ensure_layers();
        let at = self.comp.insert_layer(at);
        self.playing.insert(at, None);
        self.opening.insert(at, None);
        self.outgoing.insert(at, None);
        self.compositor.insert_layer(&self.gpu.device, at);
        self.shift_layer_state(at, true);
        self.selected_layer = Some(at);
    }

    /// Move layer `from` to index `to` (drag in the grid); playing clips go with it.
    pub(super) fn move_layer(&mut self, from: usize, to: usize) {
        self.ensure_layers();
        if from == to || !self.comp.move_layer(from, to) {
            return;
        }
        let x = self.playing.remove(from);
        self.playing.insert(to, x);
        let x = self.opening.remove(from);
        self.opening.insert(to, x);
        let x = self.outgoing.remove(from);
        self.outgoing.insert(to, x);
        self.compositor.move_layer(&self.gpu.device, from, to);
        self.remap_layers(|l| Some(moved_index(l, from, to)));
    }

    fn remove_layer(&mut self, li: usize) {
        if self.comp.layers.len() <= 1 || li >= self.comp.layers.len() {
            return;
        }
        self.stop_layer_now(li);
        self.comp.remove_layer(li);
        self.playing.remove(li);
        self.opening.remove(li);
        self.outgoing.remove(li);
        self.compositor.remove_layer(li);
        self.shift_layer_state(li, false);
        self.status = format!("Removed layer {}", li + 1);
    }

    /// Insert an empty column at `at`, or remove column `at`, in the active deck.
    fn shift_columns(&mut self, at: usize, insert: bool) {
        let deck = self.comp.active_deck();
        if !insert {
            if self.comp.columns.len() <= 1 || at >= self.comp.columns.len() {
                return;
            }
            for li in 0..self.comp.layers.len() {
                if self.playing_slot(li).is_some_and(|s| s.deck == deck && s.col == at) {
                    self.stop_layer_now(li);
                }
            }
            self.comp.remove_column(at);
        } else {
            self.comp.insert_column(at);
        }
        let fix = |c: usize| shift_layer(c, at, insert);
        for pb in self.playing.iter_mut().flatten().filter(|p| p.slot.deck == deck) {
            pb.slot.col = fix(pb.slot.col).unwrap_or(pb.slot.col);
        }
        for o in self.opening.iter_mut().flatten().filter(|o| o.slot.deck == deck) {
            o.slot.col = fix(o.slot.col).unwrap_or(o.slot.col);
        }
        for o in self.outgoing.iter_mut().flatten() {
            if let Some(pb) = o.pb.as_mut().filter(|p| p.slot.deck == deck) {
                pb.slot.col = fix(pb.slot.col).unwrap_or(pb.slot.col);
            }
        }
        self.launched_column = self
            .launched_column
            .and_then(|(d, c)| if d == deck { Some((d, fix(c)?)) } else { Some((d, c)) });
        self.selected_clip = self.selected_clip.and_then(|(l, c)| Some((l, fix(c)?)));
        self.selection = self.selection.iter().filter_map(|&(l, c)| Some((l, fix(c)?))).collect();
    }

    /// Remove deck `d` with its clips: whatever plays or previews from it
    /// stops; slots of later decks shift down.
    pub(super) fn remove_deck(&mut self, d: usize) {
        if self.comp.decks.len() <= 1 {
            return;
        }
        let name = self.comp.decks.get(d).map(|x| x.name.value.clone()).unwrap_or_default();
        for li in 0..self.comp.layers.len() {
            if self.playing_slot(li).is_some_and(|s| s.deck == d) {
                self.stop_layer_now(li);
            }
            if self
                .opening
                .get(li)
                .and_then(Option::as_ref)
                .is_some_and(|o| o.slot.deck == d)
            {
                self.opening[li] = None;
            }
        }
        if self.preview.as_ref().is_some_and(|p| p.slot.deck == d)
            || self.preview_opening.as_ref().is_some_and(|o| o.slot.deck == d)
        {
            self.stop_preview();
        }
        let was_active = self.comp.active_deck() == d;
        if self.comp.remove_deck(d).is_none() {
            return;
        }
        let fix = |x: &mut usize| {
            if *x > d {
                *x -= 1;
            }
        };
        for pb in self.playing.iter_mut().flatten() {
            fix(&mut pb.slot.deck);
        }
        for o in self.opening.iter_mut().flatten() {
            fix(&mut o.slot.deck);
        }
        for pb in self.outgoing.iter_mut().flatten().filter_map(|o| o.pb.as_mut()) {
            fix(&mut pb.slot.deck);
        }
        if let Some(pb) = &mut self.preview {
            fix(&mut pb.slot.deck);
        }
        if let Some(o) = &mut self.preview_opening {
            fix(&mut o.slot.deck);
        }
        self.launched_column = match self.launched_column {
            Some((x, _)) if x == d => None,
            Some((x, c)) => Some((if x > d { x - 1 } else { x }, c)),
            None => None,
        };
        if was_active {
            self.select_none();
        }
        self.status = format!("Removed {name}");
    }

    /// Group facts for the Layer menu of layer `li`.
    pub(super) fn layer_menu_ctx(&self, li: Option<usize>) -> LayerMenuCtx {
        let c = &self.comp;
        let named = |gi: usize| (gi, c.layergroups[gi].name.value.clone());
        let Some(li) = li else {
            return LayerMenuCtx {
                layers: c.layers.len(),
                ..Default::default()
            };
        };
        let own = c.group_of(li);
        let mut neighbours: Vec<(usize, String)> = [li.checked_sub(1), Some(li + 1)]
            .into_iter()
            .flatten()
            .filter_map(|n| c.group_of(n))
            .filter(|g| Some(*g) != own)
            .map(named)
            .collect();
        neighbours.dedup();
        LayerMenuCtx {
            layers: c.layers.len(),
            group: own.map(named),
            neighbours,
        }
    }

    /// Column the Column menu acts on: the focused clip's, else the last launched.
    pub(super) fn focus_column(&self) -> Option<usize> {
        let deck = self.comp.active_deck();
        self.selected_clip
            .map(|(_, c)| c)
            .or(self.launched_column.filter(|(d, _)| *d == deck).map(|(_, c)| c))
            .filter(|&c| c < self.comp.columns.len())
    }

    /// Menu-bar Clip menu.
    pub(super) fn clip_menu(&mut self, ui: &mut egui::Ui) {
        let any = !self.selection.is_empty();
        if ui.button("Select All    Ctrl+A").clicked() {
            self.apply_grid(GridAction::SelectAllClips);
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(any, egui::Button::new("Cut    Ctrl+X")).clicked() {
            self.copy_selection(ui.ctx(), true);
            ui.close();
        }
        if ui.add_enabled(any, egui::Button::new("Copy    Ctrl+C")).clicked() {
            self.copy_selection(ui.ctx(), false);
            ui.close();
        }
        let paste = !self.clipboard.is_empty() && self.selected_clip.is_some();
        if ui.add_enabled(paste, egui::Button::new("Paste    Ctrl+V")).clicked() {
            self.paste_clips();
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(self.selected_clip.is_some(), egui::Button::new("Render to File"))
            .on_hover_text(
                "Render the selected clip with its effects (Preferences › Clip Rendering); see the Render Queue",
            )
            .clicked()
        {
            self.render_selected_clip();
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(any, egui::Button::new("Clear    Del")).clicked() {
            self.apply_grid(GridAction::ClearClips);
            ui.close();
        }
    }

    /// Ctrl+L new layer, Ctrl+T new column, Ctrl+A select all clips.
    pub(super) fn structure_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let sc = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::L))) {
            self.apply_grid(GridAction::InsertLayer(self.comp.layers.len()));
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::T))) {
            self.apply_grid(GridAction::InsertColumn(self.comp.columns.len()));
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Key::A))) {
            self.apply_grid(GridAction::SelectAllClips);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{drop_group, drop_index, moved_index, shift_layer};

    #[test]
    fn dropping_joins_or_leaves_groups() {
        // Layers 0..5; group 0 = layers 1, 2; group 1 = layer 4.
        let g = [None, Some(0), Some(0), None, Some(1)];
        assert_eq!(drop_group(&g, 0, 2, false), Some(0), "between members");
        assert_eq!(drop_group(&g, 0, 3, false), Some(0), "under the band, above top member");
        assert_eq!(drop_group(&g, 0, 3, true), None, "on the band's upper half");
        assert_eq!(drop_group(&g, 3, 1, false), None, "below the bottom member");
        assert_eq!(
            drop_group(&g, 1, 5, false),
            Some(1),
            "top of the list, under group 1's band"
        );
        assert_eq!(
            drop_group(&g, 2, 3, false),
            Some(0),
            "member stays when dropped in place"
        );
        assert_eq!(drop_group(&g, 2, 3, true), None, "dragged out over its band");
        assert_eq!(drop_group(&g, 1, 0, false), None, "bottom of the list");
    }

    #[test]
    fn layer_move_indices() {
        // 4 layers, move 0 to the top.
        assert_eq!((0..4).map(|l| moved_index(l, 0, 3)).collect::<Vec<_>>(), [3, 0, 1, 2]);
        assert_eq!((0..4).map(|l| moved_index(l, 3, 1)).collect::<Vec<_>>(), [0, 2, 3, 1]);
        assert_eq!(drop_index(0, 4), 3); // above the top layer
        assert_eq!(drop_index(3, 0), 0); // below the bottom layer
        assert_eq!(drop_index(1, 1), 1); // its own gap: no move
        assert_eq!(drop_index(1, 2), 1);
    }

    #[test]
    fn layer_shift() {
        assert_eq!(shift_layer(0, 1, true), Some(0));
        assert_eq!(shift_layer(1, 1, true), Some(2));
        assert_eq!(shift_layer(1, 1, false), None);
        assert_eq!(shift_layer(2, 1, false), Some(1));
    }
}
