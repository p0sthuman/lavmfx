//! `cargo run -p lavmfx-audio --example listen [device-id]`: list devices, then
//! print the low/mid/high levels of the input and any LTC timecode on
//! channel 1 for 3 seconds.
fn main() {
    let d = lavmfx_audio::list();
    println!("host {}", d.host);
    for i in &d.inputs {
        println!("in  {} | {} ({} ch, {} Hz)", i.id, i.name, i.channels, i.sample_rate);
    }
    for o in &d.outputs {
        println!("out {} | {} ({} ch, {} Hz)", o.id, o.name, o.channels, o.sample_rate);
    }
    println!("default in {:?}\ndefault out {:?}", d.default_input, d.default_output);
    let tap = lavmfx_audio::InputTap::open(&lavmfx_audio::TapConfig {
        device: std::env::args().nth(1),
        fft: Some(None),
        ltc: [Some(0), None],
        ltc_fps: Default::default(),
        gain: 1.0,
        noise: 0.0,
        buffer: 480,
    })
    .expect("open input");
    for _ in 0..6 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let s = tap.spectrum.load();
        let max = |r: std::ops::Range<usize>| s[r].iter().copied().fold(0.0f32, f32::max);
        let tc = tap.timecode[0]
            .load()
            .map(|r| lavmfx_core::timecode::Timecode::from_seconds(r.secs, r.fps).display(r.fps));
        println!(
            "#{:4} L {:.2} M {:.2} H {:.2} · LTC {}",
            tap.spectrum.count(),
            max(0..12),
            max(12..24),
            max(24..32),
            tc.as_deref().unwrap_or("none")
        );
    }
}
