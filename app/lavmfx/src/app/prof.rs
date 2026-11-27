//! Debug: `LAVMFX_PROFILE=1` times the sections of each UI frame and logs,
//! every 2 s, the frame time (avg / max) and each section's avg / max (ms).
//! `mark("name")` closes the section that ran since the previous mark.
//!
//! Always on: a watchdog thread logs when the UI thread stops reaching marks
//! for more than 2 s (a freeze), naming the last mark it passed, and logs
//! again when it moves on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Last mark the UI thread passed, and when.
static LAST: Mutex<Option<(&'static str, Instant)>> = Mutex::new(None);

fn note(name: &'static str) {
    if let Ok(mut l) = LAST.lock() {
        *l = Some((name, Instant::now()));
    }
}

/// Start the freeze watchdog (once).
pub fn start_watchdog() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("ui-watchdog".into()).spawn(|| {
            let mut reported: Option<(&'static str, Instant)> = None;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let Some((name, at)) = LAST.lock().ok().and_then(|l| *l) else {
                    continue;
                };
                let stalled = at.elapsed();
                match reported {
                    Some((n, t)) if n == name && t == at => {}
                    Some((n, t)) => {
                        log::warn!(
                            "watchdog: UI thread moved on after {:.1} s stuck after '{n}'",
                            (at - t).as_secs_f32()
                        );
                        reported = None;
                    }
                    None if stalled > Duration::from_secs(2) => {
                        log::warn!(
                            "watchdog: UI thread stuck for {:.1} s after '{name}' (in the step after that mark; normal while the window is minimized)",
                            stalled.as_secs_f32()
                        );
                        reported = Some((name, at));
                    }
                    None => {}
                }
            }
        });
    });
}

struct State {
    on: bool,
    last: Option<Instant>,
    frame_start: Option<Instant>,
    /// name → (total ms, max ms, count)
    sections: HashMap<&'static str, (f64, f64, u32)>,
    frames: (f64, f64, u32),
    since: Instant,
}

thread_local! {
    static S: RefCell<State> = RefCell::new(State {
        on: std::env::var_os("LAVMFX_PROFILE").is_some(),
        last: None,
        frame_start: None,
        sections: HashMap::new(),
        frames: (0.0, 0.0, 0),
        since: Instant::now(),
    });
}

/// Start of a frame (also ends the previous frame).
pub fn frame() {
    note("frame start");
    S.with_borrow_mut(|s| {
        if !s.on {
            return;
        }
        let now = Instant::now();
        if let Some(f) = s.frame_start {
            let ms = (now - f).as_secs_f64() * 1000.0;
            s.frames.0 += ms;
            s.frames.1 = s.frames.1.max(ms);
            s.frames.2 += 1;
        }
        s.frame_start = Some(now);
        s.last = Some(now);
        if s.since.elapsed().as_secs_f32() >= 2.0 && s.frames.2 > 0 {
            let (t, m, n) = s.frames;
            let mut v: Vec<_> = s.sections.iter().map(|(k, &(t, m, n))| (*k, t / n as f64, m)).collect();
            v.sort_by(|a, b| b.2.total_cmp(&a.2));
            let top: Vec<String> = v.iter().take(8).map(|(k, a, m)| format!("{k} {a:.1}/{m:.1}")).collect();
            log::warn!(
                "profile: frame avg {:.1} max {:.1} ms ({n} frames) | {}",
                t / n as f64,
                m,
                top.join(" · ")
            );
            s.sections.clear();
            s.frames = (0.0, 0.0, 0);
            s.since = Instant::now();
        }
    });
}

/// End of the section `name` (time since the previous mark / frame start).
pub fn mark(name: &'static str) {
    note(name);
    S.with_borrow_mut(|s| {
        if !s.on {
            return;
        }
        let now = Instant::now();
        if let Some(l) = s.last {
            let ms = (now - l).as_secs_f64() * 1000.0;
            let e = s.sections.entry(name).or_insert((0.0, 0.0, 0));
            e.0 += ms;
            e.1 = e.1.max(ms);
            e.2 += 1;
        }
        s.last = Some(now);
    });
}
