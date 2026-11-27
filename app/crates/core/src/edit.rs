//! Structural edits: insert/remove layers and columns, clear clips.
//!
//! Layers are shared by all decks; columns belong to a deck. Inactive decks
//! keep their clips in [`Deck::stash`] indexed by layer, so layer edits are
//! applied there too.

use crate::model::*;

impl Composition {
    /// Insert an empty layer at index `at` (0 = bottom; clamped). Returns its index.
    pub fn insert_layer(&mut self, at: usize) -> usize {
        let at = at.min(self.layers.len());
        let n = self.layers.len() + 1;
        let layer = Layer::new(&mut self.ids, &format!("Layer {n}"), self.columns.len());
        self.layers.insert(at, layer);
        for d in &mut self.decks {
            if let Some(s) = &mut d.stash
                && at <= s.clips.len()
            {
                s.clips.insert(at, Vec::new()); // filled with empty slots on deck switch
            }
        }
        at
    }

    /// Remove layer `li` (with its clips in every deck). The last layer stays.
    pub fn remove_layer(&mut self, li: usize) -> bool {
        if li >= self.layers.len() || self.layers.len() <= 1 {
            return false;
        }
        self.layers.remove(li);
        for d in &mut self.decks {
            if let Some(s) = &mut d.stash
                && li < s.clips.len()
            {
                s.clips.remove(li);
            }
        }
        self.prune_groups();
        true
    }

    /// Move layer `from` so it ends up at index `to` (with its clips in
    /// every deck). Returns false when out of range.
    pub fn move_layer(&mut self, from: usize, to: usize) -> bool {
        let n = self.layers.len();
        if from >= n || to >= n {
            return false;
        }
        let l = self.layers.remove(from);
        self.layers.insert(to, l);
        for d in &mut self.decks {
            if let Some(s) = &mut d.stash {
                s.clips.resize_with(n, Vec::new);
                let c = s.clips.remove(from);
                s.clips.insert(to, c);
            }
        }
        true
    }

    /// Empty every slot of layer `li` in the active deck.
    pub fn clear_layer_clips(&mut self, li: usize) {
        if let Some(l) = self.layers.get_mut(li) {
            for c in &mut l.clips {
                *c = Clip::empty(&mut self.ids);
            }
        }
    }

    /// Insert an empty column at `at` in the active deck (clamped). Returns its index.
    pub fn insert_column(&mut self, at: usize) -> usize {
        let at = at.min(self.columns.len());
        let n = self.columns.len() + 1;
        self.columns
            .insert(at, Column::new(&mut self.ids, &format!("Column {n}")));
        for l in &mut self.layers {
            let at = at.min(l.clips.len());
            l.clips.insert(at, Clip::empty(&mut self.ids));
        }
        at
    }

    /// Remove column `ci` of the active deck with its clips. The last column stays.
    pub fn remove_column(&mut self, ci: usize) -> bool {
        if ci >= self.columns.len() || self.columns.len() <= 1 {
            return false;
        }
        self.columns.remove(ci);
        for l in &mut self.layers {
            if ci < l.clips.len() {
                l.clips.remove(ci);
            }
        }
        true
    }

    /// Empty every slot of column `ci` in the active deck.
    pub fn clear_column_clips(&mut self, ci: usize) {
        for l in &mut self.layers {
            if let Some(c) = l.clips.get_mut(ci) {
                *c = Clip::empty(&mut self.ids);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(c: &Composition) -> Vec<String> {
        c.layers.iter().map(|l| l.name.value.clone()).collect()
    }

    #[test]
    fn layers_insert_and_remove_across_decks() {
        let mut c = Composition::new("t", 2, 3);
        c.layers[1].clips[0].name.value = "top".into();
        let d2 = c.add_deck("Deck 2");
        c.select_deck(d2);
        c.layers[1].clips[2].name.value = "deck2 top".into();
        c.select_deck(0);
        // Insert between layer 1 and 2: both decks' clips stay on their layer.
        assert_eq!(c.insert_layer(1), 1);
        assert_eq!(named(&c), ["Layer 1", "Layer 3", "Layer 2"]);
        assert_eq!(c.layers[2].clips[0].name.value, "top");
        assert!(c.layers[1].clips.iter().all(Clip::is_empty));
        c.select_deck(d2);
        assert_eq!(c.layers[2].clips[2].name.value, "deck2 top");
        assert_eq!(c.layers[1].clips.len(), c.columns.len());
        // Remove the new layer again from deck 2's view.
        assert!(c.remove_layer(1));
        c.select_deck(0);
        assert_eq!(c.layers[1].clips[0].name.value, "top");
        assert!(c.remove_layer(0));
        assert!(!c.remove_layer(0), "last layer stays");
    }

    #[test]
    fn move_layer_keeps_clips_in_all_decks() {
        let mut c = Composition::new("t", 3, 2);
        c.layers[0].clips[0].name.value = "a".into();
        let d2 = c.add_deck("Deck 2");
        c.select_deck(d2);
        c.layers[0].clips[1].name.value = "b".into();
        c.select_deck(0);
        assert!(c.move_layer(0, 2));
        assert_eq!(named(&c), ["Layer 2", "Layer 3", "Layer 1"]);
        assert_eq!(c.layers[2].clips[0].name.value, "a");
        c.select_deck(d2);
        assert_eq!(c.layers[2].clips[1].name.value, "b");
        assert!(c.move_layer(2, 0));
        assert_eq!(c.layers[0].clips[1].name.value, "b");
        assert!(!c.move_layer(3, 0));
    }

    #[test]
    fn columns_insert_remove_clear() {
        let mut c = Composition::new("t", 2, 2);
        c.layers[0].clips[1].name.value = "x".into();
        assert_eq!(c.insert_column(1), 1);
        assert_eq!(c.columns.len(), 3);
        assert_eq!(c.layers[0].clips[2].name.value, "x");
        assert!(c.layers.iter().all(|l| l.clips.len() == 3));
        assert!(c.remove_column(0));
        assert_eq!(c.layers[0].clips[1].name.value, "x");
        c.clear_column_clips(1);
        assert!(c.layers[0].clips[1].is_empty());
        c.layers[1].clips[0].name.value = "y".into();
        c.clear_layer_clips(1);
        assert!(c.layers[1].clips[0].is_empty());
    }
}
