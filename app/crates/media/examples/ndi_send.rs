//! `cargo run -p lavmfx-media --example ndi_send -- [name] [secs]`: send a moving
//! test pattern (colour bars + a sweeping white bar) as an NDI source, 1280×720 @ 60.
use std::time::{Duration, Instant};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "lavmfx-pattern".into());
    let secs: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20.0);
    if let Ok(dir) = std::env::var("NDI_DIR") {
        lavmfx_media::ndi::set_runtime_dir(Some(dir.into()));
    }
    let tx = lavmfx_media::ndi::Sender::new(&name).expect("NDI sender");
    let (w, h) = (1280u32, 720u32);
    const BARS: [[u8; 3]; 7] = [
        [255, 255, 255],
        [255, 255, 0],
        [0, 255, 255],
        [0, 255, 0],
        [255, 0, 255],
        [255, 0, 0],
        [0, 0, 255],
    ];
    let t0 = Instant::now();
    let mut n = 0u64;
    while t0.elapsed().as_secs_f64() < secs {
        let sweep = (n * 8 % u64::from(w)) as u32;
        let mut data = tx.buffer((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let c = if x.abs_diff(sweep) < 6 {
                    [255, 255, 255]
                } else {
                    BARS[(x * 7 / w) as usize]
                };
                let c = if y > h * 3 / 4 {
                    [c[0] / 4, c[1] / 4, c[2] / 4]
                } else {
                    c
                };
                data[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        tx.send(lavmfx_media::ndi::OutFrame {
            data,
            width: w,
            height: h,
            stride: w * 4,
            rate: (60, 1),
        });
        n += 1;
        let next = t0 + Duration::from_secs_f64(n as f64 / 60.0);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    println!(
        "sent {} frames, {} receivers at the end",
        tx.frames_sent(),
        tx.connections()
    );
}
