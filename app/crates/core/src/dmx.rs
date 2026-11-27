//! DMX pixel mapping: fixtures placed over the composition, grouped in
//! lumiverses, turned into DMX channels.
//!
//! A **personality** (a fixture library entry) is an ordered list of blocks: pixel blocks (a grid of pixels with a colour
//! space, a channel distribution and gamma) and plain DMX parameters (one
//! channel with a fixed value). A **fixture** places a personality over an
//! input quad of the composition at a start channel; a **lumiverse** holds
//! fixtures and is sent to an Art-Net / sACN node.
//!
//! Everything here is pure data + maths: sampling the composition and the
//! network live elsewhere (`lavmfx` app, `lavmfx-io::dmx`). The lumiverses
//! live in the Advanced Output ([`crate::output::AdvancedOutput`]).

use serde::{Deserialize, Serialize};

use crate::output::{ColorAdjust, Flip, Pt, rect};

/// Channels in one DMX universe.
pub const UNIVERSE: usize = 512;

/// Which channels a pixel has and their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorSpace {
    /// One channel: luminance (dimmer / white LEDs).
    Mono,
    #[default]
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
    /// Red, green, blue + white (white = the common part of RGB).
    Rgbw,
    /// Grb + white (common on SK6812 strips).
    Grbw,
}

impl ColorSpace {
    pub const ALL: [ColorSpace; 9] = [
        Self::Mono,
        Self::Rgb,
        Self::Rbg,
        Self::Grb,
        Self::Gbr,
        Self::Brg,
        Self::Bgr,
        Self::Rgbw,
        Self::Grbw,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Mono => "Luminance",
            Self::Rgb => "RGB",
            Self::Rbg => "RBG",
            Self::Grb => "GRB",
            Self::Gbr => "GBR",
            Self::Brg => "BRG",
            Self::Bgr => "BGR",
            Self::Rgbw => "RGBW",
            Self::Grbw => "GRBW",
        }
    }

    /// Channels per pixel.
    pub fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Rgbw | Self::Grbw => 4,
            _ => 3,
        }
    }

    /// Write one pixel (0..1 each) into `out` (`channels()` long).
    pub fn pack(self, [r, g, b]: [f32; 3], out: &mut [u8]) {
        let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let w = r.min(g).min(b);
        let v: &[f32] = match self {
            Self::Mono => &[0.2126 * r + 0.7152 * g + 0.0722 * b],
            Self::Rgb => &[r, g, b],
            Self::Rbg => &[r, b, g],
            Self::Grb => &[g, r, b],
            Self::Gbr => &[g, b, r],
            Self::Brg => &[b, r, g],
            Self::Bgr => &[b, g, r],
            Self::Rgbw => &[r - w, g - w, b - w, w],
            Self::Grbw => &[g - w, r - w, b - w, w],
        };
        for (o, x) in out.iter_mut().zip(v) {
            *o = q(*x);
        }
    }
}

/// Corner of the pixel grid the first channel goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Corner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [Self::TopLeft, Self::TopRight, Self::BottomLeft, Self::BottomRight];

    pub fn name(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::TopRight => "Top right",
            Self::BottomLeft => "Bottom left",
            Self::BottomRight => "Bottom right",
        }
    }
}

/// How the channels run through a pixel grid ("Distribution").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Distribution {
    pub start: Corner,
    /// Fill columns first (down, then the next column) instead of rows.
    pub columns: bool,
    /// Every other row / column runs back (zig-zag wiring).
    pub snake: bool,
}

impl Distribution {
    /// Pixel (column, row) for each pixel in channel order.
    pub fn order(&self, w: usize, h: usize) -> Vec<(usize, usize)> {
        let (major, minor) = if self.columns { (w, h) } else { (h, w) };
        let mut out = Vec::with_capacity(w * h);
        for a in 0..major {
            for b in 0..minor {
                let b = if self.snake && a % 2 == 1 { minor - 1 - b } else { b };
                let (mut x, mut y) = if self.columns { (a, b) } else { (b, a) };
                if matches!(self.start, Corner::TopRight | Corner::BottomRight) {
                    x = w - 1 - x;
                }
                if matches!(self.start, Corner::BottomLeft | Corner::BottomRight) {
                    y = h - 1 - y;
                }
                out.push((x, y));
            }
        }
        out
    }
}

/// A grid of pixels sending colour data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PixelBlock {
    /// 1..=512 each.
    pub width: u32,
    pub height: u32,
    pub color: ColorSpace,
    pub distribution: Distribution,
    /// 1..3; 1 = linear. Values are raised to this power before sending.
    pub gamma: f32,
}

impl Default for PixelBlock {
    fn default() -> Self {
        Self {
            width: 1,
            height: 1,
            color: ColorSpace::Rgb,
            distribution: Distribution::default(),
            gamma: 2.5,
        }
    }
}

impl PixelBlock {
    pub fn pixels(&self) -> usize {
        self.width.clamp(1, 512) as usize * self.height.clamp(1, 512) as usize
    }
    pub fn channels(&self) -> usize {
        self.pixels() * self.color.channels()
    }
}

/// One part of a personality, in channel order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Block {
    Pixels(PixelBlock),
    /// A single channel with a fixed value (dimmer, mode, strobe…).
    Param {
        name: String,
        value: u8,
    },
}

impl Block {
    pub fn channels(&self) -> usize {
        match self {
            Block::Pixels(p) => p.channels(),
            Block::Param { .. } => 1,
        }
    }
}

/// A fixture type (personality / fixture library entry).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Personality {
    pub name: String,
    pub blocks: Vec<Block>,
}

impl Default for Personality {
    fn default() -> Self {
        Self::led_strip("LED 16", 16, 1)
    }
}

impl Personality {
    /// A plain RGB pixel strip / matrix ("LED 16" is 16 × 1).
    pub fn led_strip(name: &str, w: u32, h: u32) -> Self {
        Self {
            name: name.into(),
            blocks: vec![Block::Pixels(PixelBlock {
                width: w,
                height: h,
                ..Default::default()
            })],
        }
    }

    pub fn channels(&self) -> usize {
        self.blocks.iter().map(Block::channels).sum()
    }

    /// Aspect of the first pixel block (for new fixtures' input size).
    pub fn grid(&self) -> (u32, u32) {
        self.blocks
            .iter()
            .find_map(|b| match b {
                Block::Pixels(p) => Some((p.width.max(1), p.height.max(1))),
                _ => None,
            })
            .unwrap_or((1, 1))
    }

    /// Built-in library (common shapes).
    pub fn builtin() -> Vec<Personality> {
        let mut par = Self::led_strip("RGB Par (dimmer + RGB)", 1, 1);
        par.blocks.insert(
            0,
            Block::Param {
                name: "Dimmer".into(),
                value: 255,
            },
        );
        let mut rgbw = Self::led_strip("RGBW Par", 1, 1);
        if let Block::Pixels(p) = &mut rgbw.blocks[0] {
            p.color = ColorSpace::Rgbw;
        }
        let mut dimmer = Self::led_strip("Dimmer", 1, 1);
        if let Block::Pixels(p) = &mut dimmer.blocks[0] {
            p.color = ColorSpace::Mono;
        }
        let mut matrix = Self::led_strip("LED Matrix 8×8 (snake)", 8, 8);
        if let Block::Pixels(p) = &mut matrix.blocks[0] {
            p.distribution.snake = true;
        }
        vec![
            Self::led_strip("LED 16", 16, 1),
            Self::led_strip("LED 170 (full universe)", 170, 1),
            Self::led_strip("RGB Pixel", 1, 1),
            par,
            rgbw,
            dimmer,
            matrix,
        ]
    }
}

/// A personality placed over the composition (fixture slice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fixture {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// Copy of the library entry (kept in the setup, so a moved
    /// or deleted library file doesn't break the setup).
    pub personality: Personality,
    /// 1-based first channel in the lumiverse.
    pub start_channel: u32,
    /// Input quad in composition pixels (TL, TR, BR, BL).
    pub input: [Pt; 4],
    pub flip: Flip,
    /// Brightness / contrast / RGB (opacity unused).
    pub color: ColorAdjust,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Fixture".into(),
            enabled: true,
            personality: Personality::default(),
            start_channel: 1,
            input: rect(0.0, 0.0, 160.0, 10.0),
            flip: Flip::default(),
            color: ColorAdjust::default(),
        }
    }
}

impl Fixture {
    /// Last channel used (1-based, inclusive).
    pub fn end_channel(&self) -> u32 {
        self.start_channel + self.personality.channels().max(1) as u32 - 1
    }

    /// Composition point under slice-space (u, v) (0..1, flips applied).
    pub fn map_in(&self, u: f32, v: f32) -> Pt {
        let u = if self.flip.h { 1.0 - u } else { u };
        let v = if self.flip.v { 1.0 - v } else { v };
        let [a, b, c, d] = self.input;
        let top = [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
        let bot = [d[0] + (c[0] - d[0]) * u, d[1] + (c[1] - d[1]) * u];
        [top[0] + (bot[0] - top[0]) * v, top[1] + (bot[1] - top[1]) * v]
    }

    /// Centre of each pixel of `block` in composition px, in grid order
    /// (row by row).
    pub fn pixel_centers(&self, block: &PixelBlock) -> Vec<Pt> {
        let (w, h) = (block.width.clamp(1, 512), block.height.clamp(1, 512));
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                out.push(self.map_in((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32));
            }
        }
        out
    }
}

/// Network protocol a lumiverse is sent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Protocol {
    #[default]
    ArtNet,
    /// sACN / E1.31 (universes 1..63999).
    Sacn,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Self::ArtNet => "Art-Net",
            Self::Sacn => "sACN (E1.31)",
        }
    }
}

/// A group of fixtures sent to one node (lumiverse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lumiverse {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    /// Editor only: fixtures hidden on the Input Selection tab.
    pub folded: bool,
    pub protocol: Protocol,
    /// Node IP. Art-Net: a broadcast address (x.x.x.255) works too. sACN:
    /// empty = the universe's multicast group.
    pub target_ip: String,
    /// First universe. Art-Net: 15-bit port address (net << 8 | subnet << 4 |
    /// universe); sACN: 1..63999. A lumiverse needing more than 512 channels
    /// continues on the following universes.
    pub universe: u16,
    /// Packets per second.
    pub framerate: f32,
    /// Opacity + brightness / contrast / RGB over all its fixtures.
    pub color: ColorAdjust,
    pub fixtures: Vec<Fixture>,
}

impl Default for Lumiverse {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Lumiverse".into(),
            enabled: true,
            folded: false,
            protocol: Protocol::ArtNet,
            // Never a real network by default: the user picks the node.
            target_ip: "127.0.0.1".into(),
            universe: 0,
            framerate: 30.0,
            color: ColorAdjust::default(),
            fixtures: Vec::new(),
        }
    }
}

/// Art-Net port address parts (net 0..127, subnet 0..15, universe 0..15).
pub fn artnet_split(a: u16) -> (u8, u8, u8) {
    (((a >> 8) & 0x7f) as u8, ((a >> 4) & 0xf) as u8, (a & 0xf) as u8)
}

pub fn artnet_join(net: u8, subnet: u8, universe: u8) -> u16 {
    (u16::from(net & 0x7f) << 8) | (u16::from(subnet & 0xf) << 4) | u16::from(universe & 0xf)
}

fn adjust(c: [f32; 3], a: &ColorAdjust) -> [f32; 3] {
    // Same as the slice shader (slices.wgsl `adjust`).
    let f = |x: f32, k: f32| ((x - 0.5) * (1.0 + a.contrast) + 0.5 + a.brightness + k).clamp(0.0, 1.0);
    [f(c[0], a.red), f(c[1], a.green), f(c[2], a.blue)]
}

impl Lumiverse {
    /// Channels used (highest end channel of an enabled fixture).
    pub fn used_channels(&self) -> usize {
        self.fixtures
            .iter()
            .filter(|f| f.enabled)
            .map(|f| f.end_channel() as usize)
            .max()
            .unwrap_or(0)
    }

    /// Universes this lumiverse sends (at least one).
    pub fn universes(&self) -> usize {
        self.used_channels().div_ceil(UNIVERSE).max(1)
    }

    /// Fixtures (indices) whose channels overlap another enabled fixture's.
    pub fn overlaps(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for (i, a) in self.fixtures.iter().enumerate() {
            let hit = a.enabled
                && self.fixtures.iter().enumerate().any(|(j, b)| {
                    j != i && b.enabled && a.start_channel <= b.end_channel() && b.start_channel <= a.end_channel()
                });
            if hit {
                out.push(i);
            }
        }
        out
    }

    /// First free channel after the enabled fixtures (for a new fixture).
    pub fn next_channel(&self) -> u32 {
        self.used_channels() as u32 + 1
    }

    /// All channels (`universes() * 512` bytes). `sample` gives the
    /// composition colour (0..1 RGB) at a composition-pixel point. A disabled
    /// lumiverse / fixture sends zeros.
    pub fn render(&self, sample: &mut dyn FnMut(Pt) -> [f32; 3]) -> Vec<u8> {
        let mut out = vec![0u8; self.universes() * UNIVERSE];
        if !self.enabled {
            return out;
        }
        let opacity = self.color.opacity.clamp(0.0, 1.0);
        for f in self.fixtures.iter().filter(|f| f.enabled) {
            let mut ch = f.start_channel.max(1) as usize - 1;
            for b in &f.personality.blocks {
                match b {
                    Block::Param { value, .. } => {
                        if let Some(o) = out.get_mut(ch) {
                            *o = *value;
                        }
                        ch += 1;
                    }
                    Block::Pixels(px) => {
                        let centers = f.pixel_centers(px);
                        let w = px.width.clamp(1, 512) as usize;
                        let n = px.color.channels();
                        let gamma = px.gamma.clamp(1.0, 3.0);
                        for (x, y) in px.distribution.order(w, px.height.clamp(1, 512) as usize) {
                            let c = adjust(adjust(sample(centers[y * w + x]), &f.color), &self.color);
                            let c = c.map(|v| (v * opacity).powf(gamma));
                            if ch + n <= out.len() {
                                px.color.pack(c, &mut out[ch..ch + n]);
                            }
                            ch += n;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Bilinear sample of an RGBA8 image (rows `stride` bytes apart) at an image
/// pixel point; outside = black.
pub fn sample_rgba(data: &[u8], w: u32, h: u32, stride: u32, p: Pt) -> [f32; 3] {
    if w == 0 || h == 0 || !(p[0] >= 0.0 && p[1] >= 0.0 && p[0] <= w as f32 && p[1] <= h as f32) {
        return [0.0; 3];
    }
    let x = (p[0] - 0.5).clamp(0.0, (w - 1) as f32);
    let y = (p[1] - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |x: u32, y: u32, c: usize| -> f32 {
        let i = (y * stride + x * 4) as usize + c;
        data.get(i).copied().unwrap_or(0) as f32 / 255.0
    };
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
        let bot = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
        *o = top * (1.0 - fy) + bot * fy;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_orders() {
        let d = Distribution::default();
        assert_eq!(d.order(3, 2), [(0, 0), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)]);
        let snake = Distribution { snake: true, ..d };
        assert_eq!(snake.order(3, 2), [(0, 0), (1, 0), (2, 0), (2, 1), (1, 1), (0, 1)]);
        let cols = Distribution {
            columns: true,
            start: Corner::BottomRight,
            snake: true,
        };
        assert_eq!(cols.order(2, 2), [(1, 1), (1, 0), (0, 0), (0, 1)]);
    }

    #[test]
    fn color_spaces_pack() {
        let mut o = [0u8; 4];
        ColorSpace::Grb.pack([1.0, 0.5, 0.0], &mut o);
        assert_eq!(&o[..3], [128, 255, 0]);
        ColorSpace::Rgbw.pack([1.0, 0.5, 0.5], &mut o);
        assert_eq!(o, [128, 0, 0, 128]);
        ColorSpace::Mono.pack([1.0, 1.0, 1.0], &mut o[..1]);
        assert_eq!(o[0], 255);
    }

    #[test]
    fn artnet_address_round_trip() {
        let a = artnet_join(3, 2, 15);
        assert_eq!(a, 0x32f);
        assert_eq!(artnet_split(a), (3, 2, 15));
    }

    #[test]
    fn renders_strip_with_param_and_gamma() {
        let mut f = Fixture {
            start_channel: 10,
            input: rect(0.0, 0.0, 4.0, 1.0),
            personality: Personality::led_strip("t", 4, 1),
            ..Default::default()
        };
        f.personality.blocks.insert(
            0,
            Block::Param {
                name: "Dimmer".into(),
                value: 200,
            },
        );
        if let Block::Pixels(p) = &mut f.personality.blocks[1] {
            p.gamma = 1.0;
        }
        let l = Lumiverse {
            fixtures: vec![f],
            ..Default::default()
        };
        // Red ramp across x: pixel centres at 0.5, 1.5, 2.5, 3.5.
        let out = l.render(&mut |p| [p[0] / 4.0, 0.0, 1.0]);
        assert_eq!(out.len(), 512);
        assert_eq!(out[9], 200);
        assert_eq!(&out[10..13], [32, 0, 255]);
        assert_eq!(&out[19..22], [223, 0, 255]);
        assert_eq!(out[22], 0);
        // Gamma 2 halves mid-grey to a quarter; opacity scales before gamma.
        let mut l2 = l.clone();
        if let Block::Pixels(p) = &mut l2.fixtures[0].personality.blocks[1] {
            p.gamma = 2.0;
        }
        let out = l2.render(&mut |_| [0.5, 0.5, 0.5]);
        assert_eq!(out[10], 64);
        l2.enabled = false;
        assert!(l2.render(&mut |_| [1.0; 3]).iter().all(|&b| b == 0));
    }

    #[test]
    fn big_fixture_spans_universes_and_overlaps_are_found() {
        let mut l = Lumiverse::default();
        l.fixtures.push(Fixture {
            personality: Personality::led_strip("t", 200, 1),
            ..Default::default()
        });
        assert_eq!(l.used_channels(), 600);
        assert_eq!(l.universes(), 2);
        assert_eq!(l.next_channel(), 601);
        assert_eq!(l.render(&mut |_| [1.0; 3]).len(), 1024);
        assert!(l.overlaps().is_empty());
        l.fixtures.push(Fixture {
            start_channel: 600,
            ..Default::default()
        });
        assert_eq!(l.overlaps(), [0, 1]);
        l.fixtures[1].enabled = false;
        assert!(l.overlaps().is_empty());
    }

    #[test]
    fn flips_and_rotated_quads_map_pixels() {
        let mut f = Fixture {
            input: rect(10.0, 20.0, 100.0, 50.0),
            ..Default::default()
        };
        assert_eq!(f.map_in(0.0, 0.0), [10.0, 20.0]);
        f.flip.h = true;
        assert_eq!(f.map_in(0.0, 1.0), [110.0, 70.0]);
        let block = PixelBlock {
            width: 2,
            height: 1,
            ..Default::default()
        };
        assert_eq!(f.pixel_centers(&block), [[85.0, 45.0], [35.0, 45.0]]);
    }

    #[test]
    fn samples_rgba_bilinear() {
        // 2×1 image, stride 8: black, white.
        let img = [0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(sample_rgba(&img, 2, 1, 8, [0.5, 0.5]), [0.0; 3]);
        assert_eq!(sample_rgba(&img, 2, 1, 8, [1.5, 0.5]), [1.0; 3]);
        assert_eq!(sample_rgba(&img, 2, 1, 8, [1.0, 0.5]), [0.5; 3]);
        assert_eq!(sample_rgba(&img, 2, 1, 8, [-1.0, 0.5]), [0.0; 3]);
    }
}
