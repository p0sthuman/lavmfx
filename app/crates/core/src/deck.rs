//! Decks: each deck has its own columns and clips; layers are shared.
//!
//! The active deck's content lives in `Composition::columns` and
//! `layers[].clips` (what the REST API shows); inactive decks keep theirs in
//! [`Deck::stash`]. Switching swaps the two.

use crate::model::*;
use crate::param::BoolParam;

/// Columns a deck gets when first opened.
pub const DEFAULT_COLUMNS: usize = 8;

impl Composition {
    /// Index of the active deck (the one marked `selected`; 0 if none is).
    pub fn active_deck(&self) -> usize {
        self.decks
            .iter()
            .position(|d| d.selected.as_ref().is_some_and(|s| s.value))
            .unwrap_or(0)
    }

    /// Add an empty deck at the end; returns its index. Does not switch to it.
    pub fn add_deck(&mut self, name: &str) -> usize {
        let deck = Deck::new(&mut self.ids, name);
        self.decks.push(deck);
        self.decks.len() - 1
    }

    /// Remove deck `i` with its columns and clips. If it is the active deck
    /// the next one (or the previous, for the last) becomes active. The last
    /// deck stays. Returns the new active deck index.
    pub fn remove_deck(&mut self, i: usize) -> Option<usize> {
        if i >= self.decks.len() || self.decks.len() <= 1 {
            return None;
        }
        let active = self.active_deck();
        if i == active {
            let next = if i + 1 < self.decks.len() { i + 1 } else { i - 1 };
            self.select_deck(next);
        }
        self.decks.remove(i);
        Some(self.active_deck())
    }

    /// Make deck `i` active. Returns false if `i` is out of range.
    pub fn select_deck(&mut self, i: usize) -> bool {
        let cur = self.active_deck();
        if i >= self.decks.len() {
            return false;
        }
        if i == cur {
            return true;
        }
        let out = DeckContent {
            columns: std::mem::take(&mut self.columns),
            clips: self.layers.iter_mut().map(|l| std::mem::take(&mut l.clips)).collect(),
        };
        if let Some(d) = self.decks.get_mut(cur) {
            d.stash = Some(out);
        }
        let incoming = self.decks[i].stash.take();
        let DeckContent { columns, mut clips } = incoming.unwrap_or_default();
        self.columns = columns;
        if self.columns.is_empty() {
            for n in 1..=DEFAULT_COLUMNS {
                self.columns.push(Column::new(&mut self.ids, &format!("Column {n}")));
            }
        }
        // Layers added while this deck was inactive get empty slots.
        clips.resize_with(self.layers.len(), Vec::new);
        let n = self.columns.len();
        for (layer, mut c) in self.layers.iter_mut().zip(clips) {
            c.truncate(n);
            while c.len() < n {
                c.push(Clip::empty(&mut self.ids));
            }
            layer.clips = c;
        }
        for (k, d) in self.decks.iter_mut().enumerate() {
            match &mut d.selected {
                Some(s) => s.value = k == i,
                None => d.selected = Some(BoolParam::new(self.ids.alloc(), k == i)),
            }
        }
        true
    }

    /// Clip at (deck, layer, column), whether or not the deck is active.
    pub fn clip(&self, deck: usize, layer: usize, col: usize) -> Option<&Clip> {
        if deck == self.active_deck() {
            self.layers.get(layer)?.clips.get(col)
        } else {
            self.decks.get(deck)?.stash.as_ref()?.clips.get(layer)?.get(col)
        }
    }

    pub fn clip_mut(&mut self, deck: usize, layer: usize, col: usize) -> Option<&mut Clip> {
        if deck == self.active_deck() {
            self.layers.get_mut(layer)?.clips.get_mut(col)
        } else {
            self.decks
                .get_mut(deck)?
                .stash
                .as_mut()?
                .clips
                .get_mut(layer)?
                .get_mut(col)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decks_keep_their_own_clips() {
        let mut c = Composition::new("T", 2, 3);
        c.layers[1].clips[2].name.value = "deck1 clip".into();
        let d2 = c.add_deck("Deck 2");
        assert_eq!(c.active_deck(), 0);
        assert!(c.select_deck(d2));
        assert_eq!(c.active_deck(), 1);
        assert_eq!(c.columns.len(), DEFAULT_COLUMNS);
        assert!(
            c.layers
                .iter()
                .all(|l| l.clips.len() == DEFAULT_COLUMNS && l.clips.iter().all(Clip::is_empty))
        );
        assert_eq!(c.clip(0, 1, 2).unwrap().name.value, "deck1 clip");
        c.layers[0].clips[0].name.value = "deck2 clip".into();

        c.add_layer(); // only deck 2 knows about it so far
        assert!(c.select_deck(0));
        assert_eq!(c.columns.len(), 3);
        assert_eq!(c.layers[1].clips[2].name.value, "deck1 clip");
        assert_eq!(c.layers[2].clips.len(), 3, "new layer padded");
        assert_eq!(c.clip(1, 0, 0).unwrap().name.value, "deck2 clip");
        c.clip_mut(1, 0, 0).unwrap().name.value = "edited".into();
        assert!(!c.select_deck(9));

        // Survives a save/load round trip.
        let back: Composition = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let mut back = back;
        back.select_deck(1);
        assert_eq!(back.layers[0].clips[0].name.value, "edited");
    }

    #[test]
    fn remove_deck_keeps_the_others() {
        let mut c = Composition::new("t", 1, 2);
        let d2 = c.add_deck("Deck 2");
        let d3 = c.add_deck("Deck 3");
        c.select_deck(d2);
        c.layers[0].clips[0].name.value = "two".into();
        c.select_deck(d3);
        c.layers[0].clips[0].name.value = "three".into();
        // Removing the active last deck activates the one before it.
        assert_eq!(c.remove_deck(d3), Some(1));
        assert_eq!(c.layers[0].clips[0].name.value, "two");
        assert_eq!(c.remove_deck(0), Some(0));
        assert_eq!(c.decks[0].name.value, "Deck 2");
        assert_eq!(c.layers[0].clips[0].name.value, "two");
        assert_eq!(c.remove_deck(0), None, "last deck stays");
    }
}
