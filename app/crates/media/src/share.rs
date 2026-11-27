//! Video shared between apps on the same machine: **PipeWire** video nodes on
//! Linux, **Spout** senders on Windows, **Syphon** servers on macOS (one
//! backend per platform, same API).
//!
//! * [`sources`]: what other apps (and cameras) publish.
//! * [`receiver`]: frames of one source (shared like NDI receivers).
//! * [`output`]: publish the composition ([`FrameSink`]).
//!
//! PipeWire runs on one thread ("pipewire") that owns the connection; the
//! app talks to it through a channel. Spout / Syphon (GPU texture sharing via
//! DirectX / Metal) are not built yet: they report so.

use std::sync::Arc;

pub use crate::live::{FrameSink, OutFrame, Receiver, RecvStatus};

/// A source another app publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSource {
    /// What a clip stores and connects to (PipeWire `node.name`, Spout sender name).
    pub name: String,
    /// Shown in the browser.
    pub label: String,
}

/// "PipeWire", "Spout" or "Syphon".
pub fn backend() -> &'static str {
    if cfg!(target_os = "windows") {
        "Spout"
    } else if cfg!(target_os = "macos") {
        "Syphon"
    } else {
        "PipeWire"
    }
}

/// None when sharing works on this machine.
pub fn unavailable() -> Option<String> {
    imp::unavailable()
}

pub fn sources() -> Vec<ShareSource> {
    imp::sources()
}

/// Shared receiver of source `name`.
pub fn receiver(name: &str) -> Result<Arc<Receiver>, String> {
    crate::live::pooled(&format!("share:{name}"), || imp::receiver(name))
}

/// Publish frames as `name` (`width × height` RGBA).
pub fn output(name: &str, width: u32, height: u32) -> Result<Box<dyn FrameSink>, String> {
    imp::output(name, width, height)
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    fn not_yet() -> String {
        format!(
            "{} isn't built yet (needs a {} session)",
            super::backend(),
            if cfg!(target_os = "windows") {
                "Windows"
            } else {
                "macOS"
            }
        )
    }
    pub fn unavailable() -> Option<String> {
        Some(not_yet())
    }
    pub fn sources() -> Vec<ShareSource> {
        Vec::new()
    }
    pub fn receiver(_: &str) -> Result<Arc<Receiver>, String> {
        Err(not_yet())
    }
    pub fn output(_: &str, _: u32, _: u32) -> Result<Box<dyn FrameSink>, String> {
        Err(not_yet())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use pipewire as pw;
    use pw::properties::properties;
    use pw::spa;
    use spa::param::video::{VideoFormat, VideoInfoRaw};
    use spa::pod::Pod;

    use super::ShareSource;
    use crate::live::{
        FrameSink, OutFrame, Pixel, Receiver, Shared, gray_to_rgba, publish, rgba_frame, set_error, to_rgba,
        yuy2_to_rgba,
    };

    /// Our output nodes' `node.name` prefix (left out of [`sources`]).
    const OUT_PREFIX: &str = "lavmfx-out";

    struct Node {
        id: u32,
        name: String,
        label: String,
        /// A camera (V4L2 / libcamera): listed under Capture Devices instead.
        camera: bool,
    }

    /// What an output stream reads (filled by the app, taken in `process`).
    #[derive(Default)]
    struct OutShared {
        pending: Option<OutFrame>,
        spare: Vec<Vec<u8>>,
        sent: u64,
        /// Stream is linked to a consumer.
        streaming: bool,
    }

    enum Cmd {
        /// `mjpg`: offer MJPG instead of raw formats (second try for sources without raw).
        Input {
            key: u64,
            node: String,
            shared: Shared,
            mjpg: bool,
        },
        Output {
            key: u64,
            name: String,
            w: u32,
            h: u32,
            shared: Arc<Mutex<OutShared>>,
        },
        /// A new frame is pending for output `key`.
        Frame(u64),
        Stop(u64),
    }

    struct Pw {
        tx: pw::channel::Sender<Cmd>,
        nodes: Arc<Mutex<Vec<Node>>>,
    }

    fn pw() -> Result<&'static Pw, String> {
        static PW: OnceLock<Result<Pw, String>> = OnceLock::new();
        PW.get_or_init(start).as_ref().map_err(Clone::clone)
    }

    fn next_key() -> u64 {
        static K: AtomicU64 = AtomicU64::new(1);
        K.fetch_add(1, Ordering::Relaxed)
    }

    pub fn unavailable() -> Option<String> {
        pw().err()
    }

    pub fn sources() -> Vec<ShareSource> {
        let Ok(p) = pw() else { return Vec::new() };
        let mut v: Vec<ShareSource> = p
            .nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| !n.name.starts_with(OUT_PREFIX) && !n.camera)
            .map(|n| ShareSource {
                name: n.name.clone(),
                label: n.label.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.label.cmp(&b.label).then(a.name.cmp(&b.name)));
        number_duplicates(&mut v);
        v
    }

    /// Same labels (two identical webcams) → "Name", "Name 2", … (input sorted by label).
    pub(super) fn number_duplicates(v: &mut [ShareSource]) {
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        for s in v.iter_mut() {
            let n = seen.entry(s.label.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                s.label = format!("{} {n}", s.label);
            }
        }
    }

    fn node_exists(p: &Pw, name: &str) -> bool {
        p.nodes.lock().unwrap().iter().any(|n| n.name == name)
    }

    pub fn receiver(name: &str) -> Result<Arc<Receiver>, String> {
        let p = pw()?;
        // The registry fills in right after start: give it a moment.
        for _ in 0..20 {
            if node_exists(p, name) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if !node_exists(p, name) {
            return Err(format!("no PipeWire video source \"{name}\" (is the app sending?)"));
        }
        let key = next_key();
        let shared: Shared = Default::default();
        p.tx.send(Cmd::Input {
            key,
            node: name.to_owned(),
            shared: shared.clone(),
            mjpg: false,
        })
        .map_err(|_| "PipeWire thread stopped".to_string())?;
        let tx = p.tx.clone();
        Ok(Arc::new(Receiver::new(name, shared, move || {
            let _ = tx.send(Cmd::Stop(key));
        })))
    }

    struct Output {
        key: u64,
        name: String,
        shared: Arc<Mutex<OutShared>>,
        tx: pw::channel::Sender<Cmd>,
    }

    impl FrameSink for Output {
        fn buffer(&self, len: usize) -> Vec<u8> {
            let mut b = self.shared.lock().unwrap().spare.pop().unwrap_or_default();
            b.resize(len, 0);
            b
        }
        fn send(&self, f: OutFrame) -> bool {
            let mut s = self.shared.lock().unwrap();
            if !s.streaming {
                s.spare.push(f.data);
                s.spare.truncate(2);
                return false;
            }
            if let Some(old) = s.pending.replace(f) {
                s.spare.push(old.data);
                s.spare.truncate(2);
            }
            drop(s);
            self.tx.send(Cmd::Frame(self.key)).is_ok()
        }
        fn status(&self) -> String {
            let s = self.shared.lock().unwrap();
            if s.streaming {
                format!("PipeWire \"{}\" · in use · {} frames", self.name, s.sent)
            } else {
                format!("PipeWire \"{}\" · waiting for an app to use it", self.name)
            }
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            let _ = self.tx.send(Cmd::Stop(self.key));
        }
    }

    pub fn output(name: &str, w: u32, h: u32) -> Result<Box<dyn FrameSink>, String> {
        let p = pw()?;
        let key = next_key();
        let shared = Arc::new(Mutex::new(OutShared::default()));
        p.tx.send(Cmd::Output {
            key,
            name: name.to_owned(),
            w,
            h,
            shared: shared.clone(),
        })
        .map_err(|_| "PipeWire thread stopped".to_string())?;
        Ok(Box::new(Output {
            key,
            name: name.to_owned(),
            shared,
            tx: p.tx.clone(),
        }))
    }

    // ---------------------------------------------------------------- thread

    fn start() -> Result<Pw, String> {
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let nodes: Arc<Mutex<Vec<Node>>> = Default::default();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let n = nodes.clone();
        let tx_loop = tx.clone();
        std::thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || {
                if let Err(e) = run(rx, tx_loop, n, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Pw { tx, nodes }),
            Ok(Err(e)) => Err(format!("PipeWire: {e}")),
            Err(_) => Err("PipeWire didn't answer".into()),
        }
    }

    /// Stream + its listener (kept alive together).
    type Entry = (pw::stream::StreamRc, Box<dyn std::any::Any>);

    fn run(
        rx: pw::channel::Receiver<Cmd>,
        tx: pw::channel::Sender<Cmd>,
        nodes: Arc<Mutex<Vec<Node>>>,
        ready: &std::sync::mpsc::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
        let core = context.connect_rc(None).map_err(|e| e.to_string())?;
        let registry = core.get_registry_rc().map_err(|e| e.to_string())?;
        let (n_add, n_rm) = (nodes.clone(), nodes.clone());
        let _reg = registry
            .add_listener_local()
            .global(move |obj| {
                if obj.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = obj.props else { return };
                let class = props.get("media.class").unwrap_or_default();
                if !class.starts_with("Video/Source") {
                    return;
                }
                let name = props.get("node.name").unwrap_or_default().to_owned();
                let label = props
                    .get("node.description")
                    .or_else(|| props.get("node.nick"))
                    .unwrap_or(&name)
                    .to_owned();
                let camera = matches!(props.get("device.api"), Some("v4l2" | "libcamera"));
                n_add.lock().unwrap().push(Node {
                    id: obj.id,
                    name,
                    label,
                    camera,
                });
            })
            .global_remove(move |id| n_rm.lock().unwrap().retain(|n| n.id != id))
            .register();
        let streams: Rc<RefCell<HashMap<u64, Entry>>> = Default::default();
        let st = streams.clone();
        let core2 = core.clone();
        let _attached = rx.attach(mainloop.loop_(), move |cmd| match cmd {
            Cmd::Input {
                key,
                node,
                shared,
                mjpg,
            } => match input_stream(&core2, key, &node, shared.clone(), mjpg, &tx) {
                Ok(e) => {
                    st.borrow_mut().insert(key, e);
                }
                Err(e) => set_error(&shared, e),
            },
            Cmd::Output {
                key,
                name,
                w,
                h,
                shared,
            } => match output_stream(&core2, &name, w, h, shared) {
                Ok(e) => {
                    st.borrow_mut().insert(key, e);
                }
                Err(e) => log::warn!("PipeWire output {name}: {e}"),
            },
            Cmd::Frame(key) => {
                if let Some((s, _)) = st.borrow().get(&key) {
                    let _ = s.trigger_process();
                }
            }
            Cmd::Stop(key) => {
                if let Some((s, l)) = st.borrow_mut().remove(&key) {
                    let _ = s.disconnect();
                    drop(l);
                }
            }
        });
        // Registry globals arrive with the first roundtrip; report ready after a short spin.
        let _ = ready.send(Ok(()));
        mainloop.run();
        drop(streams);
        Ok(())
    }

    fn serialize(obj: spa::pod::Object) -> Vec<u8> {
        spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
            .expect("pod")
            .0
            .into_inner()
    }

    /// `SPA_PARAM_Buffers`: 2–16 buffers of one `stride × height` block of
    /// `data_types` (bit set of `SPA_DATA_*`). `stride` 0 = compressed (the
    /// producer sizes the block).
    /// Input buffers: memfd / dmabuf the producer owns, which we map. Not MemPtr:
    /// with it PipeWire's camera module uses the driver's user-pointer mode,
    /// which tears frames on some webcams (this laptop's Chicony HD Webcam)
    /// where FFmpeg / OBS (driver's own mmap buffers) are clean.
    const INPUT_DATA: i32 = (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_DmaBuf);

    fn buffers_param(stride: i32, height: i32, data_types: i32) -> Vec<u8> {
        use spa::sys as s;
        let int = |k: u32, v: i32| spa::pod::Property::new(k, spa::pod::Value::Int(v));
        serialize(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
            id: spa::param::ParamType::Buffers.as_raw(),
            properties: vec![
                spa::pod::Property::new(
                    s::SPA_PARAM_BUFFERS_buffers,
                    spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                        spa::utils::ChoiceFlags::empty(),
                        spa::utils::ChoiceEnum::Range {
                            default: 8,
                            min: 2,
                            max: 16,
                        },
                    ))),
                ),
                int(s::SPA_PARAM_BUFFERS_blocks, 1),
            ]
            .into_iter()
            .chain(
                (stride > 0)
                    .then(|| {
                        [
                            int(s::SPA_PARAM_BUFFERS_size, stride * height),
                            int(s::SPA_PARAM_BUFFERS_stride, stride),
                        ]
                    })
                    .into_iter()
                    .flatten(),
            )
            .chain([spa::pod::Property::new(
                s::SPA_PARAM_BUFFERS_dataType,
                spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Flags {
                        default: data_types,
                        flags: vec![],
                    },
                ))),
            )])
            .collect(),
        })
    }

    pub(super) fn video_formats(formats: &[VideoFormat], size: Option<(u32, u32)>, rate: Option<u32>) -> Vec<u8> {
        use spa::param::format::{FormatProperties as F, MediaSubtype, MediaType};
        let mut props = vec![
            spa::pod::property!(F::MediaType, Id, MediaType::Video),
            spa::pod::property!(F::MediaSubtype, Id, MediaSubtype::Raw),
        ];
        // One format is written as a plain Id, not a one-item choice: some
        // apps (OBS's camera source) skip entries whose format is a choice.
        let format = match formats {
            [one] => spa::pod::Value::Id(spa::utils::Id(one.as_raw())),
            _ => spa::pod::Value::Choice(spa::pod::ChoiceValue::Id(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Enum {
                    default: spa::utils::Id(formats[0].as_raw()),
                    alternatives: formats.iter().map(|f| spa::utils::Id(f.as_raw())).collect(),
                },
            ))),
        };
        props.push(spa::pod::Property::new(F::VideoFormat.as_raw(), format));
        match size {
            Some((w, h)) => props.push(spa::pod::property!(
                F::VideoSize,
                Rectangle,
                spa::utils::Rectangle { width: w, height: h }
            )),
            None => props.push(spa::pod::property!(
                F::VideoSize,
                Choice,
                Range,
                Rectangle,
                spa::utils::Rectangle {
                    width: 1920,
                    height: 1080
                },
                spa::utils::Rectangle { width: 1, height: 1 },
                spa::utils::Rectangle {
                    width: 8192,
                    height: 8192
                }
            )),
        }
        match rate {
            Some(r) => props.push(spa::pod::property!(
                F::VideoFramerate,
                Fraction,
                spa::utils::Fraction { num: r, denom: 1 }
            )),
            None => props.push(spa::pod::property!(
                F::VideoFramerate,
                Choice,
                Range,
                Fraction,
                spa::utils::Fraction { num: 30, denom: 1 },
                spa::utils::Fraction { num: 0, denom: 1 },
                spa::utils::Fraction { num: 1000, denom: 1 }
            )),
        }
        serialize(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: props,
        })
    }

    /// Negotiated format of a stream, from `param_changed`.
    fn parse_format(param: Option<&Pod>, id: u32) -> Option<VideoInfoRaw> {
        let param = param?;
        if id != spa::param::ParamType::Format.as_raw() {
            return None;
        }
        let (mt, ms) = spa::param::format_utils::parse_format(param).ok()?;
        if mt != spa::param::format::MediaType::Video || ms != spa::param::format::MediaSubtype::Raw {
            return None;
        }
        let mut info = VideoInfoRaw::default();
        info.parse(param).ok()?;
        Some(info)
    }

    /// What an input stream negotiated.
    #[derive(Debug, Clone, Copy)]
    enum InFormat {
        Raw {
            format: VideoFormat,
            w: usize,
            h: usize,
            fps: f64,
        },
        /// Size comes from each JPEG.
        Mjpg { fps: f64 },
    }

    /// Per input stream (loop thread): the format and the MJPG decoder.
    #[derive(Default)]
    struct InState {
        format: Option<InFormat>,
        jpeg: Option<crate::JpegDecoder>,
        /// Damaged frames in a row.
        corrupted: u32,
    }

    /// Size and rate of a negotiated MJPG format (no helper in libspa: read the pod).
    fn parse_mjpg(param: &Pod) -> Option<(usize, usize, f64)> {
        use spa::param::format::FormatProperties as F;
        let (_, v) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()).ok()?;
        let spa::pod::Value::Object(o) = v else { return None };
        let mut size = None;
        let mut fps = 0.0;
        for p in &o.properties {
            match p.value {
                spa::pod::Value::Rectangle(r) if p.key == F::VideoSize.as_raw() => {
                    size = Some((r.width as usize, r.height as usize));
                }
                spa::pod::Value::Fraction(f) if p.key == F::VideoFramerate.as_raw() && f.denom > 0 => {
                    fps = f64::from(f.num) / f64::from(f.denom);
                }
                _ => {}
            }
        }
        size.map(|(w, h)| (w, h, fps))
    }

    fn negotiated(param: Option<&Pod>, id: u32) -> Option<InFormat> {
        let param = param?;
        if id != spa::param::ParamType::Format.as_raw() {
            return None;
        }
        let (mt, ms) = spa::param::format_utils::parse_format(param).ok()?;
        if mt != spa::param::format::MediaType::Video {
            return None;
        }
        if ms == spa::param::format::MediaSubtype::Mjpg {
            let (_, _, fps) = parse_mjpg(param)?;
            return Some(InFormat::Mjpg { fps });
        }
        let info = parse_format(Some(param), id)?;
        let fr = info.framerate();
        Some(InFormat::Raw {
            format: info.format(),
            w: info.size().width as usize,
            h: info.size().height as usize,
            fps: if fr.denom > 0 {
                f64::from(fr.num) / f64::from(fr.denom)
            } else {
                0.0
            },
        })
    }

    fn mjpg_formats() -> Vec<u8> {
        use spa::param::format::{FormatProperties as F, MediaSubtype, MediaType};
        serialize(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: vec![
                spa::pod::property!(F::MediaType, Id, MediaType::Video),
                spa::pod::property!(F::MediaSubtype, Id, MediaSubtype::Mjpg),
                spa::pod::property!(
                    F::VideoSize,
                    Choice,
                    Range,
                    Rectangle,
                    spa::utils::Rectangle {
                        width: 1280,
                        height: 720
                    },
                    spa::utils::Rectangle { width: 1, height: 1 },
                    spa::utils::Rectangle {
                        width: 8192,
                        height: 8192
                    }
                ),
                spa::pod::property!(
                    F::VideoFramerate,
                    Choice,
                    Range,
                    Fraction,
                    spa::utils::Fraction { num: 30, denom: 1 },
                    spa::utils::Fraction { num: 0, denom: 1 },
                    spa::utils::Fraction { num: 1000, denom: 1 }
                ),
            ],
        })
    }

    fn input_stream(
        core: &pw::core::CoreRc,
        key: u64,
        node: &str,
        shared: Shared,
        mjpg: bool,
        tx: &pw::channel::Sender<Cmd>,
    ) -> Result<Entry, String> {
        let stream = pw::stream::StreamRc::new(
            core.clone(),
            "lavmfx-input",
            properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Camera",
                *pw::keys::TARGET_OBJECT => node,
                "node.dont-fallback" => "true",
                "node.dont-reconnect" => "true",
            },
        )
        .map_err(|e| e.to_string())?;
        let sh_state = shared.clone();
        let (retry_tx, retry_node) = (tx.clone(), node.to_owned());
        let listener = stream
            .add_local_listener_with_user_data(InState::default())
            .state_changed(move |_, st, _, new| {
                if let pw::stream::StreamState::Error(e) = new {
                    // No raw format in common: try again with MJPG (replaces this stream).
                    if !mjpg && st.format.is_none() {
                        let _ = retry_tx.send(Cmd::Input {
                            key,
                            node: retry_node.clone(),
                            shared: sh_state.clone(),
                            mjpg: true,
                        });
                    } else {
                        set_error(&sh_state, e);
                    }
                }
            })
            .param_changed(|stream, st, id, param| {
                let Some(f) = negotiated(param, id) else { return };
                // Buffers we can read (as PipeWire's video-play.c): mapped memory.
                let bytes = match f {
                    InFormat::Raw { format, w, h, .. } => {
                        let bpp = match format {
                            VideoFormat::YUY2 => 2,
                            VideoFormat::GRAY8 => 1,
                            _ => 4,
                        };
                        buffers_param((w * bpp) as i32, h as i32, INPUT_DATA)
                    }
                    // Compressed: the camera sizes its blocks.
                    InFormat::Mjpg { .. } => buffers_param(0, 0, INPUT_DATA),
                };
                if let Some(p) = Pod::from_bytes(&bytes) {
                    let _ = stream.update_params(&mut [p]);
                }
                st.format = Some(f);
            })
            .process(move |stream, st| {
                let Some(mut buf) = stream.dequeue_buffer() else { return };
                let Some(f) = st.format else { return };
                let datas = buf.datas_mut();
                let Some(d) = datas.first_mut() else { return };
                let (stride, offset, len) = (
                    d.chunk().stride().max(0) as usize,
                    d.chunk().offset() as usize,
                    d.chunk().size() as usize,
                );
                // The driver marks frames it thinks are damaged. Only trusted for MJPG
                // (truncated JPEGs); this machine's webcam flags every raw frame too.
                let compressed = matches!(f, InFormat::Mjpg { .. });
                if compressed && d.chunk().flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                    st.corrupted += 1;
                    if st.corrupted == 30 {
                        set_error(&shared, "the source sends damaged frames (camera / USB problem)");
                    }
                    return;
                }
                let damaged = d.chunk().flags().contains(spa::buffer::ChunkFlags::CORRUPTED);
                st.corrupted = 0;
                let Some(bytes) = d.data() else { return };
                let Some(src) = bytes.get(offset..) else { return };
                let (frame, fps) = match f {
                    InFormat::Mjpg { fps, .. } => {
                        if st.jpeg.is_none() {
                            match crate::JpegDecoder::new() {
                                Ok(j) => st.jpeg = Some(j),
                                Err(e) => return set_error(&shared, e.to_string()),
                            }
                        }
                        let jpeg = &src[..len.min(src.len())];
                        match st.jpeg.as_mut().and_then(|j| j.decode(jpeg)) {
                            Some((w, h, px)) => (Some(rgba_frame(w, h, px)), fps),
                            None => return, // a broken frame now and then: skip it
                        }
                    }
                    InFormat::Raw { format, w, h, fps } => {
                        let rgba = match format {
                            VideoFormat::RGBA => to_rgba(src, w, h, stride.max(w * 4), Pixel::Rgba),
                            VideoFormat::RGBx => to_rgba(src, w, h, stride.max(w * 4), Pixel::Rgbx),
                            VideoFormat::BGRA => to_rgba(src, w, h, stride.max(w * 4), Pixel::Bgra),
                            VideoFormat::BGRx => to_rgba(src, w, h, stride.max(w * 4), Pixel::Bgrx),
                            VideoFormat::YUY2 => yuy2_to_rgba(src, w, h, stride.max(w * 2)),
                            VideoFormat::GRAY8 => gray_to_rgba(src, w, h, stride.max(w)),
                            other => return set_error(&shared, format!("unsupported format {other:?}")),
                        };
                        (rgba.map(|px| rgba_frame(w as u32, h as u32, px)), fps)
                    }
                };
                match frame {
                    Some(fr) => {
                        publish(&shared, fr, fps);
                        // Shown anyway (raw), but say why it may look torn.
                        if damaged {
                            set_error(
                                &shared,
                                "the source marks its frames as damaged (camera / driver problem)",
                            );
                        }
                    }
                    None => set_error(&shared, "frame smaller than its format says"),
                }
            })
            .register()
            .map_err(|e| e.to_string())?;
        // Raw formats first; MJPG only for sources without raw ones (webcams'
        // MJPG is often damaged, e.g. this machine's HD Webcam, and PipeWire
        // would follow the camera's preference for it).
        let formats = if mjpg {
            mjpg_formats()
        } else {
            video_formats(
                &[
                    VideoFormat::RGBA,
                    VideoFormat::RGBx,
                    VideoFormat::BGRA,
                    VideoFormat::BGRx,
                    VideoFormat::YUY2,
                    VideoFormat::GRAY8,
                ],
                None,
                None,
            )
        };
        let mut params = [Pod::from_bytes(&formats).ok_or("format pod")?];
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut params,
            )
            .map_err(|e| e.to_string())?;
        Ok((stream, Box::new(listener)))
    }

    fn output_stream(
        core: &pw::core::CoreRc,
        name: &str,
        w: u32,
        h: u32,
        shared: Arc<Mutex<OutShared>>,
    ) -> Result<Entry, String> {
        let node_name = format!(
            "{OUT_PREFIX}-{}",
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                })
                .collect::<String>()
        );
        let stream = pw::stream::StreamRc::new(
            core.clone(),
            name,
            properties! {
                *pw::keys::MEDIA_CLASS => "Video/Source",
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Camera",
                *pw::keys::NODE_NAME => node_name.as_str(),
                *pw::keys::NODE_DESCRIPTION => name,
            },
        )
        .map_err(|e| e.to_string())?;
        let sh = shared.clone();
        let listener = stream
            .add_local_listener_with_user_data((w, h))
            .state_changed(move |_, _, _, new| {
                sh.lock().unwrap().streaming = matches!(new, pw::stream::StreamState::Streaming);
            })
            .param_changed(|stream, size, id, param| {
                if parse_format(param, id).is_none() {
                    return;
                }
                // Buffers: one block of stride × height.
                let bytes = buffers_param(
                    size.0 as i32 * 4,
                    size.1 as i32,
                    (1 << spa::sys::SPA_DATA_MemPtr) | (1 << spa::sys::SPA_DATA_MemFd),
                );
                if let Some(p) = Pod::from_bytes(&bytes) {
                    let _ = stream.update_params(&mut [p]);
                }
            })
            .process(move |stream, size| {
                let Some(f) = shared.lock().unwrap().pending.take() else {
                    return;
                };
                let Some(mut buf) = stream.dequeue_buffer() else {
                    shared.lock().unwrap().spare.push(f.data);
                    return;
                };
                let row = size.0 as usize * 4;
                if let Some(d) = buf.datas_mut().first_mut() {
                    if let Some(dst) = d.data() {
                        for y in 0..(size.1 as usize).min(f.height as usize) {
                            let s0 = y * f.stride as usize;
                            let (Some(src), Some(out)) =
                                (f.data.get(s0..s0 + row), dst.get_mut(y * row..y * row + row))
                            else {
                                break;
                            };
                            out.copy_from_slice(src);
                        }
                    }
                    let c = d.chunk_mut();
                    *c.offset_mut() = 0;
                    *c.stride_mut() = row as i32;
                    *c.size_mut() = (row * size.1 as usize) as u32;
                }
                let mut s = shared.lock().unwrap();
                s.sent += 1;
                s.spare.push(f.data);
                s.spare.truncate(2);
            })
            .register()
            .map_err(|e| e.to_string())?;
        // One fixed EnumFormat per pixel format (as cameras offer them), so
        // apps that read only fixed values (OBS) list them.
        let rgba = video_formats(&[VideoFormat::RGBA], Some((w, h)), Some(60));
        let rgbx = video_formats(&[VideoFormat::RGBx], Some((w, h)), Some(60));
        let mut params = [
            Pod::from_bytes(&rgba).ok_or("format pod")?,
            Pod::from_bytes(&rgbx).ok_or("format pod")?,
        ];
        stream
            .connect(
                spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::MAP_BUFFERS,
                &mut params,
            )
            .map_err(|e| e.to_string())?;
        log::info!("PipeWire output \"{name}\" ({node_name}) {w}×{h}");
        Ok((stream, Box::new(listener)))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn duplicate_labels_get_numbers() {
        let mk = |n: &str, l: &str| ShareSource {
            name: n.into(),
            label: l.into(),
        };
        let mut v = vec![
            mk("a", "HD Webcam"),
            mk("b", "HD Webcam"),
            mk("c", "HD Webcam"),
            mk("d", "OBS"),
        ];
        imp::number_duplicates(&mut v);
        let labels: Vec<&str> = v.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["HD Webcam", "HD Webcam 2", "HD Webcam 3", "OBS"]);
    }

    /// The format property of an EnumFormat pod.
    fn format_value(bytes: &[u8]) -> pipewire::spa::pod::Value {
        use pipewire::spa::pod::{Value, deserialize::PodDeserializer};
        let (_, v) = PodDeserializer::deserialize_any_from(bytes).unwrap();
        let Value::Object(o) = v else { panic!("not an object") };
        let key = pipewire::spa::param::format::FormatProperties::VideoFormat.as_raw();
        o.properties.into_iter().find(|p| p.key == key).unwrap().value
    }

    /// One format is a plain Id (OBS's camera source skips choices); several
    /// stay a choice for the receiving side.
    #[test]
    fn single_format_is_fixed() {
        use pipewire::spa::{param::video::VideoFormat, pod::Value};
        let one = imp::video_formats(&[VideoFormat::RGBA], Some((64, 32)), Some(60));
        assert!(matches!(format_value(&one), Value::Id(id) if id.0 == VideoFormat::RGBA.as_raw()));
        let two = imp::video_formats(&[VideoFormat::RGBA, VideoFormat::RGBx], None, None);
        assert!(matches!(format_value(&two), Value::Choice(_)));
    }
    use std::time::{Duration, Instant};

    /// Our output, received back through PipeWire (skips without a PipeWire daemon).
    #[test]
    fn pipewire_loopback() {
        if let Some(e) = unavailable() {
            eprintln!("skipping: {e}");
            return;
        }
        let (w, h) = (64u32, 32u32);
        let out = output("LAVMFX Test Out", w, h).unwrap();
        let t0 = Instant::now();
        // Our own outputs are hidden from `sources`: connect by node name.
        let src = "lavmfx-out-lavmfx-test-out";
        let rx = loop {
            match receiver(src) {
                Ok(r) => break r,
                Err(e) if t0.elapsed() > Duration::from_secs(5) => panic!("{e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        let mut seen = 0;
        let got = loop {
            let mut data = out.buffer((w * h * 4) as usize);
            for p in data.as_chunks_mut::<4>().0 {
                *p = [200, 100, 50, 255];
            }
            out.send(OutFrame {
                data,
                width: w,
                height: h,
                stride: w * 4,
                rate: (60, 1),
            });
            if let Some(f) = rx.latest(&mut seen) {
                break f;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "no frame: {:?} / {}",
                rx.status(),
                out.status()
            );
            std::thread::sleep(Duration::from_millis(16));
        };
        assert_eq!((got.width, got.height), (w, h));
        assert_eq!(&got.planes[0].data[..4], &[200, 100, 50, 255]);
        assert!(out.status().contains("in use"), "{}", out.status());
    }
}
