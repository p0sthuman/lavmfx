//! Layer groups: a named run of layers with its own master, bypass and solo.
//!
//! Membership lives on the layer ([`Layer::group`]); groups are kept in
//! `Composition::layergroups`. Groups with no layers left are dropped by
//! [`Composition::prune_groups`].

use crate::id::Id;
use crate::model::*;

impl Composition {
    /// Index (into `layergroups`) of layer `li`'s group.
    pub fn group_of(&self, li: usize) -> Option<usize> {
        let id = self.layers.get(li)?.group?;
        self.layergroups.iter().position(|g| g.id == id)
    }

    /// Layers (indices, bottom first) of group `gi`.
    pub fn group_members(&self, gi: usize) -> Vec<usize> {
        let Some(id) = self.layergroups.get(gi).map(|g| g.id) else {
            return Vec::new();
        };
        (0..self.layers.len())
            .filter(|&l| self.layers[l].group == Some(id))
            .collect()
    }

    /// New group named `name` holding `layers`. Returns its index.
    pub fn new_group(&mut self, layers: &[usize], name: &str) -> usize {
        let g = LayerGroup::new(&mut self.ids, name);
        let id = g.id;
        self.layergroups.push(g);
        for &l in layers {
            if let Some(layer) = self.layers.get_mut(l) {
                layer.group = Some(id);
            }
        }
        self.prune_groups();
        self.layergroups.iter().position(|g| g.id == id).unwrap_or(0)
    }

    /// Put layer `li` into group `gi`.
    pub fn add_to_group(&mut self, li: usize, gi: usize) {
        let Some(id) = self.layergroups.get(gi).map(|g| g.id) else {
            return;
        };
        if let Some(l) = self.layers.get_mut(li) {
            l.group = Some(id);
        }
        self.prune_groups();
    }

    /// Take layer `li` out of its group.
    pub fn ungroup_layer(&mut self, li: usize) {
        if let Some(l) = self.layers.get_mut(li) {
            l.group = None;
        }
        self.prune_groups();
    }

    /// Dissolve group `gi`; its layers stay, ungrouped.
    pub fn remove_group(&mut self, gi: usize) {
        let Some(id) = self.layergroups.get(gi).map(|g| g.id) else {
            return;
        };
        for l in &mut self.layers {
            if l.group == Some(id) {
                l.group = None;
            }
        }
        self.prune_groups();
    }

    /// Drop groups without layers and memberships of groups that don't exist.
    pub fn prune_groups(&mut self) {
        let used: Vec<Id> = self.layers.iter().filter_map(|l| l.group).collect();
        self.layergroups.retain(|g| used.contains(&g.id));
        let ids: Vec<Id> = self.layergroups.iter().map(|g| g.id).collect();
        for l in &mut self.layers {
            if l.group.is_some_and(|g| !ids.contains(&g)) {
                l.group = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lifecycle() {
        let mut c = Composition::new("t", 4, 1);
        let g = c.new_group(&[1, 2], "Lines");
        assert_eq!(c.layergroups[g].name.value, "Lines");
        assert_eq!(c.group_members(g), [1, 2]);
        assert_eq!(c.group_of(2), Some(g));
        assert_eq!(c.group_of(0), None);
        c.add_to_group(3, g);
        assert_eq!(c.group_members(g), [1, 2, 3]);
        c.ungroup_layer(1);
        assert_eq!(c.group_members(g), [2, 3]);
        // Removing its last layers drops the group.
        assert!(c.remove_layer(3));
        assert!(c.remove_layer(2));
        c.prune_groups();
        assert!(c.layergroups.is_empty());
        let g = c.new_group(&[0], "A");
        c.remove_group(g);
        assert!(c.layergroups.is_empty() && c.layers.iter().all(|l| l.group.is_none()));
    }

    #[test]
    fn groups_survive_json() {
        let mut c = Composition::new("t", 2, 1);
        let g = c.new_group(&[0, 1], "G");
        c.layergroups[g].folded = true;
        let back: Composition = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.group_members(0), [0, 1]);
        assert!(back.layergroups[0].folded);
    }
}
