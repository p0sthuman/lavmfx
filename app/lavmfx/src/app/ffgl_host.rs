//! FFGL plugins in the app: folder scans (background), the catalogue in
//! lavmfx-core, and the renderer hook ([`lavmfx_render::ExternalFx`]) that
//! sends frames to the plugin thread (lavmfx-io `ffgl`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lavmfx_core::ffgl::{self, FfglKind, FfglParam, FfglPlugin};
use lavmfx_io::ffgl::{FfglHost, ProcessRequest};
use lavmfx_render::{EFFECT_PARAMS, EXT_KIND_BASE, ExternalFx};

/// A scan: plugins usable, and files that aren't (with why).
pub type ScanReport = (usize, Vec<(PathBuf, String)>);

static REPORT: Mutex<Option<ScanReport>> = Mutex::new(None);

pub fn last_report() -> Option<ScanReport> {
    REPORT.lock().ok().and_then(|r| r.clone())
}

pub struct Ffgl {
    host: FfglHost,
    /// Composition BPM (f32 bits) for `FF_SET_BEATINFO`.
    bpm: AtomicU32,
    /// Instances drawn recently (ended after 2 s unused).
    seen: Mutex<(HashMap<i64, Instant>, Instant)>,
}

impl Ffgl {
    /// `gpu`: the adapter name the app renders on (FFGL picks the same GPU).
    pub fn start(gpu: &str) -> Arc<Self> {
        let hint = gpu.split_whitespace().next().map(str::to_owned);
        Arc::new(Self {
            host: FfglHost::start(hint),
            bpm: AtomicU32::new(120f32.to_bits()),
            seen: Mutex::new((HashMap::new(), Instant::now())),
        })
    }

    pub fn set_bpm(&self, bpm: f64) {
        self.bpm.store((bpm as f32).to_bits(), Ordering::Relaxed);
    }

    /// Rescan `folders` on a worker (the catalogue updates when done).
    pub fn reload(self: &Arc<Self>, folders: &[String]) -> std::thread::JoinHandle<()> {
        let dirs: Vec<PathBuf> = folders.iter().filter(|f| !f.is_empty()).map(PathBuf::from).collect();
        let me = self.clone();
        std::thread::spawn(move || {
            let infos = me.host.scan(&dirs);
            let mut problems = Vec::new();
            let plugins: Vec<FfglPlugin> = infos
                .into_iter()
                .map(|i| {
                    if let Some(e) = &i.error {
                        problems.push((i.path.clone(), e.clone()));
                    }
                    FfglPlugin {
                        title: i.name,
                        path: i.path,
                        id: i.id,
                        kind: match i.kind {
                            lavmfx_io::ffgl::FfglKind::Effect => FfglKind::Effect,
                            lavmfx_io::ffgl::FfglKind::Source => FfglKind::Source,
                            lavmfx_io::ffgl::FfglKind::Mixer => FfglKind::Mixer,
                        },
                        params: i
                            .params
                            .into_iter()
                            .map(|p| FfglParam {
                                name: p.name,
                                ty: p.ty,
                                default: p.default,
                                range: p.range,
                                options: p.options,
                            })
                            .collect(),
                        error: i.error,
                    }
                })
                .collect();
            let ok = plugins.iter().filter(|p| p.runs()).count();
            log::info!("FFGL: {ok} plugins ready, {} not usable", problems.len());
            ffgl::set_plugins(plugins);
            if let Ok(mut r) = REPORT.lock() {
                *r = Some((ok, problems));
            }
        })
    }
}

impl ExternalFx for Ffgl {
    fn is_source(&self, kind: u32) -> bool {
        let i = kind.saturating_sub(EXT_KIND_BASE) as usize;
        ffgl::plugins().get(i).is_some_and(|p| p.kind == FfglKind::Source)
    }

    fn process(
        &self,
        kind: u32,
        id: i64,
        params: &[f32; EFFECT_PARAMS],
        input: Option<&[u8]>,
        width: u32,
        height: u32,
        time: f32,
        beat: f32,
    ) -> Option<Vec<u8>> {
        let plugin = kind.checked_sub(EXT_KIND_BASE)? as usize;
        // One instance per size: the main composition and the Preview Monitor run
        // the same effect at different sizes (resizing every call is slow).
        let id = id.wrapping_mul(0x9E37_79B9) ^ (i64::from(width) << 16 | i64::from(height));
        let n = ffgl::plugins().get(plugin)?.params.len().min(EFFECT_PARAMS);
        {
            let mut s = self.seen.lock().ok()?;
            s.0.insert(id, Instant::now());
            if s.1.elapsed() > Duration::from_secs(2) {
                s.1 = Instant::now();
                s.0.retain(|_, t| t.elapsed() < Duration::from_secs(2));
                self.host.retain(s.0.keys().copied().collect());
            }
        }
        self.host.process(ProcessRequest {
            plugin,
            instance: id,
            values: params[..n].to_vec(),
            input: input.map(<[u8]>::to_vec),
            width,
            height,
            time: f64::from(time),
            bpm: f32::from_bits(self.bpm.load(Ordering::Relaxed)),
            bar_phase: (beat / 4.0).fract(),
        })
    }
}
