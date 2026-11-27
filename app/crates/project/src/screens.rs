//! Import an Advanced Output setup (`Preferences/AdvancedOutput.xml`,
//! also the format of its Advanced Output presets) into
//! [`lavmfx_core::output::AdvancedOutput`].
//!
//! Per slice: `InputRect` (composition px), `OutputRect` (screen px),
//! `Warper/BezierWarper` (control grid in OutputRect space) and
//! `Warper/Homography` (`src` = OutputRect → `dst` = the corner pin). We
//! keep the pin's `dst` as the output quad and normalise the grid to 0..1.
//! A non-identity Bezier grid becomes our Bezier grid (smooth curves
//! through the points; the file's are control points, so curves differ slightly).

use std::collections::HashMap;
use std::path::Path;

use lavmfx_core::output::{AdvancedOutput, Device, PointMode, Pt, Screen, Slice, Warp, bounds};
use roxmltree::{Document, Node};

use crate::{ProjectError, Result};

pub fn import_file(path: &Path) -> Result<AdvancedOutput> {
    let text = std::fs::read_to_string(path)?;
    import_str(&text)
}

pub fn import_str(text: &str) -> Result<AdvancedOutput> {
    let doc = Document::parse(text).map_err(|e| ProjectError::Avc(format!("advanced output XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "ScreenSetup" {
        return Err(ProjectError::Avc(
            "not an advanced output file (no <ScreenSetup>)".into(),
        ));
    }
    let mut out = AdvancedOutput::default();
    let mut next = 1u64;
    let mut id = || {
        let i = next;
        next += 1;
        i
    };
    for s in root.descendants().filter(|n| n.has_tag_name("Screen")) {
        let params = params(s);
        let mut screen = Screen {
            id: id(),
            name: params
                .get("Name")
                .cloned()
                .unwrap_or_else(|| s.attribute("name").unwrap_or("Screen").into()),
            ..Default::default()
        };
        if let Some(dev) = child(s, "OutputDevice").and_then(|d| d.children().find(|n| n.is_element())) {
            let num = |a: &str| dev.attribute(a).and_then(|v| v.parse::<f32>().ok());
            if let (Some(w), Some(h)) = (num("width"), num("height")) {
                screen.width = w.round().max(1.0) as u32;
                screen.height = h.round().max(1.0) as u32;
            }
            let tag = dev.tag_name().name();
            screen.device = if tag.contains("Virtual") {
                Device::Virtual
            } else if tag.contains("Display") || tag.contains("Monitor") {
                Device::Display {
                    index: 0,
                    name: dev.attribute("name").unwrap_or_default().into(),
                }
            } else {
                Device::Window
            };
        }
        for l in child(s, "layers")
            .into_iter()
            .flat_map(|l| l.children())
            .filter(|n| n.has_tag_name("Slice"))
        {
            screen.slices.push(slice(l, id()));
        }
        // The file lists the top slice last (Slice 1 first) —
        // keep file order = bottom first.
        out.screens.push(screen);
    }
    out.next_id = next;
    Ok(out)
}

fn child<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(tag))
}

/// `Param*` name → value of the node's own `Params` groups.
fn params(n: Node) -> HashMap<String, String> {
    n.children()
        .filter(|c| c.has_tag_name("Params"))
        .flat_map(|p| p.children())
        .filter(|p| p.is_element() && p.tag_name().name().starts_with("Param"))
        .filter_map(|p| Some((p.attribute("name")?.to_string(), p.attribute("value")?.to_string())))
        .collect()
}

fn quad(n: Option<Node>) -> Option<[Pt; 4]> {
    let pts = points(n?);
    (pts.len() == 4).then(|| [pts[0], pts[1], pts[2], pts[3]])
}

fn points(n: Node) -> Vec<Pt> {
    n.children()
        .filter(|v| v.has_tag_name("v"))
        .map(|v| {
            let f = |a| v.attribute(a).and_then(|s| s.parse().ok()).unwrap_or(0.0);
            [f("x"), f("y")]
        })
        .collect()
}

fn slice(n: Node, id: u64) -> Slice {
    let p = params(n);
    let num = |k: &str| p.get(k).and_then(|v| v.parse::<f32>().ok());
    let flag = |k: &str| p.get(k).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut s = Slice {
        id,
        name: p.get("Name").cloned().unwrap_or_else(|| "Slice".into()),
        ..Default::default()
    };
    if let Some(q) = quad(child(n, "InputRect")) {
        s.input = q;
    }
    let out_rect = quad(child(n, "OutputRect"));
    let warper = child(n, "Warper");
    let pin = warper
        .and_then(|w| child(w, "Homography"))
        .and_then(|h| quad(child(h, "dst")));
    if let Some(q) = pin.or(out_rect) {
        s.output = q;
    }
    if let (Some(r), Some(b)) = (out_rect, warper.and_then(|w| child(w, "BezierWarper"))) {
        let attr = |a| b.attribute(a).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        let (cols, rows) = (attr("controlWidth"), attr("controlHeight"));
        let pts = child(b, "vertices").map(points).unwrap_or_default();
        let (lo, hi) = bounds(&r);
        let (w, h) = ((hi[0] - lo[0]).max(1e-3), (hi[1] - lo[1]).max(1e-3));
        if cols >= 2 && rows >= 2 && pts.len() == cols * rows {
            let warp = Warp {
                mode: PointMode::Bezier,
                cols,
                rows,
                points: pts.iter().map(|p| [(p[0] - lo[0]) / w, (p[1] - lo[1]) / h]).collect(),
            };
            if !warp.is_identity() {
                s.warp = warp;
            }
        }
    }
    s.enabled = !flag("Bypassed");
    s.is_key = flag("Is Key");
    s.black_bg = flag("Black BG");
    // Shown as −1..1 around 0.
    for (k, dst) in [
        ("Brightness", &mut s.color.brightness),
        ("Contrast", &mut s.color.contrast),
        ("Red", &mut s.color.red),
        ("Green", &mut s.color.green),
        ("Blue", &mut s.color.blue),
    ] {
        if let Some(v) = num(k) {
            *dst = v;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use lavmfx_core::output::rect;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ScreenSetup name="ScreenSetup">
  <CurrentCompositionTextureSize width="1280" height="720"/>
  <screens>
    <Screen name="Screen 1" uniqueId="1">
      <Params name="Params"><Param name="Name" T="STRING" default="" value="Front"/></Params>
      <layers>
        <Slice uniqueId="2">
          <Params name="Common"><Param name="Name" T="STRING" default="Layer" value="Slice 1"/></Params>
          <InputRect orientation="0"><v x="0" y="0"/><v x="640" y="0"/><v x="640" y="360"/><v x="0" y="360"/></InputRect>
          <OutputRect orientation="0"><v x="0" y="0"/><v x="1280" y="0"/><v x="1280" y="720"/><v x="0" y="720"/></OutputRect>
          <Warper>
            <BezierWarper controlWidth="2" controlHeight="2"><vertices>
              <v x="0" y="0"/><v x="1280" y="0"/><v x="0" y="720"/><v x="1280" y="720"/>
            </vertices></BezierWarper>
            <Homography>
              <src><v x="0" y="0"/><v x="1280" y="0"/><v x="1280" y="720"/><v x="0" y="720"/></src>
              <dst><v x="100" y="50"/><v x="1200" y="0"/><v x="1280" y="720"/><v x="0" y="700"/></dst>
            </Homography>
          </Warper>
        </Slice>
      </layers>
      <OutputDevice><OutputDeviceVirtual name="Screen 1" width="1280" height="720"/></OutputDevice>
    </Screen>
  </screens>
</ScreenSetup>"#;

    #[test]
    fn imports_screen_and_slice() {
        let a = import_str(XML).unwrap();
        assert_eq!(a.screens.len(), 1);
        let s = &a.screens[0];
        assert_eq!((s.name.as_str(), s.width, s.height), ("Front", 1280, 720));
        assert_eq!(s.device, Device::Virtual);
        let sl = &s.slices[0];
        assert_eq!(sl.name, "Slice 1");
        assert_eq!(sl.input, rect(0.0, 0.0, 640.0, 360.0));
        assert_eq!(sl.output[0], [100.0, 50.0], "corner pin dst is the output quad");
        assert!(sl.warp.is_identity());
        assert!(a.next_id > sl.id);
    }

    /// An installed app's real setup, when there is one on this machine.
    #[test]
    fn imports_installed_setup() {
        let Some(p) = crate::paths::import_prefs_dirs()
            .into_iter()
            .map(|d| d.join("AdvancedOutput.xml"))
            .find(|p| p.is_file())
        else {
            return;
        };
        let a = import_file(&p).unwrap();
        assert!(!a.screens.is_empty());
        assert!(a.screens.iter().all(|s| !s.slices.is_empty()));
    }
}
