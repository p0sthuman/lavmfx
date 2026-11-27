//! Composition model, parameters and clock.
//!
//! The data model follows a widely used REST API schema so the same JSON can
//! be served to existing controllers (M7). Undoable commands arrive with live control (M7).

pub mod align;
pub mod audiofx;
pub mod autopilot;
pub mod chase;
pub mod clipboard;
pub mod clock;
pub mod control;
pub mod deck;
pub mod defaults;
pub mod dmx;
pub mod drive;
pub mod edit;
pub mod effects;
pub mod ffgl;
pub mod fft;
pub mod grid;
pub mod groups;
pub mod history;
pub mod id;
pub mod isf;
pub mod media;
pub mod mix;
pub mod model;
pub mod output;
pub mod param;
pub mod sources;
pub mod timecode;
pub mod transport;
pub mod trigger;

pub use clock::Clock;
pub use id::{Id, IdGen};
pub use model::*;
pub use param::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spare_column_stays_empty() {
        let mut c = Composition::new("T", 2, 3);
        assert!(!c.ensure_spare_column(), "last column already empty");
        let info = VideoFileInfo {
            path: "a.mov".into(),
            exists: true,
            duration: String::new(),
            duration_ms: 1000.0,
            framerate: FrameRate { num: 30, denom: 1 },
            width: 2,
            height: 2,
        };
        let ids = &mut c.ids;
        c.layers[1].clips[2].load_video(ids, "a", info);
        assert!(c.ensure_spare_column());
        assert_eq!(c.columns.len(), 4);
        assert!(c.layers.iter().all(|l| l.clips.len() == 4 && l.clips[3].is_empty()));
        assert!(!c.ensure_spare_column());
    }

    #[test]
    fn default_composition_shape() {
        let c = Composition::new("Test", 3, 8);
        assert_eq!(c.layers.len(), 3);
        assert_eq!(c.columns.len(), 8);
        assert!(c.layers.iter().all(|l| l.clips.len() == 8));
        assert!(c.layers[0].clips[0].is_empty());
    }

    #[test]
    fn load_video_round_trips() {
        let mut c = Composition::new("Test", 1, 2);
        let info = VideoFileInfo {
            path: "/media/clip.mov".into(),
            exists: true,
            duration: "00:00:02.00".into(),
            duration_ms: 2000.0,
            framerate: FrameRate { num: 30, denom: 1 },
            width: 640,
            height: 360,
        };
        let clip = &mut c.layers[0].clips[1];
        clip.load_video(&mut c.ids, "clip", info);
        let clip = &c.layers[0].clips[1];
        assert!(!clip.is_empty());
        assert_eq!(clip.video_path(), Some("/media/clip.mov"));
        assert!(matches!(&clip.transport, Some(Transport::Timeline(t)) if t.position.max == 2000.0));
        let json = serde_json::to_string(&c).unwrap();
        let back: Composition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn ids_are_unique() {
        let c = Composition::new("Test", 4, 6);
        let json = serde_json::to_value(&c).unwrap();
        let mut ids = Vec::new();
        collect_ids(&json, &mut ids);
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate ids");
        assert!(n > 100);
    }

    fn collect_ids(v: &serde_json::Value, out: &mut Vec<i64>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    if k == "id" {
                        out.push(v.as_i64().unwrap());
                    } else {
                        collect_ids(v, out);
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| collect_ids(v, out)),
            _ => {}
        }
    }

    #[test]
    fn json_roundtrip() {
        let mut c = Composition::new("Roundtrip", 2, 3);
        c.layers[0].clips[1].transport = Some(Transport::Timeline(TransportTimeline::new(&mut c.ids, 4.0)));
        let json = serde_json::to_string(&c).unwrap();
        let back: Composition = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn rest_shaped_param_parses() {
        // Shape as served by the REST API.
        let j = r#"{"id":1609, "valuetype":"ParamRange", "min":0.0, "max":1.0, "value":0.5,
                    "view":{"display_units":"percent","control_type":"slider"}}"#;
        let p: Param = serde_json::from_str(j).unwrap();
        assert!(matches!(p, Param::Range(ref r) if r.value == 0.5));
        let j = r#"{"id":2, "valuetype":"ParamChoice", "value":"Add", "index":1, "options":["Alpha","Add"]}"#;
        assert!(matches!(serde_json::from_str::<Param>(j).unwrap(), Param::Choice(_)));
    }

    #[test]
    fn duplicate_gets_fresh_ids() {
        let mut c = Composition::new("T", 1, 2);
        c.layers[0].clips[0].name.value = "a".into();
        let d = c.layers[0].clips[0].duplicate(&mut c.ids);
        assert_eq!(d.name.value, "a");
        assert_ne!(d.id, c.layers[0].clips[0].id);
        assert_ne!(d.name.id, c.layers[0].clips[0].name.id);
        c.layers[0].clips[1] = d;
        let mut ids = Vec::new();
        collect_ids(&serde_json::to_value(&c).unwrap(), &mut ids);
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate ids");
    }

    #[test]
    fn range_clamps() {
        let mut ids = IdGen::new();
        let mut r = RangeParam::new(ids.alloc(), 0.0, 1.0, 5.0);
        assert_eq!(r.value, 1.0);
        r.set(-1.0);
        assert_eq!(r.value, 0.0);
    }
}
