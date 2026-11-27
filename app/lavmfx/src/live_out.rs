//! Composition outputs to other software: NDI (network) and shared video
//! (PipeWire / Spout / Syphon). The composition texture is read back from the
//! GPU once (a small ring of mapped buffers, never waiting on the GPU) and
//! handed to every sink that's on; a sink still busy drops the frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use eframe::wgpu;
use lavmfx_media::live::{FrameSink, OutFrame};

const FREE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const RING: usize = 3;

struct Slot {
    buf: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// Frame number it holds (newest mapped one is sent).
    frame: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ndi,
    Share,
}

/// One output (NDI source / PipeWire node…).
struct Out {
    kind: Kind,
    sink: Option<Box<dyn FrameSink>>,
    /// Wanted name (None = off) and size it was made for (shared video is sized).
    want: Option<String>,
    made_for: (u32, u32),
    error: Option<String>,
    retry_at: Option<Instant>,
}

impl Out {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            sink: None,
            want: None,
            made_for: (0, 0),
            error: None,
            retry_at: None,
        }
    }

    fn configure(&mut self, name: Option<&str>, size: (u32, u32)) {
        let name = name.map(|n| if n.trim().is_empty() { "LAVMFX" } else { n.trim() });
        let resized = self.kind == Kind::Share && self.made_for != size;
        if self.want.as_deref() != name || resized {
            self.want = name.map(str::to_owned);
            self.sink = None;
            self.error = None;
            self.retry_at = None;
        }
        let Some(name) = name else { return };
        if self.sink.is_some() || self.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let made = match self.kind {
            Kind::Ndi => lavmfx_media::ndi::Sender::new(name).map(|s| Box::new(s) as Box<dyn FrameSink>),
            Kind::Share => lavmfx_media::share::output(name, size.0, size.1),
        };
        match made {
            Ok(s) => {
                log::info!("{:?} output \"{name}\" on", self.kind);
                self.sink = Some(s);
                self.made_for = size;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.retry_at = Some(Instant::now() + Duration::from_secs(5));
            }
        }
    }

    fn status(&self) -> String {
        match (&self.sink, &self.error, &self.want) {
            (Some(s), _, _) => s.status(),
            (None, Some(e), _) => e.clone(),
            (None, None, Some(_)) => "Starting…".into(),
            (None, None, None) => "Off".into(),
        }
    }
}

pub struct LiveOut {
    outs: [Out; 2],
    /// Composition recording (Record button), fed like the other outputs.
    pub record: Option<lavmfx_media::record::Recorder>,
    /// DMX pixel mapping (on while lumiverses exist).
    pub dmx: Option<crate::dmx::DmxOut>,
    ring: Vec<Slot>,
    size: (u32, u32),
    frame: u64,
}

/// Bytes per row of a readback (wgpu wants multiples of 256).
fn padded_row(w: u32) -> u32 {
    (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

impl LiveOut {
    pub fn new() -> Self {
        Self {
            outs: [Out::new(Kind::Ndi), Out::new(Kind::Share)],
            record: None,
            dmx: None,
            ring: Vec::new(),
            size: (0, 0),
            frame: 0,
        }
    }

    fn out(&self, k: Kind) -> &Out {
        &self.outs[k as usize]
    }

    /// Wanted outputs (None = off). Sinks are (re)made on name / size changes;
    /// failed ones retry every 5 s.
    pub fn configure(&mut self, ndi: Option<&str>, share: Option<&str>, size: (u32, u32)) {
        self.outs[Kind::Ndi as usize].configure(ndi, size);
        self.outs[Kind::Share as usize].configure(share, size);
    }

    /// One-line state for menus / Preferences.
    pub fn status(&self, k: Kind) -> String {
        self.out(k).status()
    }

    /// After the composition rendered: hand finished readbacks to the sinks and
    /// start one for `tex`.
    pub fn tick(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture) {
        if self.outs.iter().all(|o| o.sink.is_none()) && self.record.is_none() && self.dmx.is_none() {
            self.ring.clear();
            return;
        }
        let _ = device.poll(wgpu::PollType::Poll);
        let (w, h) = (tex.width(), tex.height());
        let row = padded_row(w);
        // Send the newest mapped frame; release the others.
        let newest = self
            .ring
            .iter()
            .enumerate()
            .filter(|(_, s)| s.state.load(Ordering::Acquire) == MAPPED)
            .max_by_key(|(_, s)| s.frame)
            .map(|(i, _)| i);
        for (i, s) in self.ring.iter().enumerate() {
            if s.state.load(Ordering::Acquire) != MAPPED {
                continue;
            }
            if Some(i) == newest
                && self.size == (w, h)
                && let Ok(view) = s.buf.slice(..).get_mapped_range()
            {
                let rec = self.record.as_ref().map(|r| r as &dyn FrameSink);
                let dmx = self.dmx.as_ref().map(|d| d as &dyn FrameSink);
                for sink in self.outs.iter().filter_map(|o| o.sink.as_deref()).chain(rec).chain(dmx) {
                    let mut data = sink.buffer(view.len());
                    data.copy_from_slice(&view);
                    sink.send(OutFrame {
                        data,
                        width: w,
                        height: h,
                        stride: row,
                        rate: (60, 1),
                    });
                }
            }
            s.buf.unmap();
            s.state.store(FREE, Ordering::Release);
        }
        // (Re)build the ring for this size.
        if self.size != (w, h) || self.ring.is_empty() {
            if self.ring.iter().any(|s| s.state.load(Ordering::Acquire) == PENDING) {
                return; // wait for the old buffers
            }
            self.size = (w, h);
            self.ring = (0..RING)
                .map(|_| Slot {
                    buf: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("live out readback"),
                        size: u64::from(row) * u64::from(h),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(FREE)),
                    frame: 0,
                })
                .collect();
        }
        let Some(slot) = self.ring.iter_mut().find(|s| s.state.load(Ordering::Acquire) == FREE) else {
            return; // GPU behind: skip this frame
        };
        self.frame += 1;
        slot.frame = self.frame;
        slot.state.store(PENDING, Ordering::Release);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("live out readback"),
        });
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            tex.size(),
        );
        queue.submit([enc.finish()]);
        let state = slot.state.clone();
        slot.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            state.store(if r.is_ok() { MAPPED } else { FREE }, Ordering::Release);
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rows_are_padded_for_wgpu() {
        assert_eq!(super::padded_row(1920), 7680);
        assert_eq!(super::padded_row(1000), 4096);
        assert_eq!(super::padded_row(1), 256);
    }
}
