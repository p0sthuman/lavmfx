//! ISF shaders in the app: folder (re)loading, a CPU check of every shader
//! when the set changes (errors show in the browsers and Preferences), and
//! GPU programs built the first time a shader is drawn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lavmfx_core::isf::{self, Arg, ImageRole, InputType, IsfShader, LoadReport};
use lavmfx_render::isf::{IsfArg, IsfGenerator, IsfImage, IsfPassDesc, IsfType, IsfVar};
use lavmfx_render::{EffectDraw, FxFrame, FxImage, ISF_KIND_BASE, IsfDesc, IsfLibrary};

/// Last folder scan (Preferences › Video shows it).
static REPORT: Mutex<Option<LoadReport>> = Mutex::new(None);

/// Rescan the shader folders (built-ins always come first).
pub fn reload(folders: &[String]) -> LoadReport {
    let dirs: Vec<PathBuf> = folders.iter().filter(|f| !f.is_empty()).map(PathBuf::from).collect();
    let r = isf::load_folders(&dirs);
    log::info!(
        "ISF: {} effects, {} sources, {} transitions, {} files skipped",
        r.effects,
        r.sources,
        r.transitions,
        r.skipped.len()
    );
    if let Ok(mut g) = REPORT.lock() {
        *g = Some(r.clone());
    }
    r
}

pub fn last_report() -> Option<LoadReport> {
    REPORT.lock().ok().and_then(|g| g.clone())
}

fn arg(a: Arg) -> IsfArg {
    match a {
        Arg::Const(v) => IsfArg::Const(v),
        Arg::Slot(i) => IsfArg::Slot(i),
    }
}

/// Core shader → renderer description (imported images decoded here).
pub fn desc(s: &IsfShader) -> Result<IsfDesc, String> {
    let vars = s
        .inputs
        .iter()
        .map(|i| IsfVar {
            name: i.name.clone(),
            ty: match i.ty {
                InputType::Float => IsfType::Float,
                InputType::Bool | InputType::Event => IsfType::Bool,
                InputType::Long | InputType::Int => IsfType::Int,
                InputType::Color => IsfType::Color,
                InputType::Point => IsfType::Point,
            },
            slot: i.slot,
            default: i.default,
        })
        .collect();
    let mut images: Vec<(String, IsfImage)> = s
        .images
        .iter()
        .map(|(n, r)| {
            let img = match r {
                ImageRole::Input => IsfImage::Input,
                ImageRole::Audio => IsfImage::Audio,
                ImageRole::AudioFft => IsfImage::AudioFft,
                ImageRole::Other => IsfImage::Empty,
            };
            (n.clone(), img)
        })
        .collect();
    for (n, path) in &s.imported {
        let img = lavmfx_media::thumbnail(path, 4096, 4096)
            .map_err(|e| format!("imported image {n} ({}): {e}", path.display()))?;
        images.push((
            n.clone(),
            IsfImage::Imported(Arc::new(FxImage {
                width: img.width,
                height: img.height,
                data: img.rgba,
                rgba: true,
            })),
        ));
    }
    Ok(IsfDesc {
        name: s.title.clone(),
        fragment: s.fragment.clone(),
        vertex: s.vertex.clone(),
        vars,
        images,
        passes: s
            .passes
            .iter()
            .map(|p| IsfPassDesc {
                target: p.target.clone(),
                persistent: p.persistent,
                float: p.float,
                width: p.width.clone(),
                height: p.height.clone(),
            })
            .collect(),
        generators: s
            .generators
            .iter()
            .map(|g| IsfGenerator {
                name: g.name.clone(),
                speed: arg(g.speed),
                bpm_sync: arg(g.bpm_sync),
            })
            .collect(),
    })
}

/// Compiled programs + what was checked.
pub struct IsfHost {
    pub lib: Arc<IsfLibrary>,
    /// Registry generation the CPU check ran for.
    checked: u64,
    /// Descriptions of shaders that passed the check, by kind.
    descs: HashMap<u32, IsfDesc>,
    /// Kinds whose GPU program is built (for `checked`).
    built: std::collections::HashSet<u32>,
}

impl IsfHost {
    pub fn new() -> Self {
        Self {
            lib: IsfLibrary::new(),
            checked: u64::MAX,
            descs: HashMap::new(),
            built: Default::default(),
        }
    }

    /// After a (re)load: check every shader on the CPU (translate + naga).
    pub fn sync(&mut self) {
        let gen_now = isf::generation();
        if gen_now == self.checked {
            return;
        }
        let t0 = std::time::Instant::now();
        self.descs.clear();
        self.built.clear();
        for (i, s) in isf::shaders().iter().enumerate() {
            if !s.runs() {
                continue;
            }
            let checked = desc(s).and_then(|d| lavmfx_render::isf::check(&d).map(|_| d));
            match checked {
                Ok(d) => {
                    self.descs.insert(ISF_KIND_BASE + i as u32, d);
                }
                Err(e) => {
                    log::warn!("ISF {}: {e}", s.title);
                    isf::set_error(&s.title, Some(e));
                }
            }
        }
        // `set_error` bumps the generation; kinds don't move.
        self.checked = isf::generation();
        log::info!("ISF: checked {} shaders in {:?}", self.descs.len(), t0.elapsed());
    }

    /// Build the GPU programs this frame draws (first use only).
    pub fn prepare(&mut self, device: &eframe::wgpu::Device, queue: &eframe::wgpu::Queue, f: &FxFrame) {
        self.sync();
        let chains = f
            .layer
            .iter()
            .chain(&f.outgoing)
            .chain(&f.group)
            .chain(std::iter::once(&f.composition));
        let kinds: Vec<u32> = chains
            .flat_map(|c| c.iter().map(|e: &EffectDraw| e.kind))
            .filter(|k| *k >= ISF_KIND_BASE && !self.built.contains(k))
            .collect();
        for k in kinds {
            self.built.insert(k);
            let Some(d) = self.descs.get(&k) else { continue };
            if let Err(e) = self.lib.compile(device, queue, k, d) {
                log::warn!("ISF {}: {e}", d.name);
                isf::set_error(&d.name, Some(e));
                self.checked = isf::generation();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_translate_and_validate() {
        reload(&[]);
        let all = isf::shaders();
        assert!(all.len() >= 7);
        for s in &all {
            let d = desc(s).unwrap();
            lavmfx_render::isf::check(&d).unwrap_or_else(|e| panic!("{}: {e}", s.title));
        }
    }

    /// Real-world shaders on this machine: `LAVMFX_ISF_TEST_DIRS=dir1:dir2`
    /// (e.g. MadMapper's Resources and materials folders); skipped without it.
    /// Prints what doesn't run.
    #[test]
    fn local_shader_folders() {
        let dirs: Vec<String> = std::env::var("LAVMFX_ISF_TEST_DIRS")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir())
            .map(str::to_owned)
            .collect();
        if dirs.is_empty() {
            return;
        }
        let r = reload(&dirs);
        let mut host = IsfHost::new();
        host.sync();
        let all = isf::shaders();
        let broken: Vec<_> = all.iter().filter(|s| !s.runs()).collect();
        for s in &broken {
            let first = s
                .error
                .as_deref()
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default();
            eprintln!("BROKEN {}: {first}", s.title);
        }
        for (f, why) in &r.skipped {
            eprintln!("SKIPPED {}: {why}", f.display());
        }
        eprintln!(
            "{} shaders, {} run, {} broken, {} skipped",
            all.len(),
            all.len() - broken.len(),
            broken.len(),
            r.skipped.len()
        );
        reload(&[]);
    }
}
