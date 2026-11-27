//! Docking shell: panels are tabs in an `egui_dock` tree that the user can
//! drag, split, close and undock. Layouts are saved as JSON in the config
//! directory (`layouts/<name>.json`; the current one in `layout.json`).

use std::path::PathBuf;

use eframe::egui::{self, Id, Ui, WidgetText};
use egui_dock::{DockState, NodeIndex, TabViewer};
use serde::{Deserialize, Serialize};

use super::LavmfxApp;

/// A dockable panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    /// Layers & clips: the clip grid with deck tabs.
    Clips,
    /// Composition monitor.
    Monitor,
    /// Preview monitor: a clip played on its own (thumbnail click).
    Preview,
    Composition,
    Layer,
    /// Selected layer group.
    Group,
    Clip,
    /// Effects browser (effects + presets to drag).
    Effects,
    /// Audio input spectrum + input choice (External FFT source).
    ExternalFft,
    /// Sources browser (generators to drag onto slots).
    Sources,
    /// Advanced output screens / slices / masks (Slices tab).
    Slices,
    /// SMPTE 1 / 2 timecode inputs (View › SMPTE).
    Smpte,
    /// Render queue (Clip › Render to File).
    Render,
}

impl Tab {
    pub const ALL: [Tab; 13] = [
        Tab::Clips,
        Tab::Monitor,
        Tab::Preview,
        Tab::Composition,
        Tab::Layer,
        Tab::Group,
        Tab::Clip,
        Tab::Effects,
        Tab::Sources,
        Tab::Slices,
        Tab::ExternalFft,
        Tab::Smpte,
        Tab::Render,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Clips => "Layers & Clips",
            Tab::Monitor => "Composition Monitor",
            Tab::Preview => "Preview Monitor",
            Tab::Composition => "Composition",
            Tab::Layer => "Layer",
            Tab::Group => "Group",
            Tab::Effects => "Effects",
            Tab::ExternalFft => "External FFT",
            Tab::Sources => "Sources",
            Tab::Slices => "Slices",
            Tab::Smpte => "SMPTE",
            Tab::Render => "Render Queue",
            Tab::Clip => "Clip",
        }
    }
}

/// View-menu toggles, saved with the layout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOptions {
    /// Mini transport (< || >) in each layer header.
    pub layer_transport: bool,
    /// Transition time + mode column right of each layer header.
    pub layer_transition: bool,
    /// Crossfader in the composition row above the grid.
    pub crossfader: bool,
    /// A / V toggles right of each layer header.
    #[serde(default = "yes")]
    pub audio_controls: bool,
    /// Toolbar row under the menu (BPM, tap, FFT gain).
    #[serde(default = "yes")]
    pub toolbar: bool,
}

fn yes() -> bool {
    true
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            layer_transport: true,
            layer_transition: true,
            crossfader: true,
            audio_controls: true,
            toolbar: true,
        }
    }
}

/// What a layout file holds.
#[derive(Serialize, Deserialize)]
pub struct LayoutFile {
    pub dock: DockState<Tab>,
    #[serde(default)]
    pub view: ViewOptions,
}

/// Built-in layouts.
pub const PRESETS: &[&str] = &[
    "Default",
    "Big Monitor",
    "Panels On Top",
    "Panels Left & Right",
    "Monitor Top Left",
];

/// A built-in layout (unknown names give "Default").
pub fn preset(name: &str) -> DockState<Tab> {
    let mut d = preset_base(name);
    add_preview(&mut d);
    add_group_tab(&mut d);
    add_effects_tab(&mut d);
    add_sources_tab(&mut d);
    add_slices_tab(&mut d);
    add_external_fft_tab(&mut d);
    d
}

/// Add the Sources browser next to the Effects browser if missing.
pub fn add_sources_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Sources) {
        return;
    }
    match dock.find_tab(&Tab::Effects) {
        Some(p) => {
            let active = dock[p.surface][p.node].tabs().and_then(|t| t.first().copied());
            dock[p.surface][p.node].append_tab(Tab::Sources);
            if let Some(a) = active {
                focus_tab(dock, a);
            }
        }
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Sources]),
        None => dock.push_to_first_leaf(Tab::Sources),
    }
}

/// Add the Slices tab next to the Effects browser if missing.
pub fn add_slices_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Slices) {
        return;
    }
    match dock.find_tab(&Tab::Effects) {
        Some(p) => {
            let active = dock[p.surface][p.node].tabs().and_then(|t| t.first().copied());
            dock[p.surface][p.node].append_tab(Tab::Slices);
            if let Some(a) = active {
                focus_tab(dock, a);
            }
        }
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Slices]),
        None => dock.push_to_first_leaf(Tab::Slices),
    }
}

/// Add the External FFT panel next to the Effects browser if missing.
pub fn add_external_fft_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::ExternalFft) {
        return;
    }
    match dock.find_tab(&Tab::Effects) {
        Some(p) => {
            let active = dock[p.surface][p.node].tabs().and_then(|t| t.first().copied());
            dock[p.surface][p.node].append_tab(Tab::ExternalFft);
            if let Some(a) = active {
                focus_tab(dock, a);
            }
        }
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::ExternalFft]),
        None => dock.push_to_first_leaf(Tab::ExternalFft),
    }
}

/// The Render Queue next to the Effects browser (like the External FFT panel).
pub fn add_render_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Render) {
        return;
    }
    match dock.find_tab(&Tab::Effects) {
        Some(p) => dock[p.surface][p.node].append_tab(Tab::Render),
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Render]),
        None => dock.push_to_first_leaf(Tab::Render),
    }
}

/// Add the Effects browser as a tab next to the Preview Monitor if missing
/// (browser bottom right).
pub fn add_effects_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Effects) {
        return;
    }
    match dock.find_tab(&Tab::Preview) {
        Some(p) => {
            dock[p.surface][p.node].append_tab(Tab::Effects);
            focus_tab(dock, Tab::Preview);
        }
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Effects]),
        None => dock.push_to_first_leaf(Tab::Effects),
    }
}

/// Add the Group tab next to the Layer tab if the layout lacks it.
pub fn add_group_tab(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Group) {
        return;
    }
    match dock.find_tab(&Tab::Layer) {
        Some(p) => {
            dock[p.surface][p.node].append_tab(Tab::Group);
            focus_tab(dock, Tab::Layer); // appending made Group the active tab
        }
        None if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Group]),
        None => dock.push_to_first_leaf(Tab::Group),
    }
}

/// Width a tab title takes in egui_dock's tab bar (its formula: text + 2 × 8
/// spacing + the close button).
fn tab_width(ui: &Ui, title: &str) -> f32 {
    let galley = egui::WidgetText::from(title).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
    galley.size().x + 16.0 + 22.0
}

/// A chevron on each panel's tab-menu button: bright when some tabs don't fit
/// the bar (they're in the menu), dim when all are visible.
fn tab_menu_chevrons(ui: &Ui, dock: &DockState<Tab>, bar_h: f32) {
    const BUTTON: f32 = 24.0; // egui_dock's add-button width
    let pal = super::theme::pal(ui);
    for (_, leaf) in dock.iter_leaves() {
        if leaf.tabs.is_empty() || leaf.tab_bar_hidden || leaf.rect.width() < BUTTON {
            continue;
        }
        let needed: f32 = leaf.tabs.iter().map(|t| tab_width(ui, t.title())).sum();
        let overflow = needed > leaf.rect.width() - BUTTON;
        let r = egui::Rect::from_min_size(
            egui::pos2(leaf.rect.right() - BUTTON, leaf.rect.top()),
            egui::vec2(BUTTON, bar_h),
        );
        let c = r.center();
        let color = if overflow {
            pal.text_strong
        } else {
            pal.text_weak.gamma_multiply(0.6)
        };
        ui.painter().add(egui::Shape::line(
            vec![
                c + egui::vec2(-4.0, -2.0),
                c + egui::vec2(0.0, 2.0),
                c + egui::vec2(4.0, -2.0),
            ],
            egui::Stroke::new(1.5, color),
        ));
    }
}

/// Bring `tab` to the front of its panel (if open).
pub fn focus_tab(dock: &mut DockState<Tab>, tab: Tab) {
    if let Some(p) = dock.find_tab(&tab) {
        let _ = dock.set_active_tab(p);
    }
}

/// Give a layout without the preview monitor one, below the composition
/// monitor (layouts saved before it existed get it this way).
pub fn add_preview(dock: &mut DockState<Tab>) {
    if is_open(dock, Tab::Preview) {
        return;
    }
    match dock.find_tab(&Tab::Monitor) {
        Some(p) if p.surface == egui_dock::SurfaceIndex::main() => {
            dock.main_surface_mut().split_below(p.node, 0.5, vec![Tab::Preview]);
        }
        _ if dock.main_surface().is_empty() => *dock = DockState::new(vec![Tab::Preview]),
        _ => dock.push_to_first_leaf(Tab::Preview),
    }
}

fn preset_base(name: &str) -> DockState<Tab> {
    use Tab::*;
    let root = NodeIndex::root();
    match name {
        "Big Monitor" => {
            let mut d = DockState::new(vec![Clips]);
            let t = d.main_surface_mut();
            let [left, _] = t.split_right(root, 0.58, vec![Monitor]);
            t.split_below(left, 0.5, vec![Composition, Layer, Group, Clip]);
            d
        }
        "Panels On Top" => {
            let mut d = DockState::new(vec![Composition, Layer, Group]);
            let t = d.main_surface_mut();
            let [top, _] = t.split_below(root, 0.5, vec![Clips]);
            let [_, right] = t.split_right(top, 0.36, vec![Clip]);
            t.split_right(right, 0.5, vec![Monitor]);
            d
        }
        "Panels Left & Right" => {
            let mut d = DockState::new(vec![Composition, Layer, Group]);
            let t = d.main_surface_mut();
            let [_, centre] = t.split_right(root, 0.24, vec![Clips]);
            let [centre, _] = t.split_right(centre, 0.68, vec![Clip]);
            t.split_below(centre, 0.55, vec![Monitor]);
            d
        }
        "Monitor Top Left" => {
            let mut d = DockState::new(vec![Monitor]);
            let t = d.main_surface_mut();
            let [mon, clips] = t.split_right(root, 0.32, vec![Clips]);
            t.split_below(mon, 0.42, vec![Composition, Layer, Group]);
            t.split_below(clips, 0.5, vec![Clip]);
            d
        }
        // Main screen: grid on top; monitor, composition/layer and clip below.
        _ => {
            let mut d = DockState::new(vec![Clips]);
            let t = d.main_surface_mut();
            let [_, bottom] = t.split_below(root, 0.42, vec![Monitor]);
            let [_, rest] = t.split_right(bottom, 0.26, vec![Composition, Layer, Group]);
            t.split_right(rest, 0.5, vec![Clip]);
            d
        }
    }
}

/// Is `tab` open anywhere (main area or an undocked window)?
pub fn is_open(dock: &DockState<Tab>, tab: Tab) -> bool {
    dock.find_tab(&tab).is_some()
}

/// Is `tab` open and in front (the active tab of its panel group, and the
/// group not collapsed)? Open tabs behind another tab don't count.
pub fn is_visible(dock: &DockState<Tab>, tab: Tab) -> bool {
    let Some(p) = dock.find_tab(&tab) else { return false };
    let node = &dock[p.surface][p.node];
    !node.is_collapsed() && node.get_leaf().is_some_and(|l| l.active == p.tab)
}

/// View menu: close `tab` when it's in front, bring it to the front when it's
/// open behind another tab, else open it.
pub fn show_or_hide_tab(dock: &mut DockState<Tab>, tab: Tab) {
    if is_open(dock, tab) && !is_visible(dock, tab) {
        focus_tab(dock, tab);
    } else {
        toggle_tab(dock, tab);
    }
}

/// Close `tab` if open, else add it to the focused (or first) panel group.
pub fn toggle_tab(dock: &mut DockState<Tab>, tab: Tab) {
    if let Some(path) = dock.find_tab(&tab) {
        dock.remove_tab(path);
    } else if tab == Tab::Smpte
        && let Some(p) = dock.find_tab(&Tab::ExternalFft)
    {
        // A small readout: with the browsers (bottom right), not over the grid.
        dock[p.surface][p.node].append_tab(tab);
        focus_tab(dock, tab);
    } else if dock.main_surface().is_empty() {
        *dock = DockState::new(vec![tab]);
    } else if dock.focused_leaf().is_some() {
        dock.push_to_focused_leaf(tab);
    } else {
        dock.push_to_first_leaf(tab);
    }
}

// ------------------------------------------------------------- files

pub(crate) fn config_dir() -> PathBuf {
    crate::app_dirs()
        .map(|d| d.config_dir().to_owned())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn layouts_dir() -> PathBuf {
    config_dir().join("layouts")
}

fn current_path() -> PathBuf {
    config_dir().join("layout.json")
}

/// File-name-safe version of a layout name.
fn file_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_&".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Layout as JSON. Node rects that were never laid out are infinite, which
/// JSON can't hold (they'd come back as `null`); they are zeroed in the copy,
/// egui_dock recomputes them every frame anyway.
pub fn to_json(dock: &DockState<Tab>, view: ViewOptions) -> String {
    #[derive(Serialize)]
    struct Ref<'a> {
        dock: &'a DockState<Tab>,
        view: ViewOptions,
    }
    let finite = |r: egui::Rect| if r.is_finite() { r } else { egui::Rect::ZERO };
    let mut dock = dock.clone();
    for (_, node) in dock.iter_all_nodes_mut() {
        if let Some(r) = node.rect() {
            node.set_rect(finite(r));
        }
        if let Some(leaf) = node.get_leaf_mut() {
            leaf.viewport = finite(leaf.viewport);
        }
    }
    serde_json::to_string_pretty(&Ref { dock: &dock, view }).unwrap_or_default()
}

fn read(path: &std::path::Path) -> anyhow::Result<LayoutFile> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn write(path: &std::path::Path, json: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// The layout in use when the app last ran, if any.
pub fn load_current() -> Option<LayoutFile> {
    read(&current_path())
        .map_err(|e| {
            if current_path().exists() {
                log::warn!("layout.json: {e}");
            }
        })
        .ok()
}

pub fn save_current(json: &str) {
    if let Err(e) = write(&current_path(), json) {
        log::warn!("saving layout.json: {e}");
    }
}

/// Names of saved layouts, sorted.
pub fn saved_names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(layouts_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "json" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    v.sort();
    v
}

pub fn save_named(name: &str, json: &str) -> anyhow::Result<String> {
    let stem = file_stem(name);
    anyhow::ensure!(!stem.is_empty(), "empty layout name");
    write(&layouts_dir().join(format!("{stem}.json")), json)?;
    Ok(stem)
}

pub fn load_named(name: &str) -> anyhow::Result<LayoutFile> {
    read(&layouts_dir().join(format!("{}.json", file_stem(name))))
}

pub fn delete_named(name: &str) -> anyhow::Result<()> {
    Ok(std::fs::remove_file(
        layouts_dir().join(format!("{}.json", file_stem(name))),
    )?)
}

// ------------------------------------------------------------- viewer

/// Shows each tab's panel from the app.
pub struct Viewer<'a> {
    pub app: &'a mut LavmfxApp,
    /// Tabs per panel (for the tab menu) and the tab picked there.
    leaves: Vec<(egui_dock::NodePath, Vec<Tab>, usize)>,
    picked: Option<Tab>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::unique(("lavmfx-tab", *tab))
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        let rect = ui.clip_rect();
        match tab {
            Tab::Clips => self.app.clip_grid(ui),
            Tab::Monitor => self.app.preview(ui),
            Tab::Preview => self.app.preview_monitor(ui),
            Tab::Composition => self.app.composition_panel(ui),
            Tab::Layer => self.app.layer_panel(ui),
            Tab::Group => self.app.group_tab(ui),
            Tab::Effects => self.app.effects_browser(ui),
            Tab::ExternalFft => self.app.external_fft_panel(ui),
            Tab::Sources => self.app.sources_browser(ui),
            Tab::Slices => self.app.slices_tab(ui),
            Tab::Smpte => self.app.smpte_panel(ui),
            Tab::Render => self.app.render_queue_panel(ui),
            Tab::Clip => self.app.clip_panel(ui),
        }
        // Effects dropped anywhere on a panel join that panel's chain.
        if let Some(t) = self.app.panel_fx_target(*tab) {
            super::effects_ui::panel_drop(ui, &mut self.app.comp, t, rect);
        }
    }

    /// The tab-bar button: every tab of this panel (the ones that don't fit
    /// the bar are hidden there); a click brings it to the front.
    fn add_popup(&mut self, ui: &mut Ui, path: egui_dock::NodePath) {
        let Some((_, tabs, active)) = self.leaves.iter().find(|(p, ..)| *p == path) else {
            return;
        };
        ui.set_min_width(140.0);
        for (i, t) in tabs.iter().enumerate() {
            let text = egui::RichText::new(t.title());
            let text = if i == *active { text.strong() } else { text };
            if ui.add(egui::Button::selectable(i == *active, text)).clicked() {
                self.picked = Some(*t);
                ui.close();
            }
        }
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            // The grid scrolls itself; the monitor fits the panel.
            Tab::Clips | Tab::Monitor | Tab::Preview | Tab::Effects | Tab::Sources | Tab::Slices => [false, false],
            _ => [false, true],
        }
    }
}

/// Draw the dock filling `ui`.
pub fn show(app: &mut LavmfxApp, ui: &mut Ui) {
    let mut dock = std::mem::replace(&mut app.dock, DockState::new(Vec::new()));
    let style = super::theme::dock_style(ui);
    let leaves = dock
        .iter_leaves()
        .map(|(p, l)| (p, l.tabs.clone(), l.active.0))
        .collect();
    let mut viewer = Viewer {
        app,
        leaves,
        picked: None,
    };
    let bar_h = style.tab_bar.height;
    egui_dock::DockArea::new(&mut dock)
        .id(egui::Id::unique("lavmfx-dock"))
        .style(style)
        .show_add_buttons(true)
        .show_add_popup(true)
        .show_leaf_collapse_buttons(false)
        .show_leaf_close_all_buttons(false)
        .show_inside(ui, &mut viewer);
    let picked = viewer.picked.take();
    let app = viewer.app;
    tab_menu_chevrons(ui, &dock, bar_h);
    app.dock = dock;
    if let Some(t) = picked {
        focus_tab(&mut app.dock, t);
    }
    if let Some(t) = app.focus_tab.take() {
        focus_tab(&mut app.dock, t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_hold_every_tab_once_and_round_trip() {
        for name in PRESETS {
            let d = preset(name);
            for t in Tab::ALL {
                // SMPTE starts hidden (View › SMPTE opens it); the
                // Render Queue opens with the first render.
                let want = usize::from(t != Tab::Smpte && t != Tab::Render);
                assert_eq!(
                    d.iter_all_tabs().filter(|(_, x)| **x == t).count(),
                    want,
                    "{name}: {t:?}"
                );
            }
            let json = to_json(&d, ViewOptions::default());
            let back: LayoutFile = serde_json::from_str(&json).unwrap();
            assert_eq!(to_json(&back.dock, back.view), json);
        }
    }

    #[test]
    fn smpte_opens_beside_external_fft() {
        for name in PRESETS {
            let mut d = preset(name);
            toggle_tab(&mut d, Tab::Smpte);
            let (a, b) = (d.find_tab(&Tab::Smpte).unwrap(), d.find_tab(&Tab::ExternalFft).unwrap());
            assert_eq!((a.surface, a.node), (b.surface, b.node), "{name}");
            toggle_tab(&mut d, Tab::Smpte);
            assert!(!is_open(&d, Tab::Smpte));
        }
    }

    #[test]
    fn toggling_tabs() {
        let mut d = preset("Default");
        toggle_tab(&mut d, Tab::Clip);
        assert!(!is_open(&d, Tab::Clip));
        toggle_tab(&mut d, Tab::Clip);
        assert!(is_open(&d, Tab::Clip));
        let mut empty = DockState::new(Vec::new());
        toggle_tab(&mut empty, Tab::Monitor);
        assert!(is_open(&empty, Tab::Monitor));
    }

    #[test]
    fn view_menu_ticks_only_tabs_in_front() {
        let mut d = preset("Default");
        // Sources is added behind another tab of its group: open, not visible.
        assert!(is_open(&d, Tab::Sources) && !is_visible(&d, Tab::Sources));
        let p = d.find_tab(&Tab::Sources).unwrap();
        let leaf = d[p.surface][p.node].get_leaf().unwrap();
        let front = leaf.tabs()[leaf.active.0];
        assert!(is_visible(&d, front));
        show_or_hide_tab(&mut d, Tab::Sources);
        assert!(is_visible(&d, Tab::Sources) && !is_visible(&d, front));
        show_or_hide_tab(&mut d, Tab::Sources);
        assert!(!is_open(&d, Tab::Sources));
        show_or_hide_tab(&mut d, Tab::Sources);
        assert!(is_visible(&d, Tab::Sources));
    }

    #[test]
    fn layout_names_are_file_safe() {
        assert_eq!(file_stem("My / Show: 2"), "My _ Show_ 2");
        assert_eq!(file_stem("Panels Left & Right"), "Panels Left & Right");
    }
}
