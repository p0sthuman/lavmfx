//! Clip clipboard: copy/cut selected clips, paste them elsewhere (any deck).
//!
//! Clips are stored as a pattern relative to the selection's top-left slot
//! as seen in the grid (highest layer, leftmost column). Pasting overwrites
//! the target slots, like dropping a drag onto a slot.

use crate::model::*;

#[derive(Debug, Clone, Default)]
pub struct ClipBoard {
    /// (layers down from the top, columns right of the left edge, clip).
    items: Vec<(usize, usize, Clip)>,
}

impl ClipBoard {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The clips' file paths, one per line (for the system clipboard).
    pub fn paths_text(&self) -> String {
        self.items
            .iter()
            .filter_map(|(.., c)| c.video_path())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Copy the non-empty clips at `slots` (active deck). Returns false (and
    /// keeps the old contents) if none of them holds a clip.
    pub fn copy(&mut self, comp: &Composition, slots: &[(usize, usize)]) -> bool {
        let filled: Vec<(usize, usize, &Clip)> = slots
            .iter()
            .filter_map(|&(l, c)| {
                comp.layers
                    .get(l)?
                    .clips
                    .get(c)
                    .filter(|c| !c.is_empty())
                    .map(|clip| (l, c, clip))
            })
            .collect();
        let (Some(top), Some(left)) = (filled.iter().map(|f| f.0).max(), filled.iter().map(|f| f.1).min()) else {
            return false;
        };
        self.items = filled
            .into_iter()
            .map(|(l, c, clip)| (top - l, c - left, clip.clone()))
            .collect();
        true
    }

    /// Paste into the active deck. With one clip on the clipboard it goes
    /// into every slot of `targets`; otherwise the pattern's top-left lands
    /// on `anchor`. Clips that would fall below layer 1 are skipped; columns
    /// are added on the right as needed. Pasted clips get fresh ids.
    /// Returns the slots written.
    pub fn paste(
        &self,
        comp: &mut Composition,
        anchor: (usize, usize),
        targets: &[(usize, usize)],
    ) -> Vec<(usize, usize)> {
        let placed: Vec<((usize, usize), &Clip)> = match self.items.as_slice() {
            [] => return Vec::new(),
            [(.., clip)] if targets.len() > 1 => targets.iter().map(|&t| (t, clip)).collect(),
            items => items
                .iter()
                .filter_map(|(dl, dc, clip)| anchor.0.checked_sub(*dl).map(|l| ((l, anchor.1 + dc), clip)))
                .collect(),
        };
        let placed: Vec<_> = placed
            .into_iter()
            .filter(|((l, _), _)| *l < comp.layers.len())
            .collect();
        let need = placed.iter().map(|((_, c), _)| c + 1).max().unwrap_or(0);
        while comp.columns.len() < need {
            comp.add_column();
        }
        let mut written = Vec::new();
        for ((l, c), clip) in placed {
            let dup = clip.duplicate(&mut comp.ids);
            comp.layers[l].clips[c] = dup;
            written.push((l, c));
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(c: &mut Composition, l: usize, col: usize, name: &str) {
        let info = VideoFileInfo {
            path: format!("/m/{name}.mov"),
            exists: true,
            duration: String::new(),
            duration_ms: 1000.0,
            framerate: FrameRate { num: 30, denom: 1 },
            width: 1,
            height: 1,
        };
        c.layers[l].clips[col].load_video(&mut c.ids, name, info);
    }

    fn row(c: &Composition, l: usize) -> String {
        c.layers[l]
            .clips
            .iter()
            .map(|c| if c.is_empty() { "_" } else { c.name.value.as_str() })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn copy_paste_keeps_pattern() {
        let mut c = Composition::new("T", 3, 4);
        named(&mut c, 2, 0, "a"); // top layer
        named(&mut c, 1, 1, "b");
        let mut cb = ClipBoard::default();
        assert!(cb.copy(&c, &[(2, 0), (1, 1), (0, 3)])); // (0,3) is empty: ignored
        assert_eq!(cb.len(), 2);
        assert_eq!(cb.paths_text(), "/m/a.mov\n/m/b.mov");
        // Top-left of the pattern onto layer 1 (index 1), column 2.
        let w = cb.paste(&mut c, (1, 2), &[(1, 2)]);
        assert_eq!(w, vec![(1, 2), (0, 3)]);
        assert_eq!(row(&c, 1), "_ b a _");
        assert_eq!(row(&c, 0), "_ _ _ b");
        assert_ne!(c.layers[1].clips[2].id, c.layers[2].clips[0].id, "fresh ids");
    }

    #[test]
    fn paste_skips_below_bottom_and_extends_columns() {
        let mut c = Composition::new("T", 2, 2);
        named(&mut c, 1, 0, "a");
        named(&mut c, 0, 1, "b");
        let mut cb = ClipBoard::default();
        cb.copy(&c, &[(1, 0), (0, 1)]);
        let w = cb.paste(&mut c, (0, 1), &[(0, 1)]); // b would land below layer 1
        assert_eq!(w, vec![(0, 1)]);
        assert_eq!(row(&c, 0), "_ a");
        let w = cb.paste(&mut c, (1, 3), &[]);
        assert_eq!(w, vec![(1, 3), (0, 4)]);
        assert_eq!(c.columns.len(), 5);
        assert!(c.layers.iter().all(|l| l.clips.len() == 5));
    }

    #[test]
    fn single_clip_fills_selection_and_empty_copy_keeps_old() {
        let mut c = Composition::new("T", 2, 3);
        named(&mut c, 0, 0, "a");
        let mut cb = ClipBoard::default();
        cb.copy(&c, &[(0, 0)]);
        assert!(!cb.copy(&c, &[(1, 2)]), "empty slots don't replace the clipboard");
        let w = cb.paste(&mut c, (1, 0), &[(1, 0), (1, 2), (0, 2)]);
        assert_eq!(w.len(), 3);
        assert_eq!(row(&c, 1), "a _ a");
        assert_eq!(row(&c, 0), "a _ a");
    }
}
