//! Threaded playback: a decode thread fills a small frame queue; the UI asks
//! for "the frame at clip time t" each render tick.
//!
//! Positions are *unwrapped* seconds: `pos = lap * duration + offset`, so
//! looping needs no special handling on the consumer side. Each frame is
//! tagged with the seek generation and lap it was decoded for; stale frames
//! (from before a scrub) are dropped.
//!
//! The first frame after a seek is shown at once, even when its time is
//! slightly past the playhead (playing backward, the playhead never gets
//! there). Soft seeks ([`Player::seek_soft`], one per frame when playing
//! backward/random) don't drop frames of earlier soft seeks: when decoding a
//! seek takes longer than a frame, the newest finished one is still shown.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{MediaInfo, OpenOptions, Result, VideoDecoder, VideoFrame};

const QUEUE: usize = 4;

enum Cmd {
    Seek { generation: u64, pos: f64 },
}

struct Tagged {
    generation: u64,
    lap: i64,
    frame: VideoFrame,
}

pub struct Player {
    info: MediaInfo,
    cmd: Option<mpsc::Sender<Cmd>>,
    frames: Receiver<Tagged>,
    generation: u64,
    /// Generation of the last hard seek: older frames are dropped.
    hard_generation: u64,
    /// Generation of the frame shown last.
    shown_generation: u64,
    next: Option<Tagged>,
    thread: Option<JoinHandle<()>>,
}

fn split(pos: f64, dur: f64) -> (i64, f64) {
    if dur <= 0.0 {
        return (0, pos.max(0.0));
    }
    let lap = (pos / dur).floor();
    (lap as i64, (pos - lap * dur).clamp(0.0, dur))
}

impl Player {
    pub fn open(path: &std::path::Path, opts: OpenOptions) -> Result<Self> {
        Ok(Self::new(crate::open(path, opts)?))
    }

    pub fn new(decoder: Box<dyn VideoDecoder>) -> Self {
        let info = decoder.info().clone();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(QUEUE);
        let name = format!("decode {}", info.path.file_name().unwrap_or_default().to_string_lossy());
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || decode_loop(decoder, cmd_rx, frame_tx))
            .expect("spawn decode thread");
        Self {
            info,
            cmd: Some(cmd_tx),
            frames: frame_rx,
            generation: 0,
            hard_generation: 0,
            shown_generation: 0,
            next: None,
            thread: Some(thread),
        }
    }

    pub fn info(&self) -> &MediaInfo {
        &self.info
    }

    pub fn duration(&self) -> f64 {
        self.info.duration.as_secs_f64()
    }

    /// Jump to unwrapped position `pos` (seconds). Used for scrubbing and cueing.
    pub fn seek(&mut self, pos: f64) {
        self.send_seek(pos);
        self.hard_generation = self.generation;
    }

    /// Like [`Self::seek`], but frames of earlier soft seeks still arriving
    /// may be shown (backward / random playback).
    pub fn seek_soft(&mut self, pos: f64) {
        self.send_seek(pos);
    }

    /// A seek was sent whose frame hasn't been shown yet.
    pub fn seek_pending(&self) -> bool {
        self.shown_generation < self.generation
    }

    fn send_seek(&mut self, pos: f64) {
        self.generation += 1;
        self.next = None;
        if let Some(tx) = &self.cmd {
            let _ = tx.send(Cmd::Seek {
                generation: self.generation,
                pos,
            });
        }
    }

    /// Latest frame due at unwrapped position `pos`, if it differs from the
    /// last one returned. Never blocks.
    pub fn update(&mut self, pos: f64) -> Option<VideoFrame> {
        let (lap, offset) = split(pos, self.duration());
        let mut out = None;
        loop {
            let t = match self.next.take() {
                Some(t) => t,
                None => match self.frames.try_recv() {
                    Ok(t) => t,
                    Err(_) => break,
                },
            };
            if t.generation < self.hard_generation {
                continue;
            }
            if t.generation != self.generation {
                // An earlier soft seek's result: better than nothing.
                if t.generation > self.shown_generation {
                    self.shown_generation = t.generation;
                    out = Some(t.frame);
                }
                continue;
            }
            let fresh = self.shown_generation < self.generation;
            let due = fresh || (t.lap, t.frame.pts.as_secs_f64()) <= (lap, offset + 1e-4);
            if due {
                self.shown_generation = self.generation;
                out = Some(t.frame);
            } else {
                self.next = Some(t);
                break;
            }
        }
        out
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.cmd = None; // disconnects: the thread exits
        while self.frames.try_recv().is_ok() {} // unblock a pending send
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn decode_loop(mut dec: Box<dyn VideoDecoder>, cmds: Receiver<Cmd>, frames: SyncSender<Tagged>) {
    let dur = dec.info().duration.as_secs_f64();
    let (mut generation, mut lap) = (0u64, 0i64);
    let mut pending: Option<Tagged> = None;
    let mut empty_laps = 0;

    loop {
        // Commands: drain everything, keep the newest seek. Wait briefly only
        // when the queue is full (pending frame waiting for room).
        let mut seek = None;
        let mut wait = pending.is_some() || empty_laps >= 2;
        loop {
            let r = if wait {
                wait = false;
                cmds.recv_timeout(Duration::from_millis(2)).map_err(|e| match e {
                    RecvTimeoutError::Timeout => TryRecvError::Empty,
                    RecvTimeoutError::Disconnected => TryRecvError::Disconnected,
                })
            } else {
                cmds.try_recv()
            };
            match r {
                Ok(Cmd::Seek { generation: g, pos }) => seek = Some((g, pos)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some((g, pos)) = seek {
            generation = g;
            let (l, offset) = split(pos, dur);
            lap = l;
            pending = None;
            empty_laps = 0;
            if let Err(e) = dec.seek(Duration::from_secs_f64(offset)) {
                log::warn!("seek: {e}");
            }
        }

        if pending.is_none() && empty_laps < 2 {
            match dec.next_frame() {
                Ok(Some(frame)) => {
                    empty_laps = 0;
                    pending = Some(Tagged { generation, lap, frame });
                }
                Ok(None) => {
                    // End of file: loop.
                    empty_laps += 1;
                    lap += 1;
                    if let Err(e) = dec.seek(Duration::ZERO) {
                        log::warn!("loop seek: {e}");
                        empty_laps = 2;
                    }
                }
                Err(e) => {
                    log::warn!("decode: {e}");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        if let Some(t) = pending.take() {
            match frames.try_send(t) {
                Ok(()) => {}
                Err(TrySendError::Full(t)) => pending = Some(t),
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorInfo, FrameFormat, Plane};

    /// 10 fps, 1 s synthetic decoder.
    struct Fake {
        info: MediaInfo,
        i: u32,
    }

    impl VideoDecoder for Fake {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn seek(&mut self, t: Duration) -> Result<()> {
            self.i = (t.as_secs_f64() * 10.0).floor() as u32;
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
            if self.i >= 10 {
                return Ok(None);
            }
            let f = VideoFrame {
                pts: Duration::from_millis(self.i as u64 * 100),
                width: 1,
                height: 1,
                format: FrameFormat::Rgba8,
                color: ColorInfo::default(),
                planes: vec![Plane {
                    data: vec![self.i as u8; 4],
                    stride: 4,
                }],
            };
            self.i += 1;
            Ok(Some(f))
        }
    }

    fn fake() -> Player {
        Player::new(Box::new(Fake {
            info: MediaInfo {
                path: "fake".into(),
                width: 1,
                height: 1,
                frame_rate: (10, 1),
                duration: Duration::from_secs(1),
                codec: "fake".into(),
                has_audio: false,
                decode_path: "test".into(),
            },
            i: 0,
        }))
    }

    /// Poll until frames stop arriving (the decode thread is asynchronous
    /// and only QUEUE frames ahead), return the last one.
    fn frame_at(p: &mut Player, pos: f64) -> u8 {
        let mut last = None;
        let mut idle = 0;
        for _ in 0..1000 {
            match p.update(pos) {
                Some(f) => {
                    last = Some(f.planes[0].data[0]);
                    idle = 0;
                }
                None if last.is_some() => {
                    idle += 1;
                    if idle > 20 {
                        break;
                    }
                }
                None => {}
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        last.unwrap_or_else(|| panic!("no frame at {pos}"))
    }

    #[test]
    fn plays_loops_and_scrubs() {
        let mut p = fake();
        assert_eq!(frame_at(&mut p, 0.0), 0);
        assert_eq!(frame_at(&mut p, 0.35), 3);
        assert_eq!(frame_at(&mut p, 0.99), 9);
        assert_eq!(frame_at(&mut p, 1.25), 2); // second lap
        p.seek(0.71);
        assert_eq!(frame_at(&mut p, 0.71), 7);
        p.seek(3.5); // scrub into lap 3
        assert_eq!(frame_at(&mut p, 3.5), 5);
        assert_eq!(frame_at(&mut p, 4.05), 0);
    }

    #[test]
    fn split_wraps() {
        assert_eq!(split(2.5, 1.0), (2, 0.5));
        assert_eq!(split(0.2, 0.0), (0, 0.2));
    }
}
