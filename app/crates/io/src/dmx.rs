//! DMX over the network: Art-Net (ArtDmx, ArtPoll / ArtPollReply) and sACN
//! (E1.31) packets, a UDP sender and an Art-Net receiver.
//!
//! Plain UDP, so tests run on loopback; nothing is sent unless the user set a
//! lumiverse's target (default 127.0.0.1).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lavmfx_core::dmx::{Protocol, UNIVERSE};

use crate::{DmxOutput, IoError, Result};

pub const ARTNET_PORT: u16 = 6454;
pub const SACN_PORT: u16 = 5568;
const ARTNET_ID: &[u8; 8] = b"Art-Net\0";
const OP_DMX: u16 = 0x5000;
const OP_POLL: u16 = 0x2000;
const OP_POLL_REPLY: u16 = 0x2100;

/// ArtDmx packet for one universe (15-bit port address). Data is padded to
/// an even length (2..=512) as the spec wants.
pub fn artnet_dmx(seq: u8, port_address: u16, data: &[u8]) -> Vec<u8> {
    let n = data.len().min(UNIVERSE);
    let len = (n + (n & 1)).max(2);
    let mut p = Vec::with_capacity(18 + len);
    p.extend_from_slice(ARTNET_ID);
    p.extend_from_slice(&OP_DMX.to_le_bytes());
    p.extend_from_slice(&[0, 14, seq, 0]); // protocol 14, sequence, physical
    p.push((port_address & 0xff) as u8); // SubUni
    p.push(((port_address >> 8) & 0x7f) as u8); // Net
    p.extend_from_slice(&(len as u16).to_be_bytes());
    p.extend_from_slice(&data[..n]);
    p.resize(18 + len, 0);
    p
}

/// (port address, channels) of an ArtDmx packet.
pub fn parse_artnet_dmx(p: &[u8]) -> Option<(u16, &[u8])> {
    if p.len() < 18 || &p[..8] != ARTNET_ID || u16::from_le_bytes([p[8], p[9]]) != OP_DMX {
        return None;
    }
    let addr = u16::from(p[14]) | (u16::from(p[15] & 0x7f) << 8);
    let len = (usize::from(u16::from_be_bytes([p[16], p[17]]))).min(UNIVERSE);
    Some((addr, p.get(18..18 + len)?))
}

/// ArtPollReply announcing this node (short name ≤ 17 chars, long ≤ 63).
pub fn artnet_poll_reply(ip: Ipv4Addr, name: &str) -> Vec<u8> {
    let mut p = vec![0u8; 239];
    p[..8].copy_from_slice(ARTNET_ID);
    p[8..10].copy_from_slice(&OP_POLL_REPLY.to_le_bytes());
    p[10..14].copy_from_slice(&ip.octets());
    p[14..16].copy_from_slice(&ARTNET_PORT.to_le_bytes());
    p[23] = 0xd0; // Status1: indicators normal, port-address set by network
    let short: Vec<u8> = name.bytes().take(17).collect();
    p[26..26 + short.len()].copy_from_slice(&short);
    let long: Vec<u8> = name.bytes().take(63).collect();
    p[44..44 + long.len()].copy_from_slice(&long);
    p[173] = 1; // NumPorts lo
    p[174] = 0x80; // PortTypes[0]: can output DMX (we send it)
    p[200] = 0; // Style: StNode
    p
}

fn is_artnet_poll(p: &[u8]) -> bool {
    p.len() >= 10 && &p[..8] == ARTNET_ID && u16::from_le_bytes([p[8], p[9]]) == OP_POLL
}

/// sACN multicast group of a universe.
pub fn sacn_multicast(universe: u16) -> Ipv4Addr {
    Ipv4Addr::new(239, 255, (universe >> 8) as u8, universe as u8)
}

/// E1.31 data packet (start code 0) for one universe.
pub fn sacn_dmx(cid: &[u8; 16], source: &str, seq: u8, universe: u16, priority: u8, data: &[u8]) -> Vec<u8> {
    let n = data.len().min(UNIVERSE);
    let total = 126 + n;
    let fl = |len: usize| (0x7000u16 | len as u16).to_be_bytes();
    let mut p = Vec::with_capacity(total);
    // Root layer.
    p.extend_from_slice(&0x0010u16.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(b"ASC-E1.17\0\0\0");
    p.extend_from_slice(&fl(total - 16));
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(cid);
    // Framing layer.
    p.extend_from_slice(&fl(total - 38));
    p.extend_from_slice(&2u32.to_be_bytes());
    let mut name = [0u8; 64];
    for (o, b) in name.iter_mut().zip(source.bytes().take(63)) {
        *o = b;
    }
    p.extend_from_slice(&name);
    p.push(priority.min(200));
    p.extend_from_slice(&0u16.to_be_bytes()); // sync address
    p.push(seq);
    p.push(0); // options
    p.extend_from_slice(&universe.to_be_bytes());
    // DMP layer.
    p.extend_from_slice(&fl(total - 115));
    p.extend_from_slice(&[0x02, 0xa1]);
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&(n as u16 + 1).to_be_bytes());
    p.push(0); // start code
    p.extend_from_slice(&data[..n]);
    p
}

/// (universe, channels) of an E1.31 data packet with start code 0.
pub fn parse_sacn_dmx(p: &[u8]) -> Option<(u16, &[u8])> {
    if p.len() < 126 || &p[4..16] != b"ASC-E1.17\0\0\0" || p[125] != 0 {
        return None;
    }
    let universe = u16::from_be_bytes([p[113], p[114]]);
    let count = usize::from(u16::from_be_bytes([p[123], p[124]])).saturating_sub(1);
    Some((universe, p.get(126..126 + count.min(UNIVERSE))?))
}

/// Where one lumiverse goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmxTarget {
    pub protocol: Protocol,
    /// IPv4 address; empty (sACN) = multicast.
    pub ip: String,
}

impl DmxTarget {
    fn addr(&self, universe: u16) -> Result<SocketAddr> {
        let ip = self.ip.trim();
        let port = match self.protocol {
            Protocol::ArtNet => ARTNET_PORT,
            Protocol::Sacn => SACN_PORT,
        };
        if ip.is_empty() {
            return match self.protocol {
                Protocol::Sacn => Ok(SocketAddr::new(IpAddr::V4(sacn_multicast(universe)), port)),
                Protocol::ArtNet => Err(IoError::Other("no target IP".into())),
            };
        }
        let ip: Ipv4Addr = ip
            .parse()
            .map_err(|_| IoError::Other(format!("\"{ip}\" is not an IPv4 address")))?;
        Ok(SocketAddr::new(IpAddr::V4(ip), port))
    }
}

/// Sends universes over UDP (one socket for both protocols).
pub struct DmxSender {
    sock: UdpSocket,
    seq: HashMap<(Protocol, u16), u8>,
    cid: [u8; 16],
    pub source_name: String,
}

impl DmxSender {
    /// `bind`: the network adapter's address (None = any).
    pub fn new(bind: Option<Ipv4Addr>, source_name: &str) -> Result<Self> {
        let sock = UdpSocket::bind((bind.unwrap_or(Ipv4Addr::UNSPECIFIED), 0))?;
        sock.set_broadcast(true)?;
        let _ = sock.set_multicast_ttl_v4(4);
        // A CID stable per name (sACN receivers track sources by it).
        let mut cid = [0u8; 16];
        let mut h: u64 = 0xcbf29ce484222325;
        for b in source_name.bytes().chain(*b"lavmfx-dmx") {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        cid[..8].copy_from_slice(&h.to_be_bytes());
        cid[8..].copy_from_slice(&h.rotate_left(29).to_be_bytes());
        Ok(Self {
            sock,
            seq: HashMap::new(),
            cid,
            source_name: source_name.into(),
        })
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.sock.local_addr().ok()
    }
}

impl DmxOutput for DmxSender {
    fn send_universe(&mut self, to: &DmxTarget, universe: u16, data: &[u8]) -> Result<()> {
        let addr = to.addr(universe)?;
        let seq = self.seq.entry((to.protocol, universe)).or_insert(0);
        // Art-Net: 0 = sequencing off, so count 1..=255.
        *seq = seq.wrapping_add(1);
        if to.protocol == Protocol::ArtNet && *seq == 0 {
            *seq = 1;
        }
        let pkt = match to.protocol {
            Protocol::ArtNet => artnet_dmx(*seq, universe & 0x7fff, data),
            Protocol::Sacn => sacn_dmx(&self.cid, &self.source_name, *seq, universe.max(1), 100, data),
        };
        self.sock.send_to(&pkt, addr)?;
        Ok(())
    }
}

/// One universe heard by a [`DmxReceiver`].
#[derive(Debug, Clone)]
pub struct Heard {
    pub data: [u8; UNIVERSE],
    pub at: Instant,
    pub from: SocketAddr,
}

#[derive(Default)]
struct Shared {
    universes: HashMap<u16, Heard>,
    /// (universe, 1-based channel, value) since the last `drain_changes`.
    changes: Vec<(u16, u16, u8)>,
}

/// Listens for ArtDmx (and answers ArtPoll with the node name) on a thread.
pub struct DmxReceiver {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub port: u16,
}

impl DmxReceiver {
    /// Listen on `bind`:`port` (Art-Net: 6454; 0 = any free port for tests).
    pub fn start(bind: Option<Ipv4Addr>, port: u16, node_name: &str) -> Result<Self> {
        let sock = UdpSocket::bind((bind.unwrap_or(Ipv4Addr::UNSPECIFIED), port))?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let _ = sock.set_broadcast(true);
        let port = sock.local_addr()?.port();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st, name) = (shared.clone(), stop.clone(), node_name.to_owned());
        let reply_ip = bind.unwrap_or(Ipv4Addr::LOCALHOST);
        let thread = std::thread::Builder::new().name("artnet in".into()).spawn(move || {
            let mut buf = [0u8; 1024];
            while !st.load(Ordering::Relaxed) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                let p = &buf[..n];
                if is_artnet_poll(p) {
                    let _ = sock.send_to(&artnet_poll_reply(reply_ip, &name), from);
                    continue;
                }
                let Some((u, data)) = parse_artnet_dmx(p) else { continue };
                let mut s = sh.lock().unwrap();
                let Shared { universes, changes } = &mut *s;
                let e = universes.entry(u).or_insert(Heard {
                    data: [0; UNIVERSE],
                    at: Instant::now(),
                    from,
                });
                for (i, &v) in data.iter().enumerate() {
                    if e.data[i] != v {
                        e.data[i] = v;
                        if changes.len() < 8192 {
                            changes.push((u, i as u16 + 1, v));
                        }
                    }
                }
                e.at = Instant::now();
                e.from = from;
            }
        })?;
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
            port,
        })
    }

    /// Latest channels of a universe.
    pub fn universe(&self, u: u16) -> Option<Heard> {
        self.shared.lock().unwrap().universes.get(&u).cloned()
    }

    /// Universes heard so far with the time of their last packet.
    pub fn active(&self) -> Vec<(u16, Instant)> {
        let mut v: Vec<_> = self
            .shared
            .lock()
            .unwrap()
            .universes
            .iter()
            .map(|(u, h)| (*u, h.at))
            .collect();
        v.sort_by_key(|a| a.0);
        v
    }

    /// Channel changes since the last call.
    pub fn drain_changes(&self) -> Vec<(u16, u16, u8)> {
        std::mem::take(&mut self.shared.lock().unwrap().changes)
    }
}

impl Drop for DmxReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// IPv4 addresses of this machine's network adapters (name, address), for
/// the "Network Adapter" choice. Linux: from `/proc/net` + `ip`; elsewhere
/// only the default route's address.
pub fn adapters() -> Vec<(String, Ipv4Addr)> {
    let mut out = vec![("Localhost".to_string(), Ipv4Addr::LOCALHOST)];
    #[cfg(target_os = "linux")]
    if let Ok(o) = std::process::Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .output()
    {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            // "2: enp3s0    inet 192.168.1.5/24 brd … scope global …"
            let f: Vec<&str> = line.split_whitespace().collect();
            if let (Some(name), Some(i)) = (f.get(1), f.iter().position(|w| *w == "inet"))
                && let Some(ip) = f
                    .get(i + 1)
                    .and_then(|a| a.split('/').next())
                    .and_then(|a| a.parse().ok())
                && ip != Ipv4Addr::LOCALHOST
            {
                out.push((name.to_string(), ip));
            }
        }
    }
    // Fallback: the address used for outgoing traffic (no packet is sent).
    if out.len() == 1
        && let Ok(s) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        && s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).is_ok()
        && let Ok(SocketAddr::V4(a)) = s.local_addr()
    {
        out.push(("Default".into(), *a.ip()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artnet_packet_layout() {
        let p = artnet_dmx(7, 0x123, &[1, 2, 3]);
        assert_eq!(&p[..8], b"Art-Net\0");
        assert_eq!(&p[8..12], [0x00, 0x50, 0, 14]);
        assert_eq!(p[12], 7);
        assert_eq!((p[14], p[15]), (0x23, 0x01));
        assert_eq!(&p[16..18], [0, 4]); // padded to even
        assert_eq!(&p[18..], [1, 2, 3, 0]);
        let (a, d) = parse_artnet_dmx(&p).unwrap();
        assert_eq!((a, d), (0x123, &[1u8, 2, 3, 0][..]));
    }

    #[test]
    fn sacn_packet_layout() {
        let data = [9u8; 512];
        let p = sacn_dmx(&[1; 16], "LAVMFX", 3, 7, 100, &data);
        assert_eq!(p.len(), 638);
        assert_eq!(&p[16..18], [0x72, 0x6e]); // flags + 622
        assert_eq!(&p[38..40], [0x72, 0x58]); // 600
        assert_eq!(&p[115..117], [0x72, 0x0b]); // 523
        assert_eq!(&p[44..50], b"LAVMFX");
        assert_eq!((p[108], p[111]), (100, 3));
        assert_eq!(&p[123..125], [0x02, 0x01]); // 513 values
        let (u, d) = parse_sacn_dmx(&p).unwrap();
        assert_eq!((u, d.len(), d[0]), (7, 512, 9));
        assert_eq!(sacn_multicast(0x0102), Ipv4Addr::new(239, 255, 1, 2));
    }

    #[test]
    fn sends_and_receives_on_loopback() {
        let rx = DmxReceiver::start(Some(Ipv4Addr::LOCALHOST), 0, "Test node").unwrap();
        let mut tx = DmxSender::new(Some(Ipv4Addr::LOCALHOST), "Test").unwrap();
        let data: Vec<u8> = (0..512).map(|i| i as u8).collect();
        // The receiver listens on a test port: send a raw packet there (the
        // sender always uses the real port; tested below).
        let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        s.send_to(&artnet_dmx(1, 5, &data), (Ipv4Addr::LOCALHOST, rx.port))
            .unwrap();
        let t = Instant::now();
        while rx.universe(5).is_none() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let h = rx.universe(5).expect("universe 5 heard");
        assert_eq!(h.data[..], data[..]);
        let ch = rx.drain_changes();
        assert_eq!(ch.len(), 510); // channels 1 and 257 stay 0
        assert_eq!(ch[0], (5, 2, 1));
        assert!(rx.drain_changes().is_empty());
        assert_eq!(rx.active().len(), 1);
        // ArtPoll gets a reply with the node name.
        let mut poll = ARTNET_ID.to_vec();
        poll.extend_from_slice(&OP_POLL.to_le_bytes());
        poll.extend_from_slice(&[0, 14, 0, 0]);
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        s.send_to(&poll, (Ipv4Addr::LOCALHOST, rx.port)).unwrap();
        let mut buf = [0u8; 300];
        let (n, _) = s.recv_from(&mut buf).unwrap();
        assert_eq!(n, 239);
        assert_eq!(&buf[26..35], b"Test node");
        let bad = DmxTarget {
            protocol: Protocol::ArtNet,
            ip: "nope".into(),
        };
        assert!(tx.send_universe(&bad, 0, &data).is_err());
    }

    #[test]
    fn sender_reaches_a_listener_with_both_protocols() {
        // Listen on the real ports only if they're free (another Art-Net app
        // may hold them); skip otherwise.
        for (proto, port) in [(Protocol::ArtNet, ARTNET_PORT), (Protocol::Sacn, SACN_PORT)] {
            let Ok(l) = UdpSocket::bind((Ipv4Addr::LOCALHOST, port)) else {
                eprintln!("port {port} busy: skipped");
                continue;
            };
            l.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut tx = DmxSender::new(Some(Ipv4Addr::LOCALHOST), "Test").unwrap();
            let to = DmxTarget {
                protocol: proto,
                ip: "127.0.0.1".into(),
            };
            tx.send_universe(&to, 3, &[42; 512]).unwrap();
            let mut buf = [0u8; 700];
            let (n, _) = l.recv_from(&mut buf).unwrap();
            let (u, d) = match proto {
                Protocol::ArtNet => parse_artnet_dmx(&buf[..n]).unwrap(),
                Protocol::Sacn => parse_sacn_dmx(&buf[..n]).unwrap(),
            };
            assert_eq!((u, d.len(), d[100]), (3, 512, 42));
        }
    }
}
