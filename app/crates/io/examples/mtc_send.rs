//! MIDI Timecode test sender (Linux / macOS: a virtual MIDI port named "MTC Test").
//! `cargo run -p lavmfx-io --example mtc_send -- [start HH:MM:SS:FF] [fps 24|25|29.97|30] [seconds]`
//! Sends a full-frame locate, then running quarter frames in real time.

#[cfg(unix)]
fn main() {
    use std::time::{Duration, Instant};

    use lavmfx_core::timecode::{Fps, Timecode, mtc_quarter_frames};
    use midir::MidiOutput;
    use midir::os::unix::VirtualOutput;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let start = args
        .first()
        .and_then(|s| Timecode::parse(s))
        .unwrap_or(Timecode::new(1, 0, 0, 0));
    let fps = match args.get(1).map(String::as_str) {
        Some("24") => Fps::F24,
        Some("29.97") => Fps::F2997Df,
        Some("30") => Fps::F30,
        _ => Fps::F25,
    };
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60.0);
    let out = MidiOutput::new("MTC Test").expect("MIDI");
    let mut port = out.create_virtual("MTC Test").expect("virtual port");
    println!(
        "MTC Test port: {} at {} fps for {secs} s",
        start.display(fps),
        fps.label()
    );
    std::thread::sleep(Duration::from_secs(1)); // let receivers connect
    let h = start.h | fps.mtc_code() << 5;
    let _ = port.send(&[0xF0, 0x7F, 0x7F, 0x01, 0x01, h, start.m, start.s, start.f, 0xF7]);
    let n0 = start.frame_number(fps);
    let t0 = Instant::now();
    let qf = 1.0 / (fps.rate() * 4.0);
    let mut k: i64 = 0; // quarter frames sent
    while t0.elapsed().as_secs_f64() < secs {
        // Each 8-piece sequence spans 2 frames and carries the frame it started on.
        let tc = Timecode::from_frame_number(n0 + (k / 8) * 2, fps);
        let _ = port.send(&mtc_quarter_frames(tc, fps)[(k % 8) as usize]);
        k += 1;
        let next = t0 + Duration::from_secs_f64(k as f64 * qf);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("mtc_send needs virtual MIDI ports (Linux / macOS); use a loopMIDI port and any MTC sender on Windows.");
}
