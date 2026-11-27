//! Fixture library: our personalities (`<name>.json`) and imported fixture
//! XML files (`Fixture Library/<name>.xml`, read only).
//!
//! The XML stores a fixture as `<Fixture fixtureName=…>` › `<Params>` ›
//! `<ParamFixturePixels>` (Width, Height, Color Space, Distribution, Gamma).
//! Its Color Space and Distribution values are indices / bit codes we can't
//! confirm without more samples: Color Space 1 is read as RGB (the
//! default), the others in the order
//! (rgb, rbg, bgr, brg, gbr, grb); Distribution is ignored (rows from the top
//! left).

use std::path::{Path, PathBuf};

use lavmfx_core::dmx::{Block, ColorSpace, Personality, PixelBlock};

use crate::{ProjectError, Result};

/// Read one fixture XML.
pub fn import_fixture_xml(text: &str) -> Result<Personality> {
    let doc = roxmltree::Document::parse(text).map_err(|e| ProjectError::Avc(format!("fixture XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Fixture" {
        return Err(ProjectError::Avc("not a fixture file (no <Fixture>)".into()));
    }
    let name = root
        .attribute("fixtureName")
        .or(root.attribute("name"))
        .unwrap_or("Fixture")
        .to_owned();
    let mut blocks = Vec::new();
    for px in root.descendants().filter(|n| n.has_tag_name("ParamFixturePixels")) {
        let val = |param: &str| {
            px.children()
                .find(|c| c.is_element() && c.attribute("name") == Some(param))
                .and_then(|c| c.attribute("value"))
                .and_then(|v| v.parse::<f32>().ok())
        };
        let color = match val("Color Space").map(|v| v as i32) {
            Some(0) => ColorSpace::Mono,
            Some(2) => ColorSpace::Rbg,
            Some(3) => ColorSpace::Bgr,
            Some(4) => ColorSpace::Brg,
            Some(5) => ColorSpace::Gbr,
            Some(6) => ColorSpace::Grb,
            _ => ColorSpace::Rgb,
        };
        blocks.push(Block::Pixels(PixelBlock {
            width: val("Width").unwrap_or(1.0).clamp(1.0, 512.0) as u32,
            height: val("Height").unwrap_or(1.0).clamp(1.0, 512.0) as u32,
            color,
            gamma: val("Gamma").unwrap_or(2.5).clamp(1.0, 3.0),
            ..Default::default()
        }));
    }
    if blocks.is_empty() {
        return Err(ProjectError::Avc(format!("fixture \"{name}\" has no pixels")));
    }
    Ok(Personality { name, blocks })
}

/// A file name for a personality (`/`, `\` and the like replaced).
pub fn file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()×".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.json", clean.trim())
}

pub fn save(dir: &Path, p: &Personality) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(&p.name));
    std::fs::write(&path, serde_json::to_string_pretty(p)?)?;
    Ok(path)
}

/// Built-ins, then every `.json` / fixture `.xml` in `dirs` (later entries with
/// the same name replace earlier ones; sorted by name after the built-ins).
pub fn load_library(dirs: &[PathBuf]) -> Vec<Personality> {
    let mut out = Personality::builtin();
    let mut found: Vec<Personality> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            let ext = f
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            let p = match ext.as_str() {
                "json" => serde_json::from_str::<Personality>(&text).ok(),
                "xml" => import_fixture_xml(&text).ok(),
                _ => None,
            };
            if let Some(p) = p {
                found.retain(|q| q.name != p.name);
                found.push(p);
            }
        }
    }
    found.sort_by_key(|a| a.name.to_lowercase());
    out.retain(|b| !found.iter().any(|f| f.name == b.name));
    out.extend(found);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LED16: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Fixture name="Fixture" uuid="83d2b59141554bccb1e9632080f11aa7" fixtureName="LED 16">
	<Params name="Params">
		<ParamFixturePixels storage="0" name="Pixels">
			<ParamRange name="Width" default="1" value="16">
				<ValueRange name="defaultRange" min="1" max="512"/>
			</ParamRange>
			<ParamRange name="Height" default="1" value="1">
				<ValueRange name="defaultRange" min="1" max="512"/>
			</ParamRange>
			<ParamChoice name="Color Space" default="1" value="1" storeChoices="0"/>
			<ParamChoice name="Distribution" default="170" value="170" storeChoices="0"/>
			<ParamRange name="Gamma" default="2.5" value="2.5">
				<ValueRange name="defaultRange" min="1" max="3"/>
			</ParamRange>
		</ParamFixturePixels>
	</Params>
</Fixture>"#;

    #[test]
    fn reads_led16_xml() {
        let p = import_fixture_xml(LED16).unwrap();
        assert_eq!(p, Personality::led_strip("LED 16", 16, 1));
        assert_eq!(p.channels(), 48);
    }

    #[test]
    fn library_round_trip() {
        let dir = std::env::temp_dir().join(format!("lavmfx-fixtures-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut p = Personality::led_strip("My/Strip", 30, 1);
        p.blocks.push(Block::Param {
            name: "Strobe".into(),
            value: 0,
        });
        let path = save(&dir, &p).unwrap();
        assert!(path.ends_with("My_Strip.json"));
        std::fs::write(dir.join("LED 16.xml"), LED16).unwrap();
        let lib = load_library(std::slice::from_ref(&dir));
        assert!(lib.contains(&p));
        assert_eq!(lib.iter().filter(|q| q.name == "LED 16").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
