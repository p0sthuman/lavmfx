//! Clip grid edits (drag and drop of clips in the active deck).

use std::collections::BTreeSet;

use crate::model::*;

impl Composition {
    /// Put the clips at `sources` onto `targets` (pairwise, active deck).
    ///
    /// - `insert: None` — overwrite whatever is at the targets.
    /// - `insert: Some(k)` — first, in every target layer, shift the slots
    ///   from column `k` on right by the group's width (targets span
    ///   `k..k+width`). Trailing empty slots absorb the shift; columns are
    ///   added only when a layer runs out of them.
    ///
    /// Move (`copy = false`) leaves the sources empty; copy duplicates the
    /// clips with fresh ids. The grid stays rectangular.
    pub fn place_clips(
        &mut self,
        sources: &[(usize, usize)],
        targets: &[(usize, usize)],
        insert: Option<usize>,
        copy: bool,
    ) {
        assert_eq!(sources.len(), targets.len());
        let ids = &mut self.ids;
        let layers = &mut self.layers;
        let taken: Vec<Clip> = sources
            .iter()
            .map(|&(l, c)| {
                let slot = &mut layers[l].clips[c];
                if copy {
                    slot.duplicate(ids)
                } else {
                    std::mem::replace(slot, Clip::empty(ids))
                }
            })
            .collect();
        if let Some(k) = insert {
            let width = targets.iter().map(|t| t.1 + 1).max().unwrap_or(k).saturating_sub(k);
            let rows: BTreeSet<usize> = targets.iter().map(|t| t.0).collect();
            let cols = self.columns.len();
            for l in rows {
                let clips = &mut layers[l].clips;
                let at = k.min(clips.len());
                clips.splice(at..at, (0..width).map(|_| Clip::empty(ids)));
                while clips.len() > cols && clips.last().is_some_and(Clip::is_empty) {
                    clips.pop();
                }
            }
        }
        for (&(l, c), clip) in targets.iter().zip(taken) {
            let clips = &mut layers[l].clips;
            while clips.len() <= c {
                clips.push(Clip::empty(ids));
            }
            clips[c] = clip;
        }
        // Rectangular: columns = longest layer, shorter layers padded.
        let need = layers.iter().map(|l| l.clips.len()).max().unwrap_or(0);
        while self.columns.len() < need {
            let n = self.columns.len() + 1;
            self.columns.push(Column::new(ids, &format!("Column {n}")));
        }
        let cols = self.columns.len();
        for l in layers.iter_mut() {
            while l.clips.len() < cols {
                l.clips.push(Clip::empty(ids));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2 layers × 4 columns; layer 0 = "a b c _", layer 1 = "x _ _ _".
    fn grid() -> Composition {
        let mut c = Composition::new("T", 2, 4);
        for (i, n) in ["a", "b", "c"].iter().enumerate() {
            fill(&mut c, 0, i, n);
        }
        fill(&mut c, 1, 0, "x");
        c
    }

    fn fill(c: &mut Composition, l: usize, col: usize, name: &str) {
        let clip = &mut c.layers[l].clips[col];
        clip.name.value = name.into();
        clip.transport = Some(Transport::Timeline(TransportTimeline::new(&mut c.ids, 1.0)));
        clip.video = Some(VideoTrackClip::new(
            &mut c.ids,
            VideoFileInfo {
                path: name.into(),
                exists: true,
                duration: String::new(),
                duration_ms: 1000.0,
                framerate: FrameRate { num: 30, denom: 1 },
                width: 1,
                height: 1,
            },
        ));
    }

    fn row(c: &Composition, l: usize) -> String {
        c.layers[l]
            .clips
            .iter()
            .map(|c| if c.is_empty() { "_" } else { c.name.value.as_str() })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn rectangular(c: &Composition) -> bool {
        c.layers.iter().all(|l| l.clips.len() == c.columns.len())
    }

    #[test]
    fn move_overwrites() {
        let mut c = grid();
        c.place_clips(&[(0, 0)], &[(0, 2)], None, false);
        assert_eq!(row(&c, 0), "_ b a _");
    }

    #[test]
    fn copy_overwrites_with_fresh_ids() {
        let mut c = grid();
        c.place_clips(&[(0, 0)], &[(1, 0)], None, true);
        assert_eq!(row(&c, 0), "a b c _");
        assert_eq!(row(&c, 1), "a _ _ _");
        assert_ne!(c.layers[0].clips[0].id, c.layers[1].clips[0].id);
    }

    #[test]
    fn insert_shifts_right_using_trailing_space() {
        let mut c = grid();
        // Move "c" between "a" and "b": b shifts right, c's old slot is a hole.
        c.place_clips(&[(0, 2)], &[(0, 1)], Some(1), false);
        assert_eq!(row(&c, 0), "a c b _");
        assert_eq!(c.columns.len(), 4);
        assert_eq!(row(&c, 1), "x _ _ _", "other layers untouched");
    }

    #[test]
    fn insert_adds_columns_when_full() {
        let mut c = grid();
        fill(&mut c, 0, 3, "d");
        // Insert a copy of x before a; layer 0 has no trailing space left.
        c.place_clips(&[(1, 0)], &[(0, 0)], Some(0), true);
        assert_eq!(row(&c, 0), "x a b c d");
        assert_eq!(c.columns.len(), 5);
        assert!(rectangular(&c));
        assert_eq!(row(&c, 1), "x _ _ _ _");
    }

    #[test]
    fn insert_group_spans_its_width() {
        let mut c = grid();
        // Move a and c (columns 0 and 2, width 3) from layer 0 to between x and the rest of layer 1.
        c.place_clips(&[(0, 0), (0, 2)], &[(1, 1), (1, 3)], Some(1), false);
        assert_eq!(row(&c, 1), "x a _ c");
        assert_eq!(row(&c, 0), "_ b _ _");
        assert!(rectangular(&c));
    }

    #[test]
    fn overwrite_past_the_end_adds_columns() {
        let mut c = grid();
        c.place_clips(&[(0, 0)], &[(1, 6)], None, false);
        assert_eq!(c.columns.len(), 7);
        assert!(rectangular(&c));
        assert_eq!(c.layers[1].clips[6].name.value, "a");
    }
}
