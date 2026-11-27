//! LTC test signal: `cargo run -p lavmfx-core --example ltc_wav -- out.wav [start HH:MM:SS:FF] [fps 24|25|29.97|30] [seconds]`
//! Mono 48 kHz 16-bit WAV at −12 dBFS. Play it into the audio input SMPTE listens to.

use lavmfx_core::timecode::{Fps, LtcEncoder, Timecode};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first() else {
        eprintln!("usage: ltc_wav out.wav [start HH:MM:SS:FF] [fps 24|25|29.97|30] [seconds]");
        std::process::exit(2);
    };
    let start = args
        .get(1)
        .and_then(|s| Timecode::parse(s))
        .unwrap_or(Timecode::new(1, 0, 0, 0));
    let fps = match args.get(2).map(String::as_str) {
        Some("24") => Fps::F24,
        Some("29.97") => Fps::F2997Df,
        Some("30") => Fps::F30,
        _ => Fps::F25,
    };
    let secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(60.0);
    let sr = 48_000u32;
    let mut enc = LtcEncoder::new(fps, sr, 0.25);
    let samples = enc.run(start, (secs * fps.rate()).round() as i64);
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut w = Vec::with_capacity(44 + data.len());
    w.extend(b"RIFF");
    w.extend((36 + data.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(sr.to_le_bytes());
    w.extend((sr * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(data);
    std::fs::write(out, w).expect("write wav");
    println!("{out}: {} at {} fps, {secs} s", start.display(fps), fps.label());
}
