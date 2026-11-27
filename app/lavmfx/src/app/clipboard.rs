//! Copy / cut / paste of clips in the grid: Ctrl+C / Ctrl+X / Ctrl+V and the
//! slot right-click menu. Paste overwrites, like dropping onto a slot.
//!
//! egui turns Ctrl+C/X/V into `Event::Copy/Cut/Paste`, and only emits
//! `Paste` when the system clipboard holds something. Copy therefore also
//! puts the clips' file paths on the system clipboard as text.

use eframe::egui;

use super::LavmfxApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotAction {
    Copy,
    Cut,
    Paste,
    Eject,
}

impl LavmfxApp {
    /// Handle clipboard keys. Call only when no text field has focus.
    pub(super) fn clipboard_shortcuts(&mut self, ctx: &egui::Context) {
        let (copy, cut, paste) = ctx.input(|i| {
            i.events.iter().fold((false, false, false), |(c, x, v), e| match e {
                egui::Event::Copy => (true, x, v),
                egui::Event::Cut => (c, true, v),
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => (c, x, true),
                _ => (c, x, v),
            })
        });
        if cut {
            self.copy_selection(ctx, true);
        } else if copy {
            self.copy_selection(ctx, false);
        }
        if paste {
            self.paste_clips();
        }
    }

    /// Copy (or cut) the selected clips.
    pub(super) fn copy_selection(&mut self, ctx: &egui::Context, cut: bool) {
        let sel: Vec<_> = self.selection.iter().copied().collect();
        if !self.clipboard.copy(&self.comp, &sel) {
            self.status = "Nothing to copy: select clips by their name strip".into();
            return;
        }
        let text = self.clipboard.paths_text();
        ctx.copy_text(if text.is_empty() { "lavmfx clips".into() } else { text });
        let n = self.clipboard.len();
        if cut {
            self.eject_selection();
        }
        self.status = format!(
            "{} {n} clip{}",
            if cut { "Cut" } else { "Copied" },
            if n == 1 { "" } else { "s" }
        );
    }

    /// Paste at the focused slot (one copied clip: into every selected slot).
    pub(super) fn paste_clips(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        let Some(anchor) = self.selected_clip else {
            self.status = "Select a slot to paste into".into();
            return;
        };
        let targets: Vec<_> = self.selection.iter().copied().collect();
        // Overwritten playing clips stop on the next tick (their id is gone).
        let written = self.clipboard.paste(&mut self.comp, anchor, &targets);
        let n = written.len();
        let skipped = self.clipboard.len().saturating_sub(n);
        if n > 0 {
            if !written.contains(&anchor) {
                self.selected_clip = written.first().copied();
            }
            self.selection = written.into_iter().collect();
        }
        self.status = format!(
            "Pasted {n} clip{}{}",
            if n == 1 { "" } else { "s" },
            if skipped > 0 && self.clipboard.len() > 1 {
                format!(" ({skipped} below layer 1 skipped)")
            } else {
                String::new()
            }
        );
    }

    /// Run a slot menu action. Copy/cut/paste act on the selection; a slot
    /// outside the selection becomes the selection first. Returns the slot
    /// to eject for `Eject` (handled by the grid's eject logic).
    pub(super) fn slot_menu_action(
        &mut self,
        slot: (usize, usize),
        action: SlotAction,
        ctx: &egui::Context,
    ) -> Option<(usize, usize)> {
        if !self.selection.contains(&slot) {
            self.select_only(slot);
        }
        match action {
            SlotAction::Copy => self.copy_selection(ctx, false),
            SlotAction::Cut => self.copy_selection(ctx, true),
            SlotAction::Paste => {
                self.selected_clip = Some(slot);
                self.paste_clips();
            }
            SlotAction::Eject => return Some(slot),
        }
        None
    }
}

/// Right-click menu of a slot.
pub(super) fn slot_menu(r: &egui::Response, filled: bool, can_paste: bool) -> Option<SlotAction> {
    let mut hit = None;
    r.context_menu(|ui| {
        let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, a: SlotAction| {
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                hit = Some(a);
                ui.close();
            }
        };
        item(ui, filled, "Copy    Ctrl+C", SlotAction::Copy);
        item(ui, filled, "Cut    Ctrl+X", SlotAction::Cut);
        item(ui, can_paste, "Paste    Ctrl+V", SlotAction::Paste);
        ui.separator();
        item(ui, filled, "Clear    Del", SlotAction::Eject);
    });
    hit
}
