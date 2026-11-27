//! Effect presets (the P button on an effect's name band): a
//! named snapshot of all of one effect's parameters — values and animations
//! (source, range, length, envelope). Stored app-wide in
//! `~/.config/lavmfx/effect-presets.json`, keyed by effect type, so a "Blur"
//! preset can be recalled on every Blur. Recall matches parameters by name;
//! ids of the target stay.

use std::cell::RefCell;
use std::collections::BTreeMap;

use lavmfx_core::{Param, ParameterCollection, VideoEffect};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub name: String,
    pub params: ParameterCollection,
    #[serde(default)]
    pub mixer: ParameterCollection,
}

type Store = BTreeMap<String, Vec<EffectPreset>>;

thread_local! {
    /// Loaded on first use (the UI runs on one thread).
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
}

fn path() -> std::path::PathBuf {
    super::dock::config_dir().join("effect-presets.json")
}

fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let store = s.get_or_insert_with(|| {
            std::fs::read_to_string(path())
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default()
        });
        f(store)
    })
}

fn persist(store: &Store) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(store).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

/// Presets of this effect type.
pub fn list(fx: &VideoEffect) -> Vec<EffectPreset> {
    with_store(|s| s.get(&fx.name).cloned().unwrap_or_default())
}

/// Save `fx`'s current settings under `name` (same name = replace).
/// Animated params keep their static value (`base`) as the value.
pub fn save(fx: &VideoEffect, name: &str) {
    let mut params = fx.params.clone();
    for p in params.values_mut() {
        if let Param::Range(r) = p
            && r.is_driven()
            && let Some(d) = &r.drive
        {
            r.value = d.base;
        }
    }
    let preset = EffectPreset {
        name: name.to_owned(),
        params,
        mixer: fx.mixer.clone(),
    };
    with_store(|s| {
        let v = s.entry(fx.name.clone()).or_default();
        match v.iter_mut().find(|x| x.name == preset.name) {
            Some(x) => *x = preset,
            None => v.push(preset),
        }
        persist(s);
    });
}

pub fn delete(fx: &VideoEffect, name: &str) {
    with_store(|s| {
        if let Some(v) = s.get_mut(&fx.name) {
            v.retain(|x| x.name != name);
            if v.is_empty() {
                s.remove(&fx.name);
            }
        }
        persist(s);
    });
}

/// Rename a saved preset (no-op if `new` is taken or empty).
pub fn rename(fx: &VideoEffect, old: &str, new: &str) {
    let new = new.trim();
    if new.is_empty() {
        return;
    }
    with_store(|s| {
        if let Some(v) = s.get_mut(&fx.name)
            && !v.iter().any(|x| x.name == new)
            && let Some(p) = v.iter_mut().find(|x| x.name == old)
        {
            p.name = new.to_owned();
        }
        persist(s);
    });
}

/// Copy values (not ids) from `src` into the same-named params of `dst`.
fn copy_values(src: &ParameterCollection, dst: &mut ParameterCollection) {
    for (k, from) in src {
        match (dst.get_mut(k), from) {
            (Some(Param::Range(t)), Param::Range(f)) => {
                t.drive = f.drive.clone();
                if let Some(d) = t.drive.as_mut() {
                    d.restart();
                }
                t.set(f.value);
            }
            (Some(Param::Choice(t)), Param::Choice(f)) => {
                t.select_value(&f.value);
            }
            (Some(Param::Bool(t)), Param::Bool(f)) => t.value = f.value,
            (Some(Param::Int(t)), Param::Int(f)) => t.value = f.value,
            (Some(Param::Color(t)), Param::Color(f)) => t.value = f.value.clone(),
            (Some(Param::String(t)), Param::String(f)) => t.value = f.value.clone(),
            (Some(Param::Text(t)), Param::Text(f)) => t.value = f.value.clone(),
            _ => {}
        }
    }
}

/// Recall a preset onto `fx` (animations restart from their beginning).
pub fn apply(preset: &EffectPreset, fx: &mut VideoEffect) {
    copy_values(&preset.params, &mut fx.params);
    copy_values(&preset.mixer, &mut fx.mixer);
}
