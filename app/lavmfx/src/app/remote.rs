//! OSC in / out and the REST webserver (M7 slice 2). Both follow
//! Preferences › OSC / Webserver and end in [`LavmfxApp::control`].
//!
//! OSC output (feedback): every ~33 ms the values of watched addresses are
//! read and sent when they changed. Watched = a built-in set (composition
//! master / speed / tempo / crossfader, each layer's master, opacity, volume,
//! bypass, solo, name; each clip's `connected` state and name in the active
//! deck) plus every param address a controller has sent to. Everything is
//! resent within a second (a slice per tick) for controllers that connect later.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use lavmfx_core::control::{self, Outcome, Value};
use lavmfx_io::osc::{OscInput, OscOutput};
use lavmfx_io::web::{HttpRequest, WebServer};

use super::LavmfxApp;
use super::prefs::{Osc, Webserver};

#[derive(Default)]
pub struct Remote {
    osc_cfg: Option<Osc>,
    osc_in: Option<OscInput>,
    osc_out: Option<OscOutput>,
    web_cfg: Option<Webserver>,
    web: Option<WebServer>,
    /// Param addresses controllers have used (fed back too).
    watched: BTreeSet<String>,
    /// Last value sent per address.
    sent: HashMap<String, Value>,
    /// Last (connected state, name) sent per clip slot [layer][column]: slots
    /// are sent only when they change (a 10 000-slot `.avc` made ~20 000
    /// strings per tick otherwise).
    slots: Vec<Vec<(i64, String)>>,
    last_feedback: Option<Instant>,
    /// Last full resend (controllers that connect later, e.g. a tablet
    /// opening Open Stage Control, get everything within a second).
    last_full: Option<Instant>,
    /// Feedback ticks (selects the slice of values resent this tick).
    tick: u64,
    pub osc_status: String,
    pub web_status: String,
    /// Tablet grid window offset (layer, column).
    pub grid_off: (usize, usize),
    /// The tablet moved its window: the app's clip grid scrolls to match.
    pub grid_pending: bool,
    /// (lowest layer, first column) the app's clip grid showed last; when it
    /// changes (scrolled in the app), the tablet's window follows.
    pub grid_seen: Option<(usize, usize)>,
    /// Slice Chase effect the tablet's Chase tab controls (index into the
    /// composition's chase effects, see `tablet::chase_effects`); `None` =
    /// the first one that isn't bypassed.
    pub chase_pick: Option<usize>,
}

/// Readable bind failure ("port 8080 is already used by another program…").
fn bind_error(port: u16, e: &str) -> String {
    if e.contains("in use") || e.contains("os error 98") || e.contains("os error 10048") {
        format!(
            "port {port} is already used by another program; choose another port (e.g. {})",
            port + 1
        )
    } else if e.contains("os error 13") || e.contains("ermission") {
        format!("no permission for port {port}; use a port above 1024")
    } else {
        format!("can't use port {port}: {e}")
    }
}

/// Clip slot changes sent per feedback tick (~30 ticks/s).
const SLOT_BUDGET: usize = 200;

/// Unchanged values are resent once every this many feedback ticks (~1 s).
const REFRESH_TICKS: u64 = 30;

/// Clip `connected` states.
const EMPTY: i64 = 0;
const DISCONNECTED: i64 = 1;
const CONNECTED: i64 = 3;

impl LavmfxApp {
    /// Once per frame: apply preference changes, handle input, send feedback.
    pub(super) fn tick_remote(&mut self) {
        self.sync_remote();
        let msgs = self.remote.osc_in.as_ref().map(OscInput::poll).unwrap_or_default();
        for m in msgs {
            if m.address.starts_with("/lavmfx/") {
                self.tablet_osc(&m);
                continue;
            }
            if m.query {
                let sel = self.control_selection();
                if let Some(v) = control::read(&mut self.comp, &sel, &m.address)
                    && let Some(out) = self.remote.osc_out.as_mut()
                {
                    out.send(&m.address, &v);
                }
                continue;
            }
            let out = self.control(&m.address, m.value);
            match out {
                Outcome::Changed | Outcome::Ignored if !m.address.ends_with("/connect") => {
                    self.remote.watched.insert(m.address.clone());
                }
                Outcome::Unknown => log::debug!("osc: unknown address {}", m.address),
                _ => {}
            }
            if let Outcome::Action(control::Action::ConnectClip {
                layer,
                column,
                on: true,
            }) = out
                && self.prefs.general.follow_external_triggers
            {
                self.select_only((layer, column));
            }
        }
        let reqs = self.remote.web.as_ref().map(WebServer::poll).unwrap_or_default();
        for r in reqs {
            self.rest(r);
        }
        self.osc_feedback();
        self.prefs_win.osc_status = self.remote.osc_status.clone();
        self.prefs_win.web_status = self.remote.web_status.clone();
    }

    fn sync_remote(&mut self) {
        let o = self.prefs.osc.clone();
        if self.remote.osc_cfg.as_ref() != Some(&o) {
            let r = &mut self.remote;
            r.osc_in = None;
            r.osc_out = None;
            r.sent.clear();
            r.slots.clear();
            let mut status = Vec::new();
            if o.input {
                match OscInput::start(o.in_port) {
                    Ok(i) => {
                        r.osc_in = Some(i);
                        status.push(format!("Listening on port {}", o.in_port));
                    }
                    Err(e) => status.push(format!("Error: OSC input: {}", bind_error(o.in_port, &e.to_string()))),
                }
            }
            if o.output {
                match OscOutput::new(&o.out_address, o.out_port, o.bundles) {
                    Ok(out) => {
                        status.push(format!("sending to {}", out.target));
                        r.osc_out = Some(out);
                    }
                    Err(e) => status.push(format!("Output: {e}")),
                }
            }
            r.osc_status = status.join(", ");
            log::info!("OSC: {}", r.osc_status);
            if let Some(e) = status.iter().find(|s| s.starts_with("Error: ")) {
                self.status = e.trim_start_matches("Error: ").to_owned();
            }
            r.osc_cfg = Some(o);
        }
        let w = self.prefs.webserver.clone();
        if self.remote.web_cfg.as_ref() != Some(&w) {
            let r = &mut self.remote;
            r.web = None;
            r.web_status = if w.enabled {
                match WebServer::start(&w.address, w.port) {
                    Ok(s) => {
                        r.web = Some(s);
                        format!("REST API at http://{}:{}/api/v1/composition", w.address, w.port)
                    }
                    Err(e) => format!("Error: webserver: {}", bind_error(w.port, &e.to_string())),
                }
            } else {
                String::new()
            };
            if !r.web_status.is_empty() {
                log::info!("Webserver: {}", r.web_status);
            }
            if let Some(e) = r.web_status.strip_prefix("Error: ") {
                self.status = e.to_owned();
            }
            self.remote.web_cfg = Some(w);
        }
    }

    fn osc_feedback(&mut self) {
        if self.remote.osc_out.is_none() || self.remote.last_feedback.is_some_and(|t| t.elapsed().as_millis() < 33) {
            return;
        }
        self.remote.last_feedback = Some(Instant::now());
        // Once a second: effect lists are rebuilt. Unchanged values are resent
        // a slice per tick (all within ~1 s), not in one burst: hundreds of
        // datagrams at once overflow the receiver (Open Stage Control dropped some).
        let full = self.remote.last_full.is_none_or(|t| t.elapsed().as_secs_f32() >= 1.0);
        if full {
            self.remote.last_full = Some(Instant::now());
        }
        self.remote.tick = self.remote.tick.wrapping_add(1);
        let mut vals: Vec<(String, Value)> = Vec::new();
        // Changed clip slots: always sent, not part of the slow resend.
        let mut slot_vals: Vec<(String, Value)> = Vec::new();
        let mut params: Vec<String> = [
            "/composition/master",
            "/composition/bypassed",
            "/composition/speed",
            "/composition/tempocontroller/tempo",
            "/composition/crossfader/phase",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let deck = self.comp.active_deck();
        for (li, l) in self.comp.layers.iter().enumerate() {
            let a = format!("/composition/layers/{}", li + 1);
            for p in ["master", "video/opacity", "audio/volume", "bypassed", "solo"] {
                params.push(format!("{a}/{p}"));
            }
            vals.push((format!("{a}/name"), Value::Str(l.name.value.clone())));
            let playing = self
                .playing
                .get(li)
                .and_then(Option::as_ref)
                .filter(|p| p.slot.deck == deck)
                .map(|p| p.slot.col);
            if self.remote.slots.len() <= li {
                self.remote.slots.resize(li + 1, Vec::new());
            }
            let row = &mut self.remote.slots[li];
            row.resize(l.clips.len(), (-1, String::new()));
            for ((ci, c), seen) in l.clips.iter().enumerate().zip(row.iter_mut()) {
                let state = if c.is_empty() {
                    EMPTY
                } else if playing == Some(ci) {
                    CONNECTED
                } else {
                    DISCONNECTED
                };
                // At most SLOT_BUDGET changes per tick (opening a big composition
                // changes thousands; a burst overflows the receiver): the rest
                // stay unsent and go out on the next ticks.
                if slot_vals.len() >= SLOT_BUDGET {
                    break;
                }
                if seen.0 != state {
                    seen.0 = state;
                    slot_vals.push((format!("{a}/clips/{}/connected", ci + 1), Value::Int(state)));
                }
                if seen.1 != c.name.value {
                    seen.1.clone_from(&c.name.value);
                    slot_vals.push((format!("{a}/clips/{}/name", ci + 1), Value::Str(c.name.value.clone())));
                }
            }
        }
        super::prof::mark("osc: clip slots");
        params.extend(self.remote.watched.iter().cloned());
        // (sent as, read from): the tablet's relative addresses read absolute ones.
        let mut pairs: Vec<(String, String)> = params.into_iter().map(|p| (p.clone(), p)).collect();
        self.tablet_feedback(&mut vals, &mut pairs);
        super::prof::mark(if full { "osc: tablet full" } else { "osc: tablet" });
        let sel = self.control_selection();
        let reads: Vec<String> = pairs.iter().map(|(_, r)| r.clone()).collect();
        for ((send, _), v) in pairs.into_iter().zip(control::read_many(&mut self.comp, &sel, &reads)) {
            if let Some(v) = v {
                vals.push((send, v));
            }
        }
        super::prof::mark("osc: read params");
        let r = &mut self.remote;
        let Some(out) = r.osc_out.as_mut() else { return };
        let slice = (r.tick % REFRESH_TICKS) as usize;
        for (i, (a, v)) in vals.into_iter().enumerate() {
            if r.sent.get(&a) != Some(&v) || i % REFRESH_TICKS as usize == slice {
                out.send(&a, &v);
                r.sent.insert(a, v);
            }
        }
        for (a, v) in slot_vals {
            out.send(&a, &v);
        }
        out.flush();
        super::prof::mark("osc: send");
    }

    // ------------------------------------------------------------------ REST

    fn rest(&mut self, r: HttpRequest) {
        let Some(path) = r.path.strip_prefix("/api/v1").map(str::to_owned) else {
            r.respond(404, "text/plain", b"not found".to_vec());
            return;
        };
        log::debug!("REST {} {path}", r.method);
        if path == "/product" {
            r.json(&serde_json::json!({
                "name": "LAVMFX",
                "major": 7, "minor": 19, "micro": 0, "revision": 0
            }));
            return;
        }
        let Some(addr) = control::rest_path(&self.comp, &path) else {
            r.respond(404, "text/plain", b"unknown path".to_vec());
            return;
        };
        let sel = self.control_selection();
        match r.method.as_str() {
            "GET" => match control::owner_json(&self.comp, &sel, &addr) {
                Some(v) => r.json(&v),
                None => r.respond(404, "text/plain", b"not found".to_vec()),
            },
            "PUT" => {
                let Ok(patch) = serde_json::from_str::<serde_json::Value>(&r.body) else {
                    r.respond(400, "text/plain", b"body is not JSON".to_vec());
                    return;
                };
                match control::patch(&mut self.comp, &sel, &addr, &patch) {
                    Some(_) => {
                        self.after_external_change();
                        r.respond(204, "text/plain", Vec::new());
                    }
                    None => r.respond(404, "text/plain", b"not found".to_vec()),
                }
            }
            "POST" => {
                // Body `false` = release of a press (connect); default = press.
                let v = match r.body.trim() {
                    "false" => Value::Bool(false),
                    _ => Value::Trigger,
                };
                match self.control(&addr, v) {
                    Outcome::Unknown => r.respond(404, "text/plain", b"unknown action".to_vec()),
                    _ => r.respond(204, "text/plain", Vec::new()),
                }
            }
            _ => r.respond(405, "text/plain", b"method not allowed".to_vec()),
        }
    }

    /// After a REST PUT: tempo follows, an undo step can be recorded.
    fn after_external_change(&mut self) {
        let bpm = self.comp.tempo_controller.tempo.value;
        if (bpm - self.clock.bpm).abs() > 1e-9 {
            self.clock.set_bpm(bpm);
        }
        self.history_poke = Some(Instant::now());
    }
}
