//! DMX pixel mapping output: the composition readback (from `live_out`) is
//! sampled under every fixture on a thread and sent as Art-Net / sACN, each
//! lumiverse at its own frame rate. The last channels of each lumiverse are
//! kept for the editor (Fixture Output Preview, DMX Output tab).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lavmfx_core::dmx::{Lumiverse, UNIVERSE, sample_rgba};
use lavmfx_io::DmxOutput;
use lavmfx_io::dmx::{DmxSender, DmxTarget};
use lavmfx_media::live::{FrameSink, OutFrame};

/// What the thread sends: the lumiverses + network settings.
#[derive(Clone, PartialEq, Default)]
struct Config {
    lumiverses: Vec<Lumiverse>,
    bind: Option<std::net::Ipv4Addr>,
    name: String,
}

/// Per lumiverse (by id): last channels, packets sent, error.
#[derive(Clone, Default)]
pub struct LumiState {
    pub channels: Vec<u8>,
    pub sent: u64,
    pub error: Option<String>,
}

#[derive(Default)]
struct Shared {
    config: Config,
    state: HashMap<u64, LumiState>,
    /// Buffers handed back for reuse.
    pool: Vec<Vec<u8>>,
    quit: bool,
}

pub struct DmxOut {
    shared: Arc<Mutex<Shared>>,
    tx: SyncSender<OutFrame>,
    thread: Option<JoinHandle<()>>,
    /// Main-thread copy of the config (only changes are passed on).
    last: Config,
}

impl DmxOut {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = sync_channel::<OutFrame>(1);
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("dmx out".into())
            .spawn(move || run(sh, rx))
            .ok();
        Self {
            shared,
            tx,
            thread,
            last: Config::default(),
        }
    }

    /// Lumiverses to send (sends while at least one exists; disabled ones
    /// send zeros = blackout) and the network settings.
    pub fn configure(&mut self, lumiverses: &[Lumiverse], bind: Option<std::net::Ipv4Addr>, name: &str) {
        if self.last.lumiverses == lumiverses && self.last.bind == bind && self.last.name == name {
            return;
        }
        self.last = Config {
            lumiverses: lumiverses.to_vec(),
            bind,
            name: name.into(),
        };
        let mut s = self.shared.lock().unwrap();
        s.config = self.last.clone();
        let ids: Vec<u64> = lumiverses.iter().map(|l| l.id).collect();
        s.state.retain(|id, _| ids.contains(id));
    }

    /// Wanted: there is something to send.
    pub fn wanted(lumiverses: &[Lumiverse]) -> bool {
        !lumiverses.is_empty()
    }

    /// Last channels / counters of a lumiverse.
    pub fn state(&self, id: u64) -> Option<LumiState> {
        self.shared.lock().unwrap().state.get(&id).cloned()
    }
}

impl Drop for DmxOut {
    fn drop(&mut self) {
        self.shared.lock().unwrap().quit = true;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl FrameSink for DmxOut {
    fn buffer(&self, len: usize) -> Vec<u8> {
        let mut b = self.shared.lock().unwrap().pool.pop().unwrap_or_default();
        b.resize(len, 0);
        b
    }
    fn send(&self, f: OutFrame) -> bool {
        match self.tx.try_send(f) {
            Ok(()) => true,
            Err(TrySendError::Full(f) | TrySendError::Disconnected(f)) => {
                self.shared.lock().unwrap().pool.push(f.data);
                false
            }
        }
    }
    fn status(&self) -> String {
        let s = self.shared.lock().unwrap();
        let errors: Vec<String> = s.state.values().filter_map(|l| l.error.clone()).collect();
        if errors.is_empty() {
            format!("Sending {} lumiverse(s)", s.config.lumiverses.len())
        } else {
            format!("Error: {}", errors.join("; "))
        }
    }
}

fn run(shared: Arc<Mutex<Shared>>, rx: Receiver<OutFrame>) {
    let mut frame: Option<OutFrame> = None;
    let mut sender: Option<(DmxSender, Option<std::net::Ipv4Addr>, String)> = None;
    let mut last_sent: HashMap<u64, Instant> = HashMap::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(f) => {
                if let Some(old) = frame.replace(f) {
                    shared.lock().unwrap().pool.push(old.data);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let config = {
            let s = shared.lock().unwrap();
            if s.quit {
                return;
            }
            s.config.clone()
        };
        let Some(f) = &frame else { continue };
        let now = Instant::now();
        // Which lumiverses are due.
        let due: Vec<&Lumiverse> = config
            .lumiverses
            .iter()
            .filter(|l| {
                let period = Duration::from_secs_f32(1.0 / l.framerate.clamp(1.0, 120.0));
                last_sent.get(&l.id).is_none_or(|t| now.duration_since(*t) >= period)
            })
            .collect();
        if due.is_empty() {
            continue;
        }
        if sender
            .as_ref()
            .is_none_or(|(_, b, n)| *b != config.bind || *n != config.name)
        {
            sender = match DmxSender::new(config.bind, &config.name) {
                Ok(s) => Some((s, config.bind, config.name.clone())),
                Err(e) => {
                    let mut s = shared.lock().unwrap();
                    for l in &due {
                        s.state.entry(l.id).or_default().error = Some(format!("network: {e}"));
                    }
                    drop(s);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
        }
        let Some((tx, ..)) = sender.as_mut() else { continue };
        for l in due {
            last_sent.insert(l.id, now);
            let ch = l.render(&mut |p| sample_rgba(&f.data, f.width, f.height, f.stride, p));
            let to = DmxTarget {
                protocol: l.protocol,
                ip: l.target_ip.clone(),
            };
            let mut error = None;
            for (k, part) in ch.chunks(UNIVERSE).enumerate() {
                if let Err(e) = tx.send_universe(&to, l.universe.wrapping_add(k as u16), part) {
                    error = Some(format!("{}: {e}", l.name));
                    break;
                }
            }
            let mut s = shared.lock().unwrap();
            let st = s.state.entry(l.id).or_default();
            st.channels = ch;
            st.sent += 1;
            st.error = error;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lavmfx_core::dmx::{Fixture, Personality};
    use lavmfx_core::output::rect;

    #[test]
    fn samples_frames_into_lumiverse_channels() {
        let mut out = DmxOut::new();
        let l = Lumiverse {
            id: 7,
            // Loopback only (the Art-Net port on this machine).
            target_ip: "127.0.0.1".into(),
            universe: 9,
            framerate: 100.0,
            fixtures: vec![Fixture {
                personality: {
                    let mut p = Personality::led_strip("t", 2, 1);
                    if let lavmfx_core::dmx::Block::Pixels(b) = &mut p.blocks[0] {
                        b.gamma = 1.0;
                    }
                    p
                },
                input: rect(0.0, 0.0, 2.0, 1.0),
                ..Default::default()
            }],
            ..Default::default()
        };
        out.configure(&[l], Some(std::net::Ipv4Addr::LOCALHOST), "Test");
        // 2×1 frame: red, blue (stride 8).
        let mut data = out.buffer(8);
        data.copy_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(out.send(OutFrame {
            data,
            width: 2,
            height: 1,
            stride: 8,
            rate: (60, 1),
        }));
        let t = Instant::now();
        while out.state(7).is_none_or(|s| s.sent == 0) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let s = out.state(7).expect("sent");
        assert_eq!(s.error, None);
        assert_eq!(&s.channels[..6], [255, 0, 0, 0, 0, 255]);
        assert!(out.status().starts_with("Sending 1"));
    }
}
