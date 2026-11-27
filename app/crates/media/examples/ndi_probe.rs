//! `cargo run -p lavmfx-media --example ndi_probe -- [name-part] [secs]`: list NDI
//! sources, receive the first matching one and print size / fps / frames / a pixel.
use std::time::{Duration, Instant};

fn main() {
    let want = std::env::args().nth(1).unwrap_or_default();
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    if let Ok(dir) = std::env::var("NDI_DIR") {
        lavmfx_media::ndi::set_runtime_dir(Some(dir.into()));
    }
    println!("runtime: {:?}", lavmfx_media::ndi::runtime_status());
    let t0 = Instant::now();
    let name = loop {
        let list = lavmfx_media::ndi::sources();
        if let Some(n) = list.iter().find(|n| n.contains(&want)) {
            println!("sources: {list:?}");
            break n.clone();
        }
        if t0.elapsed() > Duration::from_secs(10) {
            println!("not found; sources: {list:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let rx = lavmfx_media::ndi::receiver(&name).unwrap();
    let (mut seen, mut frames) = (0, 0);
    let t1 = Instant::now();
    let mut last = None;
    while t1.elapsed() < Duration::from_secs(secs) {
        if let Some(f) = rx.latest(&mut seen) {
            frames += 1;
            last = Some(f);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let st = rx.status();
    println!(
        "{name}: {}×{} @ {:.2} fps (sender), {frames} frames in {secs} s, live {}, error {:?}",
        st.width, st.height, st.fps, st.live, st.error
    );
    if let Some(f) = last {
        let (w, h) = (f.width as usize, f.height as usize);
        let px = |x: usize, y: usize| &f.planes[0].data[(y * w + x) * 4..][..4];
        println!("centre {:?} top-left {:?}", px(w / 2, h / 2), px(4, 4));
    }
}
