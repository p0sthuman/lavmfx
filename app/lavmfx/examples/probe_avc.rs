//! Open every clip of an `.avc` / `.lavmfx` and decode its first frame;
//! prints failures grouped by error. `cargo run --release -p lavmfx --example probe_avc -- file.avc`

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    let file = std::env::args().nth(1).expect("usage: probe_avc file.avc");
    let comp = match lavmfx_project::avc::import(Path::new(&file)) {
        Ok(i) => i.composition,
        Err(e) => {
            let cwd = std::env::current_dir().unwrap_or_default();
            eprintln!("can't import {file}: {e}\n(relative paths start at {})", cwd.display());
            std::process::exit(1);
        }
    };
    let mut paths: Vec<String> = Vec::new();
    let mut add = |clips: &mut dyn Iterator<Item = &lavmfx_core::Clip>| {
        for c in clips {
            if let Some(p) = c.video_path().filter(|_| !c.is_missing()) {
                paths.push(p.to_owned());
            }
        }
    };
    add(&mut comp.layers.iter().flat_map(|l| &l.clips));
    for d in &comp.decks {
        if let Some(s) = &d.stash {
            add(&mut s.clips.iter().flatten());
        }
    }
    paths.sort();
    paths.dedup();
    let only = std::env::var("PROBE_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    let mut errors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ok = 0;
    let mut codecs: BTreeMap<String, usize> = BTreeMap::new();
    for p in paths.iter().take(only) {
        let (tx, rx) = mpsc::channel();
        let q = p.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<String, String> {
                let mut d = lavmfx_media::open(Path::new(&q), Default::default()).map_err(|e| format!("open: {e}"))?;
                let codec = d.info().codec.clone();
                match d.next_frame() {
                    Ok(Some(_)) => Ok(codec),
                    Ok(None) => Err(format!("no frame ({codec})")),
                    Err(e) => Err(format!("decode ({codec}): {e}")),
                }
            })();
            let _ = tx.send(r);
        });
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(c)) => {
                ok += 1;
                *codecs.entry(c).or_default() += 1;
            }
            Ok(Err(e)) => errors.entry(e).or_default().push(p.clone()),
            Err(_) => errors.entry("timeout (15 s)".into()).or_default().push(p.clone()),
        }
    }
    println!("{} files found, {ok} decode, codecs {codecs:?}", paths.len().min(only));
    for (e, ps) in &errors {
        println!("{} × {e}", ps.len());
        for p in ps.iter().take(3) {
            println!("    {p}");
        }
    }
}
