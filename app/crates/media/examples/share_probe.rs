//! `cargo run -p lavmfx-media --example share_probe -- [name-part] [secs]`: list
//! shared video sources (PipeWire / Spout / Syphon), receive the first match and
//! print size / fps / frames / pixels. `share_probe --send [secs]` publishes a test
//! pattern as "LAVMFX Probe" instead.
use std::time::{Duration, Instant};

use lavmfx_media::share;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if let Some(e) = share::unavailable() {
        println!("{}: {e}", share::backend());
        return;
    }
    if a.first().map(String::as_str) == Some("--send") {
        let secs: f64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(20.0);
        let (w, h) = (640u32, 360u32);
        let out = share::output("LAVMFX Probe", w, h).unwrap();
        let t0 = Instant::now();
        let mut n = 0u32;
        while t0.elapsed().as_secs_f64() < secs {
            let mut data = out.buffer((w * h * 4) as usize);
            for (i, p) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = i as u32 % w;
                *p = if x.abs_diff(n * 4 % w) < 4 {
                    [255, 255, 255, 255]
                } else {
                    [(x * 255 / w) as u8, 60, 200, 255]
                };
            }
            out.send(share::OutFrame {
                data,
                width: w,
                height: h,
                stride: w * 4,
                rate: (60, 1),
            });
            n += 1;
            std::thread::sleep(Duration::from_millis(16));
        }
        println!("{}", out.status());
        return;
    }
    // `--capture <device id> [secs]`: a capture device (webcam) opened directly.
    if a.first().map(String::as_str) == Some("--capture") {
        println!("devices: {:?}", lavmfx_media::capture::devices());
        let id = a.get(1).cloned().unwrap_or_else(|| "/dev/video0".into());
        let secs: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
        return show(&id, lavmfx_media::capture::receiver(&id), secs);
    }
    let want = a.first().cloned().unwrap_or_default();
    let secs: u64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    std::thread::sleep(Duration::from_millis(300));
    let list = share::sources();
    println!(
        "sources: {:?}",
        list.iter()
            .map(|s| format!("{} [{}]", s.label, s.name))
            .collect::<Vec<_>>()
    );
    let name = list
        .iter()
        .find(|s| s.label.contains(&want) || s.name.contains(&want))
        .map_or(want.clone(), |s| s.name.clone());
    show(&name, share::receiver(&name), secs);
}

fn show(name: &str, rx: Result<std::sync::Arc<lavmfx_media::live::Receiver>, String>, secs: u64) {
    let rx = match rx {
        Ok(r) => r,
        Err(e) => return println!("{e}"),
    };
    let (mut seen, mut frames, mut last) = (0, 0, None);
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(secs) {
        if let Some(f) = rx.latest(&mut seen) {
            frames += 1;
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let st = rx.status();
    println!(
        "{name}: {}×{} @ {:.2} fps, {frames} frames in {secs} s, live {}, error {:?}",
        st.width, st.height, st.fps, st.live, st.error
    );
    if let Some(f) = last {
        let w = f.width as usize;
        let px = |x: usize, y: usize| &f.planes[0].data[(y * w + x) * 4..][..4];
        println!("centre {:?} top-left {:?}", px(w / 2, f.height as usize / 2), px(4, 4));
        // `SAVE=file.ppm`: the last frame.
        if let Ok(path) = std::env::var("SAVE") {
            let mut out = format!("P6\n{} {}\n255\n", f.width, f.height).into_bytes();
            out.extend(
                f.planes[0]
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]]),
            );
            std::fs::write(path, out).unwrap();
        }
    }
}
