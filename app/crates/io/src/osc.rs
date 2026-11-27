//! OSC over UDP (the composition's OSC map: `/composition/…`, floats normalised 0..1).

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use lavmfx_core::control::Value;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};

use crate::{IoError, Result};

/// One received message (bundles are flattened).
#[derive(Debug, Clone)]
pub struct OscIn {
    pub address: String,
    /// First argument; no argument = [`Value::Trigger`].
    pub value: Value,
    /// The argument was the string `"?"`: a query for the current value.
    pub query: bool,
    /// All arguments (`value` is the first, or the last for `/lavmfx/…` buttons with pre-arguments).
    pub args: Vec<Value>,
    pub from: SocketAddr,
}

/// Listens on a UDP port in its own thread.
pub struct OscInput {
    rx: Receiver<OscIn>,
    stop: Arc<AtomicBool>,
    pub port: u16,
}

impl OscInput {
    pub fn start(port: u16) -> Result<Self> {
        let sock = UdpSocket::bind(("0.0.0.0", port))?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let quit = stop.clone();
        std::thread::Builder::new().name("osc-in".into()).spawn(move || {
            let mut buf = vec![0u8; rosc::decoder::MTU];
            while !quit.load(Ordering::Relaxed) {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..n]) else {
                    log::debug!("osc: undecodable packet from {from}");
                    continue;
                };
                let mut out = Vec::new();
                flatten(packet, &mut out);
                for m in out {
                    let (value, query) = convert(&m.args);
                    let args = m.args.iter().map(|a| convert(std::slice::from_ref(a)).0).collect();
                    let msg = OscIn {
                        address: m.addr,
                        value,
                        query,
                        args,
                        from,
                    };
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            }
        })?;
        Ok(Self { rx, stop, port })
    }

    /// Messages received since the last call.
    pub fn poll(&self) -> Vec<OscIn> {
        self.rx.try_iter().collect()
    }
}

impl Drop for OscInput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn flatten(p: OscPacket, out: &mut Vec<OscMessage>) {
    match p {
        OscPacket::Message(m) => out.push(m),
        OscPacket::Bundle(b) => b.content.into_iter().for_each(|p| flatten(p, out)),
    }
}

fn convert(args: &[OscType]) -> (Value, bool) {
    let v = match args.first() {
        None | Some(OscType::Nil) | Some(OscType::Inf) => Value::Trigger,
        Some(OscType::Float(f)) => Value::Float(*f as f64),
        Some(OscType::Double(f)) => Value::Float(*f),
        Some(OscType::Int(i)) => Value::Int(*i as i64),
        Some(OscType::Long(i)) => Value::Int(*i),
        Some(OscType::Bool(b)) => Value::Bool(*b),
        Some(OscType::String(s)) if s == "?" => return (Value::Trigger, true),
        Some(OscType::String(s)) => Value::Str(s.clone()),
        Some(_) => Value::Trigger,
    };
    (v, false)
}

/// Sends to one target; with `bundles`, messages queue until [`flush`](Self::flush).
pub struct OscOutput {
    sock: UdpSocket,
    pub target: SocketAddr,
    pub bundles: bool,
    queue: Vec<OscPacket>,
}

impl OscOutput {
    pub fn new(host: &str, port: u16, bundles: bool) -> Result<Self> {
        let target = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| IoError::Other(format!("can't resolve {host}")))?;
        let sock = UdpSocket::bind(("0.0.0.0", 0))?;
        Ok(Self {
            sock,
            target,
            bundles,
            queue: Vec::new(),
        })
    }

    pub fn send(&mut self, address: &str, v: &Value) {
        self.send_to(address, v, None);
    }

    /// Send to `to` instead of the target (query replies without an output).
    pub fn send_to(&mut self, address: &str, v: &Value, to: Option<SocketAddr>) {
        let args = match v {
            Value::Trigger => vec![],
            Value::Float(f) => vec![OscType::Float(*f as f32)],
            Value::Int(i) => vec![OscType::Int(*i as i32)],
            Value::Bool(b) => vec![OscType::Int(*b as i32)],
            Value::Str(s) => vec![OscType::String(s.clone())],
            Value::Step(d) => vec![OscType::Float(*d as f32)],
        };
        let p = OscPacket::Message(OscMessage {
            addr: address.to_owned(),
            args,
        });
        if self.bundles && to.is_none() {
            self.queue.push(p);
        } else {
            self.write(&p, to.unwrap_or(self.target));
        }
    }

    pub fn flush(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        // Keep bundles under a typical MTU.
        let all = std::mem::take(&mut self.queue);
        for chunk in all.chunks(16) {
            let b = OscPacket::Bundle(OscBundle {
                timetag: OscTime {
                    seconds: 0,
                    fractional: 1,
                },
                content: chunk.to_vec(),
            });
            self.write(&b, self.target);
        }
    }

    fn write(&self, p: &OscPacket, to: SocketAddr) {
        match rosc::encoder::encode(p) {
            Ok(bytes) => {
                let _ = self.sock.send_to(&bytes, to);
            }
            Err(e) => log::debug!("osc encode: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback() {
        let port = 47_311;
        let input = OscInput::start(port).unwrap();
        let mut out = OscOutput::new("127.0.0.1", port, true).unwrap();
        out.send("/composition/master", &Value::Float(0.5));
        out.send("/composition/columns/1/connect", &Value::Trigger);
        out.send("/composition/master", &Value::Str("?".into()));
        out.flush();
        let mut got = Vec::new();
        for _ in 0..50 {
            got.extend(input.poll());
            if got.len() >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].value, Value::Float(0.5));
        assert_eq!(got[1].value, Value::Trigger);
        assert!(got[2].query);
    }
}
