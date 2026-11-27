//! Live video in and out shared by NDI (`ndi.rs`) and PipeWire / Spout /
//! Syphon (`share.rs`): a receiver = newest frame + status, filled by a
//! protocol thread and read by any number of players; an output = a sink
//! the composition readback hands RGBA frames to.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::{ColorInfo, FrameFormat, Plane, VideoFrame, YuvMatrix};

/// What a receiver shows in the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecvStatus {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// A frame arrived in the last second.
    pub live: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Slot {
    frame: Option<Arc<VideoFrame>>,
    /// Bumped per frame.
    seq: u64,
    status: RecvStatus,
    last: Option<Instant>,
}

pub(crate) type Shared = Arc<Mutex<Slot>>;

/// A frame in, from the protocol thread. `fps` ≤ 0 = unknown: measured.
pub(crate) fn publish(s: &Shared, f: VideoFrame, fps: f64) {
    let mut s = s.lock().unwrap();
    let fps = if fps > 0.0 {
        fps
    } else {
        match s.last.map(|t| t.elapsed().as_secs_f64()) {
            Some(dt) if dt > 0.0 && s.status.fps > 0.0 => s.status.fps * 0.9 + 0.1 / dt,
            Some(dt) if dt > 0.0 => 1.0 / dt,
            _ => 0.0,
        }
    };
    s.status = RecvStatus {
        width: f.width,
        height: f.height,
        fps,
        live: true,
        error: None,
    };
    s.frame = Some(Arc::new(f));
    s.seq += 1;
    s.last = Some(Instant::now());
}

pub(crate) fn set_error(s: &Shared, e: impl Into<String>) {
    s.lock().unwrap().status.error = Some(e.into());
}

/// One live source being received (shared by everything showing it).
pub struct Receiver {
    pub name: String,
    shared: Shared,
    /// Stops the protocol side (joins its thread / tells its loop).
    stop: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Receiver {
    pub(crate) fn new(name: &str, shared: Shared, stop: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            name: name.to_owned(),
            shared,
            stop: Some(Box::new(stop)),
        }
    }

    /// The newest frame if it's newer than `seen` (updates `seen`).
    pub fn latest(&self, seen: &mut u64) -> Option<Arc<VideoFrame>> {
        let s = self.shared.lock().unwrap();
        if s.seq == *seen {
            return None;
        }
        *seen = s.seq;
        s.frame.clone()
    }

    pub fn status(&self) -> RecvStatus {
        let s = self.shared.lock().unwrap();
        let mut st = s.status.clone();
        st.live = s.last.is_some_and(|t| t.elapsed() < Duration::from_secs(1));
        st
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        if let Some(f) = self.stop.take() {
            f();
        }
    }
}

/// The running receiver for `key` or a new one from `make` (players and the
/// preview reading the same source share it; it stops when the last drops).
pub(crate) fn pooled(key: &str, make: impl FnOnce() -> Result<Arc<Receiver>, String>) -> Result<Arc<Receiver>, String> {
    static POOL: Mutex<Option<HashMap<String, Weak<Receiver>>>> = Mutex::new(None);
    let mut pool = POOL.lock().unwrap();
    let map = pool.get_or_insert_with(HashMap::new);
    map.retain(|_, w| w.strong_count() > 0);
    if let Some(r) = map.get(key).and_then(Weak::upgrade) {
        return Ok(r);
    }
    let r = make()?;
    map.insert(key.to_owned(), Arc::downgrade(&r));
    Ok(r)
}

/// Byte order of 4-byte pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pixel {
    Rgba,
    /// Fourth byte undefined: made opaque.
    Rgbx,
    Bgra,
    Bgrx,
}

/// Rows of `stride` bytes → tight RGBA.
pub fn to_rgba(src: &[u8], w: usize, h: usize, stride: usize, px: Pixel) -> Option<Vec<u8>> {
    let (bgr, opaque) = match px {
        Pixel::Rgba => (false, false),
        Pixel::Rgbx => (false, true),
        Pixel::Bgra => (true, false),
        Pixel::Bgrx => (true, true),
    };
    if w == 0 || h == 0 || stride < w * 4 || src.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &src[y * stride..y * stride + w * 4];
        if !bgr && !opaque {
            out.extend_from_slice(row);
            continue;
        }
        for p in row.as_chunks::<4>().0 {
            let (r, b) = if bgr { (p[2], p[0]) } else { (p[0], p[2]) };
            out.extend_from_slice(&[r, p[1], b, if opaque { 255 } else { p[3] }]);
        }
    }
    Some(out)
}

/// YUY2 (Y0 U Y1 V, BT.601 limited, cameras) rows → tight RGBA.
pub fn yuy2_to_rgba(src: &[u8], w: usize, h: usize, stride: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || stride < w * 2 || src.len() < stride * (h - 1) + w * 2 {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    let px = |y: u8, u: u8, v: u8| {
        let (c, d, e) = (f32::from(y) - 16.0, f32::from(u) - 128.0, f32::from(v) - 128.0);
        let k = |x: f32| x.round().clamp(0.0, 255.0) as u8;
        [
            k(1.164 * c + 1.596 * e),
            k(1.164 * c - 0.392 * d - 0.813 * e),
            k(1.164 * c + 2.017 * d),
            255,
        ]
    };
    for y in 0..h {
        let row = &src[y * stride..y * stride + w * 2];
        for (i, q) in row.as_chunks::<4>().0.iter().enumerate() {
            out.extend_from_slice(&px(q[0], q[1], q[3]));
            if 2 * i + 1 < w {
                out.extend_from_slice(&px(q[2], q[1], q[3]));
            }
        }
        if w % 2 == 1 {
            // Odd width: the last pixel has half a macropixel.
            let q = &row[(w - 1) * 2..];
            out.extend_from_slice(&px(q[0], q[1], *row.get(w * 2 - 1).unwrap_or(&128)));
        }
    }
    Some(out)
}

/// 8-bit grey rows (infrared cameras) → tight RGBA.
pub fn gray_to_rgba(src: &[u8], w: usize, h: usize, stride: usize) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || stride < w || src.len() < stride * (h - 1) + w {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for &g in &src[y * stride..y * stride + w] {
            out.extend_from_slice(&[g, g, g, 255]);
        }
    }
    Some(out)
}

/// A tight RGBA frame.
pub fn rgba_frame(w: u32, h: u32, data: Vec<u8>) -> VideoFrame {
    VideoFrame {
        pts: Duration::ZERO,
        width: w,
        height: h,
        format: FrameFormat::Rgba8,
        color: ColorInfo {
            matrix: YuvMatrix::Bt709,
            full_range: true,
        },
        planes: vec![Plane { data, stride: w * 4 }],
    }
}

/// One frame to send: RGBA rows of `stride` bytes.
pub struct OutFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Frame rate as num / den (e.g. 60 / 1).
    pub rate: (i32, i32),
}

/// Something the composition is sent to (NDI source, PipeWire node…).
pub trait FrameSink: Send {
    /// A buffer of `len` bytes to fill (recycled when possible).
    fn buffer(&self, len: usize) -> Vec<u8>;
    /// Queue a frame; false = dropped (the sink is still busy).
    fn send(&self, f: OutFrame) -> bool;
    /// One-line state for menus.
    fn status(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_orders_become_rgba() {
        // 2×2, stride 12 (4 bytes padding per row).
        let row = [1u8, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0];
        let src: Vec<u8> = row.iter().chain(row.iter()).copied().collect();
        assert_eq!(
            to_rgba(&src, 2, 2, 12, Pixel::Rgba).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            to_rgba(&src, 2, 1, 12, Pixel::Rgbx).unwrap(),
            [1, 2, 3, 255, 5, 6, 7, 255]
        );
        assert_eq!(to_rgba(&src, 2, 1, 12, Pixel::Bgra).unwrap(), [3, 2, 1, 4, 7, 6, 5, 8]);
        assert_eq!(
            to_rgba(&src, 2, 1, 12, Pixel::Bgrx).unwrap(),
            [3, 2, 1, 255, 7, 6, 5, 255]
        );
        assert!(to_rgba(&src[..10], 2, 2, 12, Pixel::Rgba).is_none());
    }

    #[test]
    fn gray_converts() {
        assert_eq!(
            gray_to_rgba(&[10, 20, 0, 30, 40, 0], 2, 2, 3).unwrap(),
            [10, 10, 10, 255, 20, 20, 20, 255, 30, 30, 30, 255, 40, 40, 40, 255]
        );
    }

    #[test]
    fn yuy2_converts() {
        // White, black (limited range), 3 pixels wide (odd).
        let src = [235u8, 128, 16, 128, 235, 128, 0, 0];
        let out = yuy2_to_rgba(&src, 3, 1, 8).unwrap();
        assert_eq!(out.len(), 12);
        assert!(
            out[..3].iter().all(|v| *v >= 254) && out[4..7].iter().all(|v| *v <= 1),
            "{out:?}"
        );
    }
}
