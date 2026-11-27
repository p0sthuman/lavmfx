//! Test peer for Ableton Link: `cargo run -p lavmfx-io --example link_peer -- 133 10`
//! joins the session at that tempo for that many seconds, printing peers and beat.
use lavmfx_io::link::Link;

fn main() {
    let mut a = std::env::args().skip(1);
    let bpm: f64 = a.next().and_then(|v| v.parse().ok()).unwrap_or(133.0);
    let secs: u64 = a.next().and_then(|v| v.parse().ok()).unwrap_or(10);
    let mut l = Link::new(bpm);
    l.set_enabled(true);
    l.sync(bpm);
    for _ in 0..secs * 2 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (t, b) = l.sync(f64::NAN);
        println!("peers {} tempo {t:.1} beat {b:.2}", l.peers());
    }
}
