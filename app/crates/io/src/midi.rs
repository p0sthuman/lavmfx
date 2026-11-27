//! MIDI in / out (midir: ALSA / CoreMIDI / WinMM).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use lavmfx_core::timecode::{MtcDecoder, Reading, SharedTimecode};

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::{IoError, Result};

const CLIENT: &str = "LAVMFX";

/// The parts of a MIDI message shortcuts care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum MidiKind {
    Note,
    Cc,
    /// Pitch bend (14 bit, `number` unused).
    Bend,
}

/// One channel message (channel 0..15).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiMsg {
    pub kind: MidiKind,
    pub channel: u8,
    pub number: u8,
    /// 0..1 (velocity / CC value / bend); note off = 0.
    pub value: f64,
}

impl MidiMsg {
    pub fn parse(b: &[u8]) -> Option<Self> {
        let (&status, rest) = b.split_first()?;
        let channel = status & 0x0f;
        let d = |i: usize| rest.get(i).copied().unwrap_or(0);
        Some(match status & 0xf0 {
            0x90 => Self {
                kind: MidiKind::Note,
                channel,
                number: d(0),
                value: d(1) as f64 / 127.0,
            },
            0x80 => Self {
                kind: MidiKind::Note,
                channel,
                number: d(0),
                value: 0.0,
            },
            0xb0 => Self {
                kind: MidiKind::Cc,
                channel,
                number: d(0),
                value: d(1) as f64 / 127.0,
            },
            0xe0 => Self {
                kind: MidiKind::Bend,
                channel,
                number: 0,
                value: ((d(1) as u16) << 7 | d(0) as u16) as f64 / 16383.0,
            },
            _ => return None,
        })
    }

    /// Bytes to send `value` (0..1) as this kind of message.
    pub fn bytes(&self) -> Vec<u8> {
        let v7 = (self.value.clamp(0.0, 1.0) * 127.0).round() as u8;
        match self.kind {
            MidiKind::Note => vec![0x90 | self.channel, self.number & 0x7f, v7],
            MidiKind::Cc => vec![0xb0 | self.channel, self.number & 0x7f, v7],
            MidiKind::Bend => {
                let v = (self.value.clamp(0.0, 1.0) * 16383.0).round() as u16;
                vec![0xe0 | self.channel, (v & 0x7f) as u8, (v >> 7) as u8]
            }
        }
    }
}

/// A message and the input port it came from.
#[derive(Debug, Clone)]
pub struct MidiIn {
    pub device: String,
    pub msg: MidiMsg,
}

/// Port names (inputs, outputs); our own ports are left out.
pub fn ports() -> (Vec<String>, Vec<String>) {
    let ins: Vec<String> = MidiInput::new(CLIENT)
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
        .unwrap_or_default();
    let outs: Vec<String> = MidiOutput::new(CLIENT)
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
        .unwrap_or_default();
    let mine = |n: &String| !n.starts_with(CLIENT);
    (
        ins.into_iter().filter(mine).collect(),
        outs.into_iter().filter(mine).collect(),
    )
}

/// Open input ports, all delivering into one queue.
pub struct MidiInputs {
    _conns: Vec<MidiInputConnection<()>>,
    rx: Receiver<MidiIn>,
    pub open: Vec<String>,
}

impl MidiInputs {
    /// Open every input whose name is in `names`. MIDI Timecode from any of
    /// them goes to `mtc`.
    pub fn open(names: &[String], mtc: Arc<SharedTimecode>) -> Result<Self> {
        let (tx, rx) = channel();
        let mut conns = Vec::new();
        let mut open = Vec::new();
        let probe = MidiInput::new(CLIENT).map_err(|e| IoError::Other(e.to_string()))?;
        for p in probe.ports() {
            let Ok(name) = probe.port_name(&p) else { continue };
            if !names.contains(&name) {
                continue;
            }
            let mut m = MidiInput::new(CLIENT).map_err(|e| IoError::Other(e.to_string()))?;
            // Timecode (F1) and sysex (full-frame MTC) are wanted; clock bytes
            // arrive too and are dropped by `parse`.
            m.ignore(midir::Ignore::ActiveSense);
            let tx: Sender<MidiIn> = tx.clone();
            let dev = name.clone();
            let mtc = mtc.clone();
            let mut dec = MtcDecoder::default();
            match m.connect(
                &p,
                "lavmfx-in",
                move |_, bytes, _| {
                    if let Some(p) = dec.message(bytes) {
                        mtc.store(Reading {
                            secs: p.secs,
                            fps: p.fps,
                            at: std::time::Instant::now(),
                            locate: p.full,
                        });
                        return;
                    }
                    if let Some(msg) = MidiMsg::parse(bytes) {
                        let _ = tx.send(MidiIn {
                            device: dev.clone(),
                            msg,
                        });
                    }
                },
                (),
            ) {
                Ok(c) => {
                    conns.push(c);
                    open.push(name);
                }
                Err(e) => log::warn!("MIDI in {name}: {e}"),
            }
        }
        Ok(Self {
            _conns: conns,
            rx,
            open,
        })
    }

    pub fn poll(&self) -> Vec<MidiIn> {
        self.rx.try_iter().collect()
    }
}

/// Open output ports by name (feedback: LEDs, motor faders).
pub struct MidiOutputs {
    conns: Vec<(String, MidiOutputConnection)>,
}

impl MidiOutputs {
    pub fn open(names: &[String]) -> Result<Self> {
        let mut conns = Vec::new();
        let probe = MidiOutput::new(CLIENT).map_err(|e| IoError::Other(e.to_string()))?;
        for p in probe.ports() {
            let Ok(name) = probe.port_name(&p) else { continue };
            if !names.contains(&name) {
                continue;
            }
            let m = MidiOutput::new(CLIENT).map_err(|e| IoError::Other(e.to_string()))?;
            match m.connect(&p, "lavmfx-out") {
                Ok(c) => conns.push((name, c)),
                Err(e) => log::warn!("MIDI out {name}: {e}"),
            }
        }
        Ok(Self { conns })
    }

    pub fn names(&self) -> Vec<String> {
        self.conns.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Send to the output named `device`, or to all when `None`.
    pub fn send(&mut self, device: Option<&str>, msg: &MidiMsg) {
        let b = msg.bytes();
        for (n, c) in &mut self.conns {
            if device.is_none_or(|d| d == n) {
                let _ = c.send(&b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_encode() {
        let m = MidiMsg::parse(&[0x91, 60, 127]).unwrap();
        assert_eq!((m.kind, m.channel, m.number, m.value), (MidiKind::Note, 1, 60, 1.0));
        assert_eq!(MidiMsg::parse(&[0x81, 60, 64]).unwrap().value, 0.0);
        let c = MidiMsg::parse(&[0xb0, 7, 64]).unwrap();
        assert_eq!(c.kind, MidiKind::Cc);
        assert_eq!(c.bytes(), vec![0xb0, 7, 64]);
        assert!(MidiMsg::parse(&[0xf8]).is_none(), "clock ignored");
    }

    /// Needs ALSA sequencer: a virtual port feeds a real input connection.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn virtual_port_loopback() {
        use midir::os::unix::VirtualOutput;
        let out = MidiOutput::new("Loop").unwrap();
        let mut v = out.create_virtual("loop-test").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (ins, _) = ports();
        let name = ins
            .into_iter()
            .find(|n| n.contains("loop-test"))
            .expect("virtual port listed");
        let mtc = Arc::new(SharedTimecode::default());
        let inputs = MidiInputs::open(&[name], mtc.clone()).unwrap();
        v.send(&[0xb0, 1, 127]).unwrap();
        // MIDI Timecode: 8 quarter frames of 01:02:03:04 at 25 fps, then a clock tick.
        use lavmfx_core::timecode::{Fps, Timecode, mtc_quarter_frames};
        for q in mtc_quarter_frames(Timecode::new(1, 2, 3, 4), Fps::F25) {
            v.send(&q).unwrap();
        }
        v.send(&[0xf8]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let got = inputs.poll();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].msg.number, 1);
        let r = mtc.load().expect("MTC decoded");
        assert_eq!(r.fps, Fps::F25);
        assert_eq!(Timecode::from_seconds(r.secs, r.fps), Timecode::new(1, 2, 3, 5));
        // Full frame (locate).
        v.send(&[0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x20 | 2, 0, 0, 0, 0xF7])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let r = mtc.load().unwrap();
        assert!(r.locate);
        assert_eq!(Timecode::from_seconds(r.secs, r.fps), Timecode::new(2, 0, 0, 0));
    }
}
