//! Composition files.
//!
//! * Native format: pretty JSON (`.lavmfx`), a direct serialisation of
//!   [`lavmfx_core::Composition`] with a version header for migrations.
//!   JSON (not RON) because the model uses serde `flatten`/untagged enums to
//!   match the REST schema, which RON cannot round-trip.
//! * `.avc` import (XML): [`avc`], media paths via [`paths`].

use std::path::Path;

use lavmfx_core::Composition;
use serde::{Deserialize, Serialize};

pub mod avc;
pub mod fixtures;
pub mod paths;
pub mod screens;

pub const EXTENSION: &str = "lavmfx";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("file format version {found} is newer than supported ({FORMAT_VERSION})")]
    TooNew { found: u32 },
    #[error("not a readable composition: {0}")]
    Avc(String),
}

pub type Result<T> = std::result::Result<T, ProjectError>;

#[derive(Serialize, Deserialize)]
struct FileV1 {
    version: u32,
    composition: Composition,
}

pub fn to_string(comp: &Composition) -> Result<String> {
    let file = FileV1 {
        version: FORMAT_VERSION,
        composition: comp.clone(),
    };
    Ok(serde_json::to_string_pretty(&file)?)
}

pub fn from_str(s: &str) -> Result<Composition> {
    let file: FileV1 = serde_json::from_str(s)?;
    if file.version > FORMAT_VERSION {
        return Err(ProjectError::TooNew { found: file.version });
    }
    let mut comp = file.composition;
    comp.align_options();
    comp.ensure_master_ids();
    Ok(comp)
}

/// Save atomically (write temp file, then rename).
pub fn save(comp: &Composition, path: &Path) -> Result<()> {
    let tmp = path.with_extension("lavmfx.tmp");
    std::fs::write(&tmp, to_string(comp)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Composition> {
    from_str(&std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_without_layer_masters_load() {
        // Saved on 2026-11-25 while layer / group masters were removed.
        let mut c = Composition::new("File", 2, 1);
        c.new_group(&[1], "G");
        let mut j: serde_json::Value = serde_json::from_str(&to_string(&c).unwrap()).unwrap();
        for k in ["layers", "layergroups"] {
            for o in j["composition"][k].as_array_mut().unwrap() {
                o.as_object_mut().unwrap().remove("master");
            }
        }
        let back = from_str(&j.to_string()).unwrap();
        let ids: Vec<_> = back
            .layers
            .iter()
            .map(|l| l.master.id)
            .chain(back.layergroups.iter().map(|g| g.master.id))
            .collect();
        assert!(ids.iter().all(|&i| i != 0), "{ids:?}");
        assert!(back.layers.iter().all(|l| l.master.value == 1.0));
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn file_roundtrip() {
        let c = Composition::new("File", 3, 4);
        let s = to_string(&c).unwrap();
        assert_eq!(from_str(&s).unwrap(), c);
    }

    #[test]
    fn rejects_newer_version() {
        let c = Composition::new("v", 1, 1);
        let s = to_string(&c).unwrap().replacen("\"version\": 1", "\"version\": 99", 1);
        assert!(matches!(from_str(&s), Err(ProjectError::TooNew { found: 99 })));
    }
}
