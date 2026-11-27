// Video effects. One pipeline; `fx.kind` selects the effect
// (index = lavmfx-core `effects::SHADER_EFFECTS`). Params arrive as 32 floats
// in that table's order (colours take 4 slots, bools 0/1, choices an index).
//
// Input `src` is premultiplied (layer/group buffers may be transparent).
// Effects work on straight colour; the result is mixed with the original by
// the effect's dry/wet Opacity and Blend Mode, then stored premultiplied.
// Param meanings follow their names and ranges.

struct Fx {
    p: array<vec4<f32>, 8>,
    resolution: vec2<f32>,
    time: f32,
    beat: f32,
    opacity: f32,
    blend: u32,
    kind: u32,
    pass_: u32,  // multi-pass effects: current pass (0-based)
    passes: u32,
    prev_time: f32, // time of this effect's previous frame (-1 = first)
    head: u32,      // history ring: newest layer
    depth: u32,     // history ring size (0 = none)
};

@group(0) @binding(0) var<uniform> fx: Fx;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var prev_tex: texture_2d<f32>;
@group(0) @binding(4) var hist_tex: texture_2d_array<f32>;
@group(0) @binding(5) var aux_tex: texture_2d<f32>; // CPU image (text coverage in R, or RGBA: Mask file)
@group(0) @binding(6) var below_tex: texture_2d<f32>; // mixed below the layer (UV Map), premultiplied
struct SliceQ {
    p01: vec4<f32>, // TL, TR (composition uv)
    p23: vec4<f32>, // BR, BL
    info: vec4<f32>, // x name rank, y index, z 1 = real slice
};
@group(0) @binding(7) var<storage, read> slices_buf: array<SliceQ>;
// Slice Chase values of this effect, one per slices_buf entry (CPU, lavmfx-core
// `chase`): x level 0..1, y phase in its step 0..1, z step number, w 1 = in the sequence.
@group(0) @binding(8) var<storage, read> chase_buf: array<vec4<f32>>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

fn P(i: u32) -> f32 {
    return fx.p[i / 4u][i % 4u];
}
fn PB(i: u32) -> bool {
    return P(i) > 0.5;
}
fn PC(i: u32) -> vec4<f32> {
    return vec4<f32>(P(i), P(i + 1u), P(i + 2u), P(i + 3u));
}

fn unpremul(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 1e-5) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(c.rgb / c.a, c.a);
}

// Straight colour at uv; outside the frame transparent (or opaque black).
fn tex(uv: vec2<f32>) -> vec4<f32> {
    return unpremul(textureSampleLevel(src_tex, samp, uv, 0.0));
}
fn tex_bg(uv: vec2<f32>, black: bool) -> vec4<f32> {
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), black);
    }
    return tex(uv);
}
// Mirrored repeat.
fn mirror_uv(uv: vec2<f32>) -> vec2<f32> {
    let m = abs(fract(uv * 0.5) * 2.0 - 1.0);
    return 1.0 - m;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}
fn aspect() -> f32 {
    return fx.resolution.x / fx.resolution.y;
}
fn rot(p: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
}
fn hash(n: f32) -> f32 {
    return fract(sin(n * 127.1 + 311.7) * 43758.5453);
}

fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let k = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = select(vec4<f32>(c.gb, k.xy), vec4<f32>(c.bg, k.wz), c.g < c.b);
    let q = select(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), p.x < c.r);
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}
fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let p = abs(fract(c.xxx + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return c.z * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

// Weighted 2D blur around uv with radius r (uv units), n×n taps.
fn blur(uv: vec2<f32>, r: vec2<f32>, n: i32, gauss: f32) -> vec4<f32> {
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    let h = f32(n - 1) * 0.5;
    for (var y = 0; y < n; y++) {
        for (var x = 0; x < n; x++) {
            let o = (vec2<f32>(f32(x), f32(y)) - h) / max(h, 1.0);
            let w = mix(1.0, exp(-dot(o, o) * 2.5), gauss);
            acc += textureSampleLevel(src_tex, samp, uv + o * r, 0.0) * w; // premultiplied
            wsum += w;
        }
    }
    return unpremul(acc / wsum);
}

// ------------------------------------------------------------------ effects

fn e_transform(uv: vec2<f32>) -> vec4<f32> {
    let res = fx.resolution;
    let anchor = vec2<f32>(P(8u), P(9u));
    var q = uv * res - res * 0.5 - vec2<f32>(P(0u), P(1u)) - anchor;
    q = rot(q, -radians(P(7u)));
    let s = P(2u) / 100.0 * vec2<f32>(P(3u) / 100.0 * cos(radians(P(6u))), P(4u) / 100.0 * cos(radians(P(5u))));
    if (abs(s.x) < 1e-4 || abs(s.y) < 1e-4) {
        return vec4<f32>(0.0);
    }
    q = q / s + anchor;
    return tex_bg((q + res * 0.5) / res, false);
}

fn e_blur(uv: vec2<f32>) -> vec4<f32> {
    // 0 Blur, 1/2 Blur X/Y Distance (radius, of the width), 3 Quality (unused: 33 taps), 4 Gaussian.
    // Two passes: horizontal, vertical.
    let d = select(P(1u), P(2u), fx.pass_ == 1u) * P(0u) * 0.5 * aspect();
    return sep_blur(uv, pass_dir(fx.pass_, d), P(4u));
}

fn e_brightness_contrast(c: vec4<f32>) -> vec4<f32> {
    let k = P(1u);
    let f = select(1.0 + k, 1.0 / max(1.0 - k * 0.99, 0.01), k > 0.0);
    return vec4<f32>((c.rgb - 0.5) * f + 0.5 + P(0u), c.a);
}

fn e_hue_rotate(c: vec4<f32>) -> vec4<f32> {
    var h = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    h.x = fract(h.x * P(0u) + P(1u));
    h.y = clamp(h.y * P(2u) * 2.0, 0.0, 1.0);
    return vec4<f32>(hsv2rgb(h), c.a);
}

fn e_saturation(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(mix(vec3<f32>(luma(c.rgb)), c.rgb, P(0u) * 2.0), c.a);
}

fn e_invert(c: vec4<f32>) -> vec4<f32> {
    let k = vec3<f32>(P(0u), P(1u), P(2u));
    return vec4<f32>(mix(c.rgb, 1.0 - c.rgb, k), mix(c.a, 1.0 - c.a, P(3u)));
}

fn e_colorize(c: vec4<f32>) -> vec4<f32> {
    // 0-3 Color, 4 Mode (palette: not yet), 5 Weighted, 6 GreysKeep, 7 GreysTolerance, 8 Invert, 9 Contrast
    let col = PC(0u);
    var l = luma(c.rgb);
    if (PB(8u)) {
        l = 1.0 - l;
    }
    l = clamp((l - 0.5) * (1.0 + P(9u) * 3.0) + 0.5, 0.0, 1.0);
    var o: vec3<f32>;
    if (PB(5u)) {
        o = col.rgb * l * 2.0;
    } else {
        let h = rgb2hsv(col.rgb);
        o = hsv2rgb(vec3<f32>(h.x, h.y, l));
    }
    let sat = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))).y;
    if (PB(6u) && sat < P(7u)) {
        o = c.rgb;
    }
    return vec4<f32>(o, c.a);
}

fn e_tint(c: vec4<f32>) -> vec4<f32> {
    let l = luma(c.rgb);
    return vec4<f32>(mix(PC(0u).rgb, PC(4u).rgb, l), c.a);
}

fn e_threshold(c: vec4<f32>) -> vec4<f32> {
    // 0 Threshold, 1 Smoothness, 2-5 Color 1, 6-9 Color 2, 10 Alpha (Original|Combine|Replace)
    let l = luma(c.rgb);
    let h = P(1u) * 0.5 + 1e-4;
    let t = smoothstep(P(0u) - h, P(0u) + h, l);
    let col = mix(PC(6u), PC(2u), t);
    let mode = u32(P(10u) + 0.5);
    var a = c.a;
    if (mode == 1u) {
        a = c.a * col.a;
    } else if (mode == 2u) {
        a = col.a;
    }
    return vec4<f32>(col.rgb, a);
}

fn e_posterize(c: vec4<f32>) -> vec4<f32> {
    let n = mix(32.0, 2.0, P(0u));
    return vec4<f32>(floor(c.rgb * n + 0.5) / n, c.a);
}

fn e_exposure(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * exp2((P(0u) - 0.5) * 8.0), c.a);
}

fn e_vignette(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Roundness, 1 Size, 2 Softness, 3 Black BG
    let a = aspect();
    let d = length((uv - 0.5) * 2.0 * mix(vec2<f32>(1.0), vec2<f32>(a, 1.0) / max(a, 1.0), P(0u)));
    let r = 1.42 * (1.0 - P(1u));
    let v = 1.0 - smoothstep(r * (1.0 - P(2u)), r + 1e-4, d);
    if (PB(3u)) {
        return vec4<f32>(c.rgb * v, c.a);
    }
    return vec4<f32>(c.rgb, c.a * v);
}

fn e_mirror(uv0: vec2<f32>) -> vec4<f32> {
    // 0 X, 1 Y, 2 In / Out, 3 Flip X, 4 Flip Y, 5 Black BG
    var uv = uv0;
    let ax = 0.5 + P(0u) * 0.5;
    let ay = 1.0 - P(1u) * 0.5;
    let keep_first = P(2u) > 0.5;
    if (keep_first) {
        if (uv.x > ax) { uv.x = 2.0 * ax - uv.x; }
        if (uv.y > ay) { uv.y = 2.0 * ay - uv.y; }
    } else {
        if (uv.x < 1.0 - ax) { uv.x = 2.0 * (1.0 - ax) - uv.x; }
        if (uv.y < 1.0 - ay) { uv.y = 2.0 * (1.0 - ay) - uv.y; }
    }
    if (PB(3u)) { uv.x = 1.0 - uv.x; }
    if (PB(4u)) { uv.y = 1.0 - uv.y; }
    return tex_bg(uv, PB(5u));
}

fn e_mirror_quad(uv: vec2<f32>) -> vec4<f32> {
    var q = 1.0 - abs(uv * 2.0 - 1.0);
    if (PB(0u)) { q.x = 1.0 - q.x; }
    if (PB(1u)) { q.y = 1.0 - q.y; }
    return tex(q);
}

fn e_flip(uv0: vec2<f32>) -> vec4<f32> {
    var uv = uv0;
    if (PB(0u)) { uv.x = 1.0 - uv.x; }
    if (PB(1u)) { uv.y = 1.0 - uv.y; }
    return tex_bg(uv, PB(2u));
}

fn e_kaleidoscope(uv: vec2<f32>) -> vec4<f32> {
    // 0 Angles, 1 Rotation, 2 Zoom, 3 Input Rotation, 4 Black BG
    let a = aspect();
    var p = (uv - 0.5) * vec2<f32>(a, 1.0);
    let n = max(2.0, floor(P(0u) * 20.0 + 0.5));
    let seg = TAU / n;
    var ang = atan2(p.y, p.x) - P(1u) * TAU;
    ang = ang - floor(ang / seg) * seg;
    ang = min(ang, seg - ang) + P(3u) * TAU;
    let r = length(p) / exp2((P(2u) - 0.5) * 4.0);
    p = vec2<f32>(cos(ang), sin(ang)) * r;
    return tex(mirror_uv(p / vec2<f32>(a, 1.0) + 0.5));
}

fn e_polar_kaleido(uv: vec2<f32>) -> vec4<f32> {
    // 0 Rings (1-16), 1 Parts (1-16), 2 Angle
    let p = (uv - 0.5) * vec2<f32>(aspect(), 1.0);
    let ang = atan2(p.y, p.x) / TAU + 0.5;
    let u = ang * P(1u) + P(2u);
    let v = length(p) * 2.0 * P(0u);
    return tex(mirror_uv(vec2<f32>(u * 2.0, v)));
}

fn shift_uv(uv: vec2<f32>, s: f32, mode: u32) -> vec2<f32> {
    switch mode {
        case 0u: { return uv + vec2<f32>(s, 0.0); }
        case 1u: { return uv + vec2<f32>(0.0, s); }
        case 2u: { return vec2<f32>(uv.x, (uv.y - 0.5) / max(1.0 - abs(s) * 2.0, 0.05) + 0.5); }
        case 3u: { return vec2<f32>((uv.x - 0.5) / max(1.0 - abs(s) * 2.0, 0.05) + 0.5, uv.y); }
        case 4u: {
            let a = aspect();
            return rot((uv - 0.5) * vec2<f32>(a, 1.0), s * PI) / vec2<f32>(a, 1.0) + 0.5;
        }
        default: { return (uv - 0.5) / (1.0 + s) + 0.5; }
    }
}

fn e_shift_rgb(uv: vec2<f32>) -> vec4<f32> {
    // 0 Distance, 1 Mode, 2 R, 3 G, 4 B, 5 Black BG
    let d = P(0u) * 0.1;
    let mode = u32(P(1u) + 0.5);
    let bg = PB(5u);
    let r = tex_bg(shift_uv(uv, P(2u) * d, mode), bg);
    let g = tex_bg(shift_uv(uv, P(3u) * d, mode), bg);
    let b = tex_bg(shift_uv(uv, P(4u) * d, mode), bg);
    return vec4<f32>(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}

fn e_wave_warp(uv: vec2<f32>) -> vec4<f32> {
    // 0 Mode (sine..triangle), 1 Height, 2 Width, 3 Angle, 4 Speed, 5 Pin edges, 6 Black BG
    let a = aspect();
    let th = P(3u) * TAU; // Angle 1 = 6.3 rad
    let dir = vec2<f32>(cos(th), sin(th));
    let pos = dot((uv - 0.5) * vec2<f32>(a, 1.0), dir);
    let ph = TAU * pos / ((1.0 - P(2u)) * 0.3 + 0.01) - fx.time * P(4u) * 8.0; // Width 0.2 ≈ 7 waves across
    let tri = abs(fract(ph / TAU) * 4.0 - 2.0) - 1.0;
    let w = mix(sin(ph), tri, P(0u));
    var off = vec2<f32>(-dir.y, dir.x) * w * P(1u) * 0.1 / vec2<f32>(a, 1.0);
    if (PB(5u)) {
        off *= sin(PI * clamp(uv.x, 0.0, 1.0)) * sin(PI * clamp(uv.y, 0.0, 1.0));
    }
    return tex_bg(uv + off, PB(6u));
}

fn e_strobe(c: vec4<f32>) -> vec4<f32> {
    // 0 Frequency, 1 Fade, 2-5 Color
    let rate = P(0u) * 30.0;
    if (rate <= 0.0) {
        return c;
    }
    let ph = fract(fx.time * rate);
    var k = select(0.0, 1.0, ph < 0.5);
    if (PB(1u)) {
        k = 1.0 - ph;
    }
    let col = PC(2u);
    return vec4<f32>(mix(c.rgb, col.rgb, k * col.a), max(c.a, k * col.a));
}

fn e_trails(uv: vec2<f32>) -> vec4<f32> {
    // 0 Feedback, 1 Mode (Alpha|Luma|Lighten). Works premultiplied.
    let cur = textureSampleLevel(src_tex, samp, uv, 0.0);
    let old = textureSampleLevel(prev_tex, samp, uv, 0.0) * P(0u);
    let mode = u32(P(1u) + 0.5);
    var o: vec4<f32>;
    if (mode == 1u) {
        let k = luma(unpremul(cur).rgb) * cur.a;
        o = cur * k + old * (1.0 - k);
    } else if (mode == 2u) {
        o = max(cur, old);
    } else {
        o = cur + old * (1.0 - cur.a);
    }
    return unpremul(o);
}

fn e_fish_eye(uv: vec2<f32>) -> vec4<f32> {
    let a = aspect();
    let p = (uv - 0.5) * 2.0 * vec2<f32>(a, 1.0) / max(a, 1.0);
    let r = length(p);
    let q = p * pow(max(r, 1e-4), P(0u) * 1.5);
    return tex_bg(q * max(a, 1.0) / vec2<f32>(a, 1.0) * 0.5 + 0.5, PB(1u));
}

fn e_tile(uv: vec2<f32>) -> vec4<f32> {
    // 0 Tile X, 1 Tile Y, 2 Skew X, 3 Skew Y, 4 Rotation Angle, 5 Black BG
    let a = aspect();
    var p = rot((uv - 0.5) * vec2<f32>(a, 1.0), P(4u) * TAU) / vec2<f32>(a, 1.0);
    var q = p * vec2<f32>(1.0 + P(0u) * 10.0, 1.0 + P(1u) * 10.0) + 0.5;
    q = q + vec2<f32>((P(2u) - 0.5) * 2.0 * q.y, (P(3u) - 0.5) * 2.0 * q.x);
    return tex(fract(q));
}

fn e_crop(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Left, 1 Right, 2 Top, 3 Bottom (pixels), 4 Invert, 5 Black BG
    let px = uv * fx.resolution;
    var inside = px.x >= P(0u) && px.x <= P(1u) && px.y >= P(2u) && px.y <= P(3u);
    if (PB(4u)) {
        inside = !inside;
    }
    if (inside) {
        return c;
    }
    return select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(5u));
}

fn e_edge(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Mode, 1 Multiplier, 2 Use Color, 3-6 Color, 7 Amount
    let d = 1.0 / fx.resolution;
    var gx = 0.0;
    var gy = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let l = luma(tex(uv + vec2<f32>(f32(x), f32(y)) * d).rgb);
            let wx = f32(x) * select(1.0, 2.0, y == 0);
            let wy = f32(y) * select(1.0, 2.0, x == 0);
            gx += l * wx;
            gy += l * wy;
        }
    }
    let e = clamp(length(vec2<f32>(gx, gy)) * P(1u) * 10.0, 0.0, 1.0);
    let ec = select(vec3<f32>(e), PC(3u).rgb * e, PB(2u));
    let o = mix(c.rgb + ec, ec, P(0u));
    return vec4<f32>(mix(c.rgb, o, P(7u)), c.a);
}

fn e_sharpen(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    let d = P(1u) / fx.resolution;
    let avg = (tex(uv + vec2<f32>(d.x, 0.0)) + tex(uv - vec2<f32>(d.x, 0.0)) + tex(uv + vec2<f32>(0.0, d.y))
        + tex(uv - vec2<f32>(0.0, d.y))) * 0.25;
    return vec4<f32>(c.rgb + (c.rgb - avg.rgb) * P(0u) * 4.0, c.a);
}

fn e_solid(c: vec4<f32>) -> vec4<f32> {
    let col = PC(0u);
    if (PB(4u)) {
        return vec4<f32>(mix(c.rgb, col.rgb, col.a), max(c.a, col.a));
    }
    return col;
}

fn e_auto_mask(c: vec4<f32>) -> vec4<f32> {
    let ch = u32(P(0u) + 0.5);
    var m = luma(c.rgb);
    if (ch == 1u) { m = c.r; } else if (ch == 2u) { m = c.g; } else if (ch == 3u) { m = c.b; }
    m = clamp((m - 0.5) * exp2((P(1u) - 0.5) * 6.0) + 0.5, 0.0, 1.0);
    if (PB(2u)) {
        m = 1.0 - m;
    }
    return vec4<f32>(c.rgb, c.a * m);
}

fn e_vibrance(c: vec4<f32>) -> vec4<f32> {
    let l = luma(c.rgb);
    let s = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))).y;
    return vec4<f32>(mix(vec3<f32>(l), c.rgb, 1.0 + P(0u) * (1.0 - s)), c.a);
}

fn e_displace(uv: vec2<f32>) -> vec4<f32> {
    // 0 X factor, 1 Y factor, 2 Smooth, 3 Threshold, 4 Invert, 5 Mode (Repeat|Wrap|Transparent), 6 Black BG
    var src = tex(uv);
    if (PB(2u)) {
        src = blur(uv, vec2<f32>(0.01, 0.01 * aspect()), 3, 1.0);
    }
    var l = max(luma(src.rgb) - P(3u), 0.0) / max(1.0 - P(3u), 1e-3);
    if (PB(4u)) {
        l = 1.0 - l;
    }
    let q = uv + (vec2<f32>(P(0u), P(1u)) - 0.5) * l;
    let mode = u32(P(5u) + 0.5);
    if (mode == 0u) {
        return tex(fract(q));
    } else if (mode == 1u) {
        return tex(mirror_uv(q));
    }
    return tex_bg(q, PB(6u));
}

// Random value held for 1/(freq*30) s; 0 when freq is 0.
fn twitch(freq: f32, seed: f32) -> f32 {
    if (freq <= 0.0) {
        return 0.0;
    }
    return hash(floor(fx.time * freq * 30.0) + seed) * 2.0 - 1.0;
}

fn e_twitch(uv: vec2<f32>) -> vec4<f32> {
    // 0 Global, 1/2 Max Pos X/Y, 3 Pos Freq, 4 Max Scale, 5 Scale Freq, 6 Max Opacity, 7 Opacity Freq,
    // 8 Max Light, 9 Light Freq, 10 Max RGB Shift, 11 RGB Freq, 12 Max Blur, 13 Blur Freq, 14 Black BG
    let g = P(0u);
    let pos = vec2<f32>(twitch(P(3u), 1.0) * P(1u), twitch(P(3u), 2.0) * P(2u)) * g * 0.5;
    let sc = 1.0 + abs(twitch(P(5u), 3.0)) * P(4u) * g;
    let q = (uv - 0.5 - pos) / sc + 0.5;
    let shift = twitch(P(11u), 4.0) * P(10u) * g * 0.05;
    let br = abs(twitch(P(13u), 5.0)) * P(12u) * g * 0.03;
    let bg = PB(14u);
    var c: vec4<f32>;
    if (br > 0.0) {
        c = blur(q, vec2<f32>(br, br * aspect()), 4, 1.0);
    } else {
        c = tex_bg(q, bg);
    }
    if (shift != 0.0) {
        c = vec4<f32>(tex_bg(q + vec2<f32>(shift, 0.0), bg).r, c.g, tex_bg(q - vec2<f32>(shift, 0.0), bg).b, c.a);
    }
    let light = abs(twitch(P(9u), 6.0)) * P(8u) * g;
    let op = 1.0 - abs(twitch(P(7u), 7.0)) * P(6u) * g;
    return vec4<f32>(c.rgb + light, c.a * op);
}

fn e_drop_shadow(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Distance (px), 1 Angle (deg), 2-5 Color, 6 Spread, 7 Strength
    let off = vec2<f32>(cos(radians(P(1u))), sin(radians(P(1u)))) * P(0u) / fx.resolution;
    let r = P(6u) * 20.0;
    let a = blur(uv - off, vec2<f32>(r, r * aspect()), 5, 1.0).a;
    let col = PC(2u);
    let sa = clamp(a * P(7u) / 8.0, 0.0, 1.0) * col.a;
    // c over shadow (straight colours)
    let oa = c.a + sa * (1.0 - c.a);
    let rgb = (c.rgb * c.a + col.rgb * sa * (1.0 - c.a)) / max(oa, 1e-5);
    return vec4<f32>(rgb, oa);
}

// ------------------------------------------------------------------ M5b batch 1 helpers

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}
// Random 0..1 for an integer cell `p` (floored) and a seed.
fn rnd(p: vec2<f32>, seed: u32) -> f32 {
    let q = vec2<i32>(floor(p));
    return f32(pcg(bitcast<u32>(q.x) ^ pcg(bitcast<u32>(q.y) ^ pcg(seed)))) / 4294967295.0;
}
fn vnoise(p: vec2<f32>, seed: u32) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(rnd(i, seed), rnd(i + vec2<f32>(1.0, 0.0), seed), u.x);
    let b = mix(rnd(i + vec2<f32>(0.0, 1.0), seed), rnd(i + vec2<f32>(1.0, 1.0), seed), u.x);
    return mix(a, b, u.y);
}
// Ordered-dither threshold (Bayer matrix 2^bits square), 0..1.
fn bayer(px: vec2<f32>, bits: u32) -> f32 {
    let x = u32(max(px.x, 0.0));
    let y = u32(max(px.y, 0.0));
    var v = 0u;
    for (var i = 0u; i < bits; i++) {
        let bx = (x >> i) & 1u;
        let by = (y >> i) & 1u;
        v = (v << 2u) | ((bx ^ by) << 1u) | by;
    }
    return (f32(v) + 0.5) / f32(1u << (2u * bits));
}
// Straight colour `rgb` with coverage `a` over straight `bg`.
fn over(rgb: vec3<f32>, a: f32, bg: vec4<f32>) -> vec4<f32> {
    let oa = a + bg.a * (1.0 - a);
    return vec4<f32>((rgb * a + bg.rgb * bg.a * (1.0 - a)) / max(oa, 1e-5), oa);
}
fn px_coord(uv: vec2<f32>) -> vec2<f32> {
    return uv * fx.resolution;
}
// Falling snowflakes, 0..1 coverage. density 0..1, size ≈1, wind/wobble in screen widths.
fn flakes(uv: vec2<f32>, density: f32, speed: f32, size: f32, wind: f32, fall: f32, wobble: f32, soft: f32) -> f32 {
    var s = 0.0;
    let a = aspect();
    for (var i = 0u; i < 5u; i++) {
        let fi = f32(i);
        let scale = 8.0 + fi * 7.0;
        let v = vec2<f32>(wind, fall) * fx.time * speed * (1.2 - fi * 0.15);
        var p = ((uv - 0.5) * vec2<f32>(a, 1.0) - v) * scale;
        p.x += sin(fx.time * 1.3 + p.y * 0.7 + fi) * wobble * 2.0;
        let id = floor(p);
        if (rnd(id, i * 7u) < density) {
            let ctr = 0.25 + 0.5 * vec2<f32>(rnd(id, i * 7u + 1u), rnd(id, i * 7u + 2u));
            let r = (0.06 + 0.08 * rnd(id, i * 7u + 3u)) * size;
            let d = length(fract(p) - ctr);
            let f = 1.0 - smoothstep(r * (1.0 - soft) - 0.02, r, d);
            s = max(s, f * (1.0 - fi * 0.13));
        }
    }
    return s;
}

// ------------------------------------------------------------------ M5b batch 1 effects

fn e_levels(c: vec4<f32>) -> vec4<f32> {
    // 0 Base (offset), 1 Midpoint (gamma), 2 Ratio (contrast); 0.5 = neutral
    var v = clamp(c.rgb + (P(0u) - 0.5), vec3<f32>(0.0), vec3<f32>(1.0));
    v = pow(v, vec3<f32>(exp2((0.5 - P(1u)) * 3.0)));
    v = (v - 0.5) * exp2((P(2u) - 0.5) * 4.0) + 0.5;
    return vec4<f32>(v, c.a);
}

fn palette(i: u32, t: f32) -> vec3<f32> {
    // Cosine palettes a + b·cos(2π(c·t + d)).
    var pa = array<vec3<f32>, 8>(
        vec3<f32>(0.5), vec3<f32>(0.5), vec3<f32>(0.5), vec3<f32>(0.5),
        vec3<f32>(0.5), vec3<f32>(0.8, 0.5, 0.4), vec3<f32>(0.5, 0.3, 0.2), vec3<f32>(0.5));
    var pb = array<vec3<f32>, 8>(
        vec3<f32>(0.5), vec3<f32>(0.5), vec3<f32>(0.5), vec3<f32>(0.5),
        vec3<f32>(0.5), vec3<f32>(0.2, 0.4, 0.2), vec3<f32>(0.5, 0.4, 0.3), vec3<f32>(0.5));
    var pc = array<vec3<f32>, 8>(
        vec3<f32>(1.0), vec3<f32>(1.0), vec3<f32>(1.0, 1.0, 0.5), vec3<f32>(1.0, 0.7, 0.4),
        vec3<f32>(2.0, 1.0, 0.0), vec3<f32>(2.0, 1.0, 1.0), vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 1.0, 0.0));
    var pd = array<vec3<f32>, 8>(
        vec3<f32>(0.0, 0.33, 0.67), vec3<f32>(0.0, 0.1, 0.2), vec3<f32>(0.8, 0.9, 0.3), vec3<f32>(0.0, 0.15, 0.2),
        vec3<f32>(0.5, 0.2, 0.25), vec3<f32>(0.0, 0.25, 0.25), vec3<f32>(0.0, 0.1, 0.2), vec3<f32>(0.5, 0.2, 0.0));
    return pa[i] + pb[i] * cos(TAU * (pc[i] * t + pd[i]));
}

fn e_recolour(c: vec4<f32>) -> vec4<f32> {
    // 0 Palette (8 built-in gradients), 1 Floor, 2 Ceiling (luma window), 3 Cycle Phase,
    // 4 Cycle Speed (0.5 = still), 5 Use Alpha (alpha from luma)
    let i = u32(clamp(P(0u) * 8.0, 0.0, 7.0));
    let l = luma(c.rgb);
    let t = clamp((l - P(1u)) / max(P(2u) - P(1u), 1e-3), 0.0, 1.0);
    let col = palette(i, t + P(3u) + fx.time * (P(4u) - 0.5) * 2.0);
    return vec4<f32>(col, select(c.a, c.a * l, PB(5u)));
}

fn e_color_pass(c: vec4<f32>) -> vec4<f32> {
    // 0 Hue 1, 1 Hue 2 (kept hue band), 2 Invert; the rest goes grey
    let h = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))).x;
    let lo = min(P(0u), P(1u));
    let hi = max(P(0u), P(1u));
    var k = smoothstep(lo - 0.01, lo + 0.01, h) * (1.0 - smoothstep(hi - 0.01, hi + 0.01, h));
    if (hi >= 0.999) {
        k = smoothstep(lo - 0.01, lo + 0.01, h);
    }
    if (PB(2u)) {
        k = 1.0 - k;
    }
    return vec4<f32>(mix(vec3<f32>(luma(c.rgb)), c.rgb, k), c.a);
}

fn e_add_subtract(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb + vec3<f32>(P(0u), P(1u), P(2u)), c.a);
}

fn ck_alpha(col: vec3<f32>) -> f32 {
    let h = rgb2hsv(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)));
    var d = abs(h.x - P(1u));
    d = min(d, 1.0 - d);
    let hue_k = 1.0 - smoothstep(P(2u), P(2u) + P(3u) + 1e-4, d);
    let sat_k = smoothstep(P(4u), P(4u) + P(5u) + 1e-4, h.y); // whites (low sat) stay
    let val_k = smoothstep(P(6u), P(6u) + P(7u) + 1e-4, h.z); // blacks stay
    return 1.0 - hue_k * sat_k * val_k;
}

fn e_chroma_key(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Show Mask, 1 Key Hue, 2/3 Hue Threshold/Softness, 4/5 White, 6/7 Black,
    // 8/9 Spill Threshold/Softness, 10 Edge Shrink, 11 Edge Blur (radii up to 8 px)
    let a0 = ck_alpha(c.rgb);
    let px = 1.0 / fx.resolution;
    let rs = P(10u) * 8.0;
    let rb = P(11u) * 8.0;
    var mn = a0;
    var sum = a0;
    for (var i = 0; i < 8; i++) {
        let ang = f32(i) * TAU / 8.0;
        let o = vec2<f32>(cos(ang), sin(ang)) * px;
        if (rs > 0.0) {
            mn = min(mn, ck_alpha(tex(uv + o * rs).rgb));
        }
        sum += select(a0, ck_alpha(tex(uv + o * rb).rgb), rb > 0.0);
    }
    let a = min(mn, sum / 9.0);
    if (PB(0u)) {
        return vec4<f32>(vec3<f32>(a * c.a), 1.0);
    }
    // Spill: desaturate what is left near the key hue.
    let h = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    var d = abs(h.x - P(1u));
    d = min(d, 1.0 - d);
    let spill = (1.0 - smoothstep(P(8u), P(8u) + P(9u) + 1e-4, d)) * h.y;
    return vec4<f32>(mix(c.rgb, vec3<f32>(luma(c.rgb)), spill), c.a * a);
}

fn e_dither(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Mode 2x|4x|8x (Bayer size), 1 bit per channel
    let bits = u32(clamp(P(0u), 0.0, 2.0)) + 1u;
    let t = bayer(px_coord(uv), bits);
    return vec4<f32>(step(vec3<f32>(t), c.rgb), c.a);
}

fn e_dot_screen(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Saturation (colour back in), 1 Angle (×360°), 2 Scale (dot size); halftone like three.js DotScreen
    let p = rot(px_coord(uv) - fx.resolution * 0.5, P(1u) * TAU) * exp2((0.5 - P(2u)) * 4.0);
    let pattern = sin(p.x) * sin(p.y) * 4.0;
    let g = clamp(luma(c.rgb) * 10.0 - 5.0 + pattern, 0.0, 1.0);
    return vec4<f32>(mix(vec3<f32>(g), c.rgb * g, P(0u)), c.a);
}

fn e_hatched(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Density (1-8), 1 Levels (2-6), 2 Invert, 3-6 Color1 (paper), 7-10 Color2 (ink)
    let px = px_coord(uv);
    let spacing = max(24.0 / max(P(0u), 1.0), 3.0);
    let n = max(floor(P(1u) + 0.5), 2.0);
    let dark = floor((1.0 - luma(c.rgb)) * n) / (n - 1.0); // 0..1 in steps
    var dirs = array<vec2<f32>, 4>(
        vec2<f32>(0.7071, 0.7071), vec2<f32>(0.7071, -0.7071), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0));
    var ink = 0.0;
    for (var k = 0u; k < 4u; k++) {
        if (dark > f32(k) / 4.0 + 0.01) {
            let d = abs(fract(dot(px, dirs[k]) / spacing) - 0.5) * spacing;
            ink = max(ink, 1.0 - smoothstep(0.5, 1.5, d));
        }
    }
    var paper = PC(3u);
    var pen = PC(7u);
    if (PB(2u)) {
        let t = paper;
        paper = pen;
        pen = t;
    }
    let o = mix(paper, pen, ink);
    return vec4<f32>(o.rgb, c.a * o.a);
}

fn e_lorez(uv: vec2<f32>) -> vec4<f32> {
    // 0 Pixel Size (1-64 px), 1 Bit Reduction (8→1 bits), 2 Dither
    let size = 1.0 + floor(P(0u) * 63.0);
    let cell = floor(px_coord(uv) / size);
    var col = tex((cell + 0.5) * size / fx.resolution);
    let levels = exp2(8.0 - P(1u) * 7.0) - 1.0;
    let t = (bayer(cell, 3u) - 0.5) * P(2u);
    col = vec4<f32>(floor(col.rgb * levels + 0.5 + t) / levels, col.a);
    return col;
}

fn e_pixel_blur(uv: vec2<f32>) -> vec4<f32> {
    // 0 Blur, 1/2 Distance X/Y (px), 3 Interval (tap spacing: fewer taps), 4 Gaussian
    let r = vec2<f32>(P(1u), P(2u)) * P(0u) / fx.resolution;
    let n = clamp(i32(12.0 / max(P(3u), 1.0)) + 1, 3, 13);
    return blur(uv, r, n, P(4u));
}

fn e_pixel_high_pass(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0-2 R/G/B: per-channel mix towards a 3 px high pass (0.5 grey = flat)
    let b = blur(uv, 3.0 / fx.resolution, 3, 0.0);
    let hp = clamp(c.rgb - b.rgb + 0.5, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(mix(c.rgb, hp, vec3<f32>(P(0u), P(1u), P(2u))), c.a);
}

fn e_polkadot(uv: vec2<f32>) -> vec4<f32> {
    // 0 Dot Size (cell = up to half the height), 1 Threshold (darker cells drop out)
    let cell = max(P(0u) * 0.5 * fx.resolution.y, 3.0);
    let px = px_coord(uv);
    let ctr = (floor(px / cell) + 0.5) * cell;
    let s = tex(ctr / fx.resolution);
    let r = cell * 0.45;
    var m = 1.0 - smoothstep(r - 1.0, r + 0.5, length(px - ctr));
    if (luma(s.rgb) < P(1u)) {
        m = 0.0;
    }
    return vec4<f32>(s.rgb, s.a * m);
}

fn e_raster(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0/1 Modulo X/Y: keep every n-th pixel column and row, 2 Black BG
    let p = floor(px_coord(uv));
    let m = max(floor(vec2<f32>(P(0u), P(1u)) + 0.5), vec2<f32>(1.0));
    let keep = (p.x % m.x) < 0.5 && (p.y % m.y) < 0.5;
    if (keep) {
        return c;
    }
    return select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(2u));
}

fn noisy_n(g: vec2<f32>, seed: u32, smooth_: bool) -> f32 {
    if (smooth_) {
        return vnoise(g, seed);
    }
    return rnd(g, seed);
}

fn e_noisy(uv: vec2<f32>) -> vec4<f32> {
    // 0 Noise (amount), 1/2 Noise X/Y (displacement), 3 Noise S (brightness), 4 Noise C (colour),
    // 5 Resolution (1 = per pixel, 0 = 64 px grain), 6 Interpolate (smooth noise)
    let grain = mix(64.0, 1.0, P(5u));
    let g = px_coord(uv) / grain;
    let t = u32(fx.time * 30.0) * 16u;
    let sm = PB(6u);
    let amt = P(0u);
    let off = (vec2<f32>(noisy_n(g, t, sm), noisy_n(g, t + 1u, sm)) - 0.5) * vec2<f32>(P(1u), P(2u)) * amt * 0.2;
    var col = tex(uv + off);
    let s = (noisy_n(g, t + 2u, sm) - 0.5) * P(3u) * amt * 2.0;
    let n = vec3<f32>(noisy_n(g, t + 3u, sm), noisy_n(g, t + 4u, sm), noisy_n(g, t + 5u, sm)) - 0.5;
    col = vec4<f32>(col.rgb + s + n * P(4u) * amt * 2.0, col.a);
    return col;
}

fn e_static(uv: vec2<f32>) -> vec4<f32> {
    // 0 Mode Color|Grayscale|Two-tone: TV static (Opacity mixes it over the image)
    let p = floor(px_coord(uv));
    let t = u32(fx.time * 60.0) * 3u;
    let r = rnd(p, t);
    let mode = u32(P(0u) + 0.5);
    var o = vec3<f32>(r);
    if (mode == 0u) {
        o = vec3<f32>(r, rnd(p, t + 1u), rnd(p, t + 2u));
    } else if (mode == 2u) {
        o = vec3<f32>(step(0.5, r));
    }
    return vec4<f32>(o, 1.0);
}

fn e_snow(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Amount, 1 Speed, 2 Textured (soft flakes), 3 View Y (fall steepness), 4 View X (wind, 0.5 = none), 5 Black BG
    let base = select(c, vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(5u));
    let s = flakes(uv, P(0u), P(1u) * 0.5, 1.0, (P(4u) - 0.5) * 2.0, mix(0.3, 1.7, P(3u)), 0.1, P(2u));
    return vec4<f32>(mix(base.rgb, vec3<f32>(1.0), s), max(base.a, s));
}

fn e_snowy(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Size (1-2), 1 Speed, 2 Amount (1-200), 3 Spread (sideways drift), 4 Threshold (snow settles
    // on brighter parts), 5 Show Image, 6 Smart Edges (settles only on top edges)
    let base = select(vec4<f32>(0.0, 0.0, 0.0, 1.0), c, PB(5u));
    let s = flakes(uv, P(2u) / 200.0, P(1u) * 0.1, P(0u), 0.0, 1.0, P(3u), 0.3);
    let l = luma(c.rgb);
    var cover = smoothstep(P(4u) - 0.05, P(4u) + 0.05, l);
    if (PB(6u)) {
        var e = 0.0;
        for (var k = 1; k <= 4; k++) {
            let above = luma(tex(uv - vec2<f32>(0.0, f32(k) / fx.resolution.y)).rgb);
            e = max(e, smoothstep(0.05, 0.2, l - above));
        }
        cover *= e;
    }
    let w = max(s, cover * 0.85);
    return vec4<f32>(mix(base.rgb, vec3<f32>(1.0), w), max(base.a, w));
}

fn e_sparkles(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Texture (Sparkle|Input colour), 1 Spawn Mode (Manual = Auto here), 2 Size (1-2), 3 Color (hue, 0 = white),
    // 4 Speed (twinkle), 5 Amount (1-2500 cells), 6 Spread (jiggle), 7 Threshold (luma 64-255 to spawn),
    // 8 Move, 9 Show Image
    let px = px_coord(uv);
    let cell = sqrt(fx.resolution.x * fx.resolution.y / max(P(5u), 1.0));
    let id0 = floor(px / cell);
    var glow = 0.0;
    var gcol = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let id = id0 + vec2<f32>(f32(x), f32(y));
            var pos = (id + vec2<f32>(rnd(id, 11u), rnd(id, 12u))) * cell;
            let ph = rnd(id, 13u) * TAU;
            if (PB(8u)) {
                pos += vec2<f32>(sin(fx.time * P(4u) + ph), cos(fx.time * P(4u) * 0.8 + ph)) * P(6u) * cell;
            }
            let s = tex(pos / fx.resolution);
            if (luma(s.rgb) * 255.0 < P(7u)) {
                continue;
            }
            let tw = pow(0.5 + 0.5 * sin(fx.time * P(4u) * 3.0 + ph), 4.0);
            let r = cell * 0.35 * P(2u) * tw;
            let d = px - pos;
            let core = pow(max(0.0, 1.0 - length(d) / max(r * 0.3, 1e-3)), 2.0);
            let ray = max(0.0, 1.0 - abs(d.x) / max(r, 1e-3)) * max(0.0, 1.0 - abs(d.y) / max(r * 0.06, 0.7))
                + max(0.0, 1.0 - abs(d.y) / max(r, 1e-3)) * max(0.0, 1.0 - abs(d.x) / max(r * 0.06, 0.7));
            let g = clamp(core + ray, 0.0, 1.0);
            if (g > glow) {
                glow = g;
                gcol = s.rgb;
            }
        }
    }
    var col = select(hsv2rgb(vec3<f32>(P(3u), 1.0, 1.0)), vec3<f32>(1.0), P(3u) <= 0.001);
    if (P(0u) > 0.5) {
        col = gcol;
    }
    let base = select(vec4<f32>(0.0, 0.0, 0.0, 1.0), c, PB(9u));
    return over(col, glow, base);
}

fn e_grid(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Width (of a cell), 1/2 Size X/Y (screen heights), 3 Fuzzyness, 4-7 Color
    let p = (uv - 0.5) * vec2<f32>(aspect(), 1.0);
    let cell = max(vec2<f32>(P(1u), P(2u)), vec2<f32>(0.005));
    let d = abs(fract(p / cell + 0.5) - 0.5) * cell;
    let m = min(cell.x, cell.y);
    let hw = P(0u) * m * 0.5;
    let fz = P(3u) * m * 0.5 + 1.0 / fx.resolution.y;
    let line = 1.0 - smoothstep(hw, hw + fz, min(d.x, d.y));
    let col = PC(4u);
    let k = line * col.a;
    return vec4<f32>(mix(c.rgb, col.rgb, k), max(c.a, k));
}

fn rotx(v: vec3<f32>, a: f32) -> vec3<f32> {
    let r = rot(v.yz, a);
    return vec3<f32>(v.x, r.x, r.y);
}
fn roty(v: vec3<f32>, a: f32) -> vec3<f32> {
    let r = rot(v.xz, a);
    return vec3<f32>(r.x, v.y, r.y);
}

fn e_point_grid(uv: vec2<f32>) -> vec4<f32> {
    // 0 Size, 1 Smooth Start (dot softness), 2 Black BG, 3 Space (bright dots grow), 4 Resolution (dot count),
    // 5-7 Rotate X/Y/Z (0.5 = flat): the image as a dot grid on a tilted plane
    let a = aspect();
    let bg = select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(2u));
    let p = rot((uv - 0.5) * vec2<f32>(a, 1.0), -(P(7u) - 0.5) * TAU);
    let tx = (P(5u) - 0.5) * PI * 0.66;
    let ty = (P(6u) - 0.5) * PI * 0.66;
    let f = 1.5;
    let dir = normalize(vec3<f32>(p, f));
    let n = roty(rotx(vec3<f32>(0.0, 0.0, -1.0), tx), ty);
    let den = dot(n, dir);
    if (abs(den) < 1e-4) {
        return bg;
    }
    let t = dot(n, vec3<f32>(0.0, 0.0, f)) / den;
    if (t <= 0.0) {
        return bg;
    }
    let hit = dir * t - vec3<f32>(0.0, 0.0, f);
    let q = rotx(roty(hit, -ty), -tx).xy; // plane coords (screen heights)
    let cell = 1.0 / mix(5.0, 60.0, P(4u));
    let ctr = (floor(q / cell) + 0.5) * cell;
    let suv = ctr / vec2<f32>(a, 1.0) + 0.5;
    if (any(suv < vec2<f32>(0.0)) || any(suv > vec2<f32>(1.0))) {
        return bg;
    }
    let s = tex(suv);
    let r = cell * 0.5 * mix(0.6, 1.0, P(0u)) * mix(1.0, luma(s.rgb) * 2.0, P(3u));
    let soft = P(1u) * r * 0.5 + 1.0 / fx.resolution.y;
    let m = (1.0 - smoothstep(r - soft, r, length(q - ctr))) * s.a;
    return over(s.rgb, m, bg);
}

fn e_circles(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Size (ring spacing), 1 Black BG: concentric halftone rings, thicker where brighter
    let p = (uv - 0.5) * vec2<f32>(aspect(), 1.0);
    let sp = mix(0.005, 0.1, P(0u));
    let f = fract(length(p) / sp);
    let w = luma(c.rgb) * 0.5;
    let soft = 1.0 / (fx.resolution.y * sp);
    let m = 1.0 - smoothstep(w, w + soft, abs(f - 0.5));
    let bg = select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(1u));
    return over(c.rgb, m * c.a, bg);
}

fn e_stripper(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Number (×50 stripes), 1 Mode (>0.5: image outside the stripes), 2 Direction (×360°),
    // 3 Direction Spread (stripe position jitter), 4 MovementType (scroll | breathe | jump),
    // 5 Speed, 6 SpeedSpread (per stripe), 7 GlobalWidth, 8/9 Min/MaxWidth, 10 Feather, 11-14 background RGBA
    let n = max(floor(P(0u) * 50.0), 1.0);
    let ang = P(2u) * TAU;
    var t = dot((uv - 0.5) * vec2<f32>(aspect(), 1.0), vec2<f32>(cos(ang), sin(ang))) * n;
    let mv = fx.time * P(5u) * 4.0;
    let mt = P(4u);
    var seed = 0u;
    if (mt < 0.333) {
        t += mv;
    } else if (mt >= 0.666) {
        seed = u32(max(mv, 0.0)) * 5u;
    }
    let i = vec2<f32>(floor(t), 0.0);
    var w = mix(P(8u), P(9u), rnd(i, seed + 1u)) * P(7u);
    if (mt >= 0.333 && mt < 0.666) {
        let sp = 1.0 + P(6u) * (rnd(i, 3u) - 0.5) * 2.0;
        w *= 0.5 + 0.5 * sin(mv * sp + rnd(i, 4u) * TAU);
    }
    let ctr = 0.5 + (rnd(i, seed + 2u) - 0.5) * P(3u) * (1.0 - w);
    let d = abs(fract(t) - ctr);
    let fe = P(10u) * 0.5 + n / fx.resolution.y;
    var m = 1.0 - smoothstep(w * 0.5 - fe, w * 0.5 + 1e-3, d);
    if (P(1u) > 0.5) {
        m = 1.0 - m;
    }
    let bg = vec4<f32>(P(11u), P(12u), P(13u), P(14u));
    return mix(bg, c, m);
}

fn e_heat(c: vec4<f32>) -> vec4<f32> {
    // 0 Heat (shifts the luma → thermal ramp; 0.5 = straight)
    let l = clamp(luma(c.rgb) * (0.5 + P(0u)) + (P(0u) - 0.5) * 0.5, 0.0, 1.0);
    var stops = array<vec3<f32>, 6>(
        vec3<f32>(0.0), vec3<f32>(0.1, 0.0, 0.5), vec3<f32>(0.7, 0.0, 0.6),
        vec3<f32>(1.0, 0.3, 0.0), vec3<f32>(1.0, 0.85, 0.0), vec3<f32>(1.0));
    let x = l * 5.0;
    let i = u32(min(floor(x), 4.0));
    return vec4<f32>(mix(stops[i], stops[i + 1u], x - f32(i)), c.a);
}

// ------------------------------------------------------------------ M5b batch 2 helpers

// Half-height units: x in [-aspect, aspect], y in [-1, 1] (y down).
fn hpos(uv: vec2<f32>) -> vec2<f32> {
    return (uv - 0.5) * 2.0 * vec2<f32>(aspect(), 1.0);
}
fn hpos_uv(p: vec2<f32>) -> vec2<f32> {
    return p / vec2<f32>(aspect(), 1.0) * 0.5 + 0.5;
}
fn black_or_clear(black: bool) -> vec4<f32> {
    return select(vec4<f32>(0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0), black);
}
fn premul(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}
// Premultiplied `top` over premultiplied `acc`.
fn over_pm(top: vec4<f32>, acc: vec4<f32>) -> vec4<f32> {
    return top + acc * (1.0 - top.a);
}
// Source drawn as a copy centred at `ctr` (half-height units), scaled and rotated; premultiplied.
// Premultiplied input `d` frames ago (0 = now); without a history ring: the current input.
fn hist_pm(uv: vec2<f32>, d: u32) -> vec4<f32> {
    if (fx.depth == 0u) {
        return textureSampleLevel(src_tex, samp, uv, 0.0);
    }
    let dd = min(d, fx.depth - 1u);
    let layer = (fx.head + fx.depth - dd) % fx.depth;
    return textureSampleLevel(hist_tex, samp, uv, i32(layer), 0.0);
}
fn copy_pm(p: vec2<f32>, ctr: vec2<f32>, scale: vec2<f32>, ang: f32) -> vec4<f32> {
    return copy_pm_d(p, ctr, scale, ang, 0u);
}
// Same, from the input `d` frames ago.
fn copy_pm_d(p: vec2<f32>, ctr: vec2<f32>, scale: vec2<f32>, ang: f32, d: u32) -> vec4<f32> {
    if (abs(scale.x) < 1e-4 || abs(scale.y) < 1e-4) {
        return vec4<f32>(0.0);
    }
    let q = rot(p - ctr, -ang) / scale;
    let uv = hpos_uv(q);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return vec4<f32>(0.0);
    }
    return hist_pm(uv, d);
}
fn unpremul_pm(c: vec4<f32>) -> vec4<f32> {
    return unpremul(c);
}

// Square (0..1)² → quad homography from 8 params at `o` (TL, TR, BR, BL as x,y pairs).
fn quad_h(o: u32) -> mat3x3<f32> {
    let x0 = P(o);
    let y0 = P(o + 1u);
    let x1 = P(o + 2u);
    let y1 = P(o + 3u);
    let x2 = P(o + 4u);
    let y2 = P(o + 5u);
    let x3 = P(o + 6u);
    let y3 = P(o + 7u);
    let sx = x0 - x1 + x2 - x3;
    let sy = y0 - y1 + y2 - y3;
    let dx1 = x1 - x2;
    let dx2 = x3 - x2;
    let dy1 = y1 - y2;
    let dy2 = y3 - y2;
    var den = dx1 * dy2 - dx2 * dy1;
    if (abs(den) < 1e-6) {
        den = 1e-6;
    }
    let g = (sx * dy2 - dx2 * sy) / den;
    let h = (dx1 * sy - sx * dy1) / den;
    return mat3x3<f32>(
        vec3<f32>(x1 - x0 + g * x1, y1 - y0 + g * y1, g),
        vec3<f32>(x3 - x0 + h * x3, y3 - y0 + h * y3, h),
        vec3<f32>(x0, y0, 1.0));
}
fn inv3(m: mat3x3<f32>) -> mat3x3<f32> {
    let r0 = cross(m[1], m[2]);
    let r1 = cross(m[2], m[0]);
    let r2 = cross(m[0], m[1]);
    let det = dot(m[0], r0);
    return transpose(mat3x3<f32>(r0, r1, r2)) * (1.0 / select(det, 1e-6, abs(det) < 1e-9));
}
fn hmap(m: mat3x3<f32>, p: vec2<f32>) -> vec2<f32> {
    let r = m * vec3<f32>(p, 1.0);
    return r.xy / select(r.z, 1e-6, abs(r.z) < 1e-6);
}
fn inside01(s: vec2<f32>) -> bool {
    return all(s >= vec2<f32>(0.0)) && all(s <= vec2<f32>(1.0));
}
fn grid_lines(s: vec2<f32>) -> f32 {
    let g = abs(fract(s * 10.0 + 0.5) - 0.5);
    return 1.0 - smoothstep(0.01, 0.025, min(g.x, g.y));
}

// Point on a plane tilted by (tx, ty) seen from a camera at distance f; None → w < 0.
fn plane_hit(p: vec2<f32>, tx: f32, ty: f32, f: f32) -> vec3<f32> {
    let dir = normalize(vec3<f32>(p, f));
    let n = roty(rotx(vec3<f32>(0.0, 0.0, -1.0), tx), ty);
    let den = dot(n, dir);
    if (abs(den) < 1e-4) {
        return vec3<f32>(0.0, 0.0, -1.0);
    }
    let t = dot(n, vec3<f32>(0.0, 0.0, f)) / den;
    if (t <= 0.0) {
        return vec3<f32>(0.0, 0.0, -1.0);
    }
    let hit = dir * t - vec3<f32>(0.0, 0.0, f);
    return vec3<f32>(rotx(roty(hit, -ty), -tx).xy, 1.0);
}

// ------------------------------------------------------------------ M5b batch 2: warps

fn e_bendoscope(uv: vec2<f32>) -> vec4<f32> {
    // 0 Divisions (segments 2..42), 1 Black BG: kaleidoscope whose segments bend with the radius
    let p = hpos(uv);
    let n = 2.0 + floor(P(0u) * 40.0 + 0.5);
    let seg = TAU / n;
    let r = length(p);
    var a = atan2(p.y, p.x) + r * 0.6;
    a = a - floor(a / seg) * seg;
    a = min(a, seg - a);
    let q = vec2<f32>(cos(a), sin(a)) * r;
    let s = hpos_uv(q * 0.8);
    if (PB(1u)) {
        return tex_bg(s, true);
    }
    return tex(mirror_uv(s));
}

fn e_distortion(uv: vec2<f32>) -> vec4<f32> {
    // 0 Distort (bulge strength), 1 Radius (of the lens, half-heights)
    let p = hpos(uv);
    let rr = max(P(1u) * 2.0, 1e-3);
    let r = length(p);
    var q = p;
    if (r < rr) {
        q = p * pow(r / rr, P(0u) * 1.5);
    }
    return tex_bg(hpos_uv(q), false);
}

fn e_goo(uv: vec2<f32>) -> vec4<f32> {
    // 0 Resolution (4-128 noise cells), 1 Speed, 2-4 MaxDistortion X/Y/Z (Z = local zoom), 5 Black BG
    let g = uv * P(0u) * 0.25 * vec2<f32>(aspect(), 1.0);
    let t = fx.time * P(1u) * 4.0;
    let nx = vnoise(g + vec2<f32>(t, 0.0), 21u) - 0.5;
    let ny = vnoise(g + vec2<f32>(0.0, t), 22u) - 0.5;
    let nz = vnoise(g - vec2<f32>(t, t), 23u) - 0.5;
    var q = uv + vec2<f32>(nx * P(2u), ny * P(3u)) * 0.5;
    q = (q - uv) + mix(uv, vec2<f32>(0.5), nz * P(4u));
    if (PB(5u)) {
        return tex_bg(q, true);
    }
    return tex(mirror_uv(q));
}

fn e_ripples(uv: vec2<f32>) -> vec4<f32> {
    // 0 Frequency H, 1 Amplitude H, 2 Frequency V, 3 Amplitude V, 4 Amplitude Both, 5 Phase, 6 Black BG
    let ph = P(5u) * TAU;
    let dx = sin(uv.y * P(0u) * 60.0 + ph) * P(1u) * 0.05 * P(4u);
    let dy = sin(uv.x * P(2u) * 60.0 + ph) * P(3u) * 0.05 * P(4u);
    let q = uv + vec2<f32>(dx, dy);
    if (PB(6u)) {
        return tex_bg(q, true);
    }
    return tex(mirror_uv(q));
}

fn e_twisted(uv: vec2<f32>) -> vec4<f32> {
    // 0 Twirl (0.5 = none), 1 Radius, 2 Black BG
    let p = hpos(uv);
    let rr = max(P(1u) * 4.0, 1e-3);
    let k = max(1.0 - length(p) / rr, 0.0);
    let q = rot(p, (P(0u) - 0.5) * 4.0 * PI * k * k);
    return tex_bg(hpos_uv(q), PB(2u));
}

fn e_tunnel(uv: vec2<f32>) -> vec4<f32> {
    // 0 Position (depth), 1 Speed, 2 Twist, 4 Rotation (0.5 = none, bipolar), 3 Kaleidoscope (mirrored
    // repeats round the wall), 5/6 TurnH/V (look direction), 7/8 WobbleH/V, 9 Black BG
    var p = hpos(uv);
    p -= vec2<f32>(P(5u) - 0.5, P(6u) - 0.5) * 2.0;
    let r = max(length(p), 1e-4);
    let depth = 0.35 / r;
    let z = depth + P(0u) * 4.0 + fx.time * (P(1u) - 0.5) * 4.0;
    p += vec2<f32>(sin(z * 2.0) * P(7u), cos(z * 1.7) * P(8u)) * r;
    var a = atan2(p.y, p.x) / TAU + 0.5;
    a += (P(2u) - 0.5) * depth * 0.5 + fx.time * (P(4u) - 0.5) * 0.5;
    let reps = 1.0 + floor(P(3u) * 8.0);
    let q = vec2<f32>(a * reps * 2.0, z);
    let c = tex(mirror_uv(q));
    // Far end fades out.
    let fog = smoothstep(0.0, 0.08, r);
    if (PB(9u)) {
        return vec4<f32>(c.rgb * fog, 1.0);
    }
    return vec4<f32>(c.rgb, c.a * fog);
}

fn e_suckr(uv: vec2<f32>) -> vec4<f32> {
    // 0 Suck (pull towards the centre), 1 Black BG
    let p = hpos(uv);
    let r = length(p);
    let q = p * (1.0 + P(0u) * 3.0 * (1.0 - smoothstep(0.0, 1.2, r)));
    return tex_bg(hpos_uv(q), PB(1u));
}

fn e_blow(uv: vec2<f32>) -> vec4<f32> {
    // 0/1 X/Y (centre), 2/3 Width/Height of the bulge
    let c = vec2<f32>(P(0u), P(1u));
    let rad = max(vec2<f32>(P(2u), P(3u)) * 0.5, vec2<f32>(1e-3));
    let d = (uv - c) / rad;
    let r = length(d);
    var q = uv;
    if (r < 1.0) {
        q = c + d * rad * mix(0.3, 1.0, r * r);
    }
    return tex_bg(q, false);
}

fn e_slide(uv: vec2<f32>) -> vec4<f32> {
    // 0/1 X/Y offset (wraps), 2 Black BG (no wrap)
    let q = uv - vec2<f32>(P(0u), P(1u));
    if (PB(2u)) {
        return tex_bg(q, true);
    }
    return tex(fract(q));
}

fn e_fragment(uv: vec2<f32>) -> vec4<f32> {
    // 0 Fragments (count), 1 Distance (fly-out), 2 Rotation (whole), 3-5 FragRotation X/Y/Z (0.5 = none),
    // 6 FragScale (0.5 = 1×), 7 CompositeOpacity, 8 Black BG
    let p = rot(hpos(uv), -P(2u) * TAU);
    let cell = 2.0 / (3.0 + floor(P(0u) * 40.0));
    let id0 = floor(p / cell);
    var acc = vec4<f32>(0.0);
    let sc = vec2<f32>(cos((P(4u) - 0.5) * PI), cos((P(3u) - 0.5) * PI)) * exp2((P(6u) - 0.5) * 2.0);
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let id = id0 + vec2<f32>(f32(x), f32(y));
            let home = (id + 0.5) * cell;
            let dir = normalize(home + vec2<f32>(1e-3));
            let ctr = home + dir * P(1u) * cell * 2.0 * rnd(id, 31u);
            let ang = (P(5u) - 0.5) * TAU * (rnd(id, 32u) - 0.5) * 2.0;
            let l = rot(p - ctr, -ang) / max(abs(sc), vec2<f32>(1e-3));
            if (all(abs(l) <= vec2<f32>(cell * 0.5))) {
                let s = textureSampleLevel(src_tex, samp, hpos_uv(home + l), 0.0);
                acc = over_pm(s * P(7u), acc);
            }
        }
    }
    let bg = premul(black_or_clear(PB(8u)));
    return unpremul(over_pm(acc, bg));
}

fn e_triangulate(uv: vec2<f32>) -> vec4<f32> {
    // 0 Scale (largest triangle), 1 Levels (subdivisions where detailed), 2 Flip Direction,
    // 3 Threshold (0 = no subdivision; higher = subdivide at lower contrast)
    let px = uv * vec2<f32>(aspect(), 1.0);
    var size = max(P(0u), 0.005);
    let levels = i32(P(1u) + 0.5);
    for (var l = 1; l < levels; l++) {
        let c = (floor(px / size) + 0.5) * size;
        let k = size * 0.5;
        let a = luma(tex(vec2<f32>(c.x - k, c.y - k) / vec2<f32>(aspect(), 1.0)).rgb);
        let b = luma(tex(vec2<f32>(c.x + k, c.y + k) / vec2<f32>(aspect(), 1.0)).rgb);
        let d = luma(tex(vec2<f32>(c.x + k, c.y - k) / vec2<f32>(aspect(), 1.0)).rgb);
        let e = luma(tex(vec2<f32>(c.x - k, c.y + k) / vec2<f32>(aspect(), 1.0)).rgb);
        if (max(max(a, b), max(d, e)) - min(min(a, b), min(d, e)) < 1.0 - P(3u) || size < 0.004) {
            break;
        }
        size *= 0.5;
    }
    let id = floor(px / size);
    var f = fract(px / size);
    let flip = PB(2u) != ((i32(id.x + id.y) & 1) == 1);
    if (flip) {
        f.x = 1.0 - f.x;
    }
    // Two triangles per cell; sample at the centroid.
    var cen = select(vec2<f32>(2.0 / 3.0, 1.0 / 3.0), vec2<f32>(1.0 / 3.0, 2.0 / 3.0), f.y > f.x);
    if (flip) {
        cen.x = 1.0 - cen.x;
    }
    return tex((id + cen) * size / vec2<f32>(aspect(), 1.0));
}

fn e_cube_tiles(uv: vec2<f32>) -> vec4<f32> {
    // 0 Resolution (tiles), 1 Zoom (tile size), 2 Amount (rotation), 3-5 Rotate X/Y/Z (0.5 = none),
    // 6 Num Cubes (share of still tiles: 0 = all turn), 7 Black BG. Tiles turn like cards.
    let n = 2.0 + floor(P(0u) * 30.0);
    let a = aspect();
    let g = uv * vec2<f32>(n * a, n);
    let id = floor(g);
    let l = (fract(g) - 0.5) * 2.0;
    var turn = P(2u) * (0.5 + rnd(id, 41u));
    if (rnd(id, 42u) < P(6u)) {
        turn = 0.0;
    }
    let tx = (P(3u) - 0.5) * TAU * turn;
    let ty = (P(4u) - 0.5) * TAU * turn;
    let tz = (P(5u) - 0.5) * TAU * turn;
    let h = plane_hit(rot(l, -tz) / mix(0.5, 1.0, P(1u)), tx, ty, 3.0);
    let bg = black_or_clear(PB(7u));
    if (h.z < 0.0 || any(abs(h.xy) > vec2<f32>(1.0))) {
        return bg;
    }
    let s = tex((id + h.xy * 0.5 + 0.5) / vec2<f32>(n * a, n));
    let shade = mix(1.0, abs(cos(tx) * cos(ty)), 0.5);
    return over(s.rgb * shade, s.a, bg);
}

fn e_video_wall(uv: vec2<f32>) -> vec4<f32> {
    // 0 Screens (per row), 1 Zoom, 2-4 Rotate X/Y/Z (0.5 = flat), 5/6 Pan X/Y, 7 SubscreenSize
    // (0 = each screen shows the whole picture, 1 = the screens share it), 8 SubscreenMode (unused),
    // 9 GapSize, 10 BrightnessVary, 11 Black BG
    let bg = black_or_clear(PB(11u));
    let a = aspect();
    let p = rot(hpos(uv), -(P(4u) - 0.5) * TAU);
    let h = plane_hit(p, (P(2u) - 0.5) * PI * 0.8, (P(3u) - 0.5) * PI * 0.8, 2.0);
    if (h.z < 0.0) {
        return bg;
    }
    let n = 1.0 + floor(P(0u) * 12.0);
    let q = h.xy / exp2((P(1u) - 0.3) * 3.0) + vec2<f32>(P(5u) - 0.5, P(6u) - 0.5) * 4.0;
    let cell = vec2<f32>(2.0 * a, 2.0) / n;
    let g = q / cell + n * 0.5;
    let id = floor(g);
    let f = fract(g);
    let gap = P(9u) * 0.2;
    if (any(f < vec2<f32>(gap)) || any(f > vec2<f32>(1.0 - gap))) {
        return bg;
    }
    let lf = (f - gap) / (1.0 - 2.0 * gap);
    let whole = lf;
    let shared_ = (id + lf) / n;
    let s = tex(mix(whole, fract(shared_), P(7u)));
    let b = 1.0 - P(10u) * rnd(id, 51u);
    return over(s.rgb * b, s.a, bg);
}

fn e_infinite_zoom(uv: vec2<f32>) -> vec4<f32> {
    // 0 Rotation Speed, 1 Zoom (speed, ±), 2 Range (zoom doublings across the slices), 3 Alpha, 4 Num Slices
    let p = hpos(uv);
    let n = max(floor(P(4u) + 0.5), 2.0);
    var acc = vec4<f32>(0.0);
    let ph0 = fx.time * P(1u) * 2.0;
    for (var k = 0; k < 30; k++) {
        if (f32(k) >= n) {
            break;
        }
        // back (small) to front (big)
        let ph = fract(f32(k) / n + ph0);
        let sc = exp2((ph - 0.5) * P(2u));
        let fade = smoothstep(0.0, 0.15, ph) * (1.0 - smoothstep(0.85, 1.0, ph));
        let c = copy_pm(p, vec2<f32>(0.0), vec2<f32>(sc), fx.time * P(0u) * (ph - 0.5));
        let al = mix(1.0, P(3u), 0.5) * fade;
        acc = over_pm(c * al, acc);
    }
    return unpremul(acc);
}

fn e_warp_speed(uv: vec2<f32>) -> vec4<f32> {
    // 15 copies, scale linspace(Scale, 1), opacity linspace(1, Fade), rotation by Rotate
    // growing per copy, newest/smallest on top, centred at Center X/Y; each copy vignetted.
    // Copy i shows the input i frames ago (history ring).
    // 0 Vignette, 1 Fade, 2 Scale, 3 Rotate (-1..1), 4/5 Center X/Y (-1..1)
    let p = hpos(uv);
    let a = aspect();
    var acc = vec4<f32>(0.0);
    for (var i = 14; i >= 0; i--) {
        let k = f32(i) / 14.0;
        let sc = mix(max(P(2u), 0.1), 1.0, k);
        let ctr = vec2<f32>(P(4u) * a, P(5u)) * (1.0 - sc);
        let c = copy_pm_d(p, ctr, vec2<f32>(sc), P(3u) * PI * k, u32(i));
        // vignette: soft edges of each copy
        let l = rot(p - ctr, -P(3u) * PI * k) / (sc * vec2<f32>(a, 1.0));
        let e = max(abs(l.x), abs(l.y));
        let v = 1.0 - smoothstep(1.0 - max(P(0u), 0.01), 1.0, e);
        acc = over_pm(c * v * mix(1.0, P(1u), k), acc);
    }
    return unpremul(acc);
}

fn e_radar(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Angle (sweep, ×360°; animate it), 1 Trail Length, 2 Fade (floor of the trail), 3 Show Grid, 4 Grid Scale
    let p = hpos(uv);
    var d = (atan2(p.y, p.x) / TAU) - P(0u);
    d = fract(-d); // 0 at the beam, growing behind it
    let tl = max(P(1u), 1e-3);
    var k = max(1.0 - d / tl, 0.0);
    k = max(k * k, P(2u));
    var o = vec4<f32>(c.rgb, c.a * k);
    if (PB(3u)) {
        let r = length(p) / max(P(4u), 0.05);
        let ring = 1.0 - smoothstep(0.0, 0.02 / max(P(4u), 0.05), abs(fract(r + 0.5) - 0.5) * P(4u));
        let ax = 1.0 - smoothstep(0.002, 0.006, min(abs(p.x), abs(p.y)));
        let g = max(ring, ax) * 0.6;
        o = over(vec3<f32>(0.3, 1.0, 0.4), g, o);
    }
    return o;
}

fn e_search_light(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // a circle moved by two Perlin noises (Frequency), Size = radius and shrinks the
    // range of motion, Blur = anti-aliasing 4..500 px, mask. 0 Size, 1 Blur, 2 Frequency, 3 Invert
    let t = fx.time * P(2u);
    let range = (1.0 - P(0u)) * vec2<f32>(aspect(), 1.0);
    let ctr = (vec2<f32>(vnoise(vec2<f32>(t, 0.5), 61u), vnoise(vec2<f32>(t, 7.5), 62u)) - 0.5) * 2.0 * range;
    let r = P(0u);
    let blur = mix(4.0, 500.0, P(1u)) / fx.resolution.y;
    var m = 1.0 - smoothstep(r - blur * 0.5, r + blur * 0.5, length(hpos(uv) - ctr));
    if (PB(3u)) {
        m = 1.0 - m;
    }
    return vec4<f32>(c.rgb, c.a * m);
}

fn e_reveal(uv: vec2<f32>) -> vec4<f32> {
    // 0 Phase, 1/2 X/Y tiles, 3 Border (px), 4 Texture Full|Color (tile = its mean colour), 5 Stretch
    // (whole image per tile), 6 Fade, 7 Scaling None|Centre|Up|Right|Corner, 8 Order Normal|Random,
    // 9 Seed (Audio = Normal here), 10 Black BG
    let n = max(floor(vec2<f32>(P(1u), P(2u)) + 0.5), vec2<f32>(1.0));
    let g = uv * n;
    let id = floor(g);
    let f = fract(g);
    let count = n.x * n.y;
    var order = (id.y * n.x + id.x) / count;
    if (P(8u) > 0.5) {
        order = rnd(id, 71u);
    }
    let w = 0.25;
    let k = clamp((P(0u) * (1.0 + w) - order) / w, 0.0, 1.0);
    let bg = black_or_clear(PB(10u));
    var l = f;
    switch u32(P(7u) + 0.5) {
        case 1u: { l = (f - 0.5) / max(k, 1e-3) + 0.5; }
        case 2u: { l = vec2<f32>(f.x, (f.y - 1.0) / max(k, 1e-3) + 1.0); }
        case 3u: { l = vec2<f32>(f.x / max(k, 1e-3), f.y); }
        case 4u: { l = f / max(k, 1e-3); }
        default: {}
    }
    let border = P(3u) * n / fx.resolution;
    if (k <= 0.0 || !inside01(l) || any(f < border * 0.5) || any(f > 1.0 - border * 0.5)) {
        return bg;
    }
    var s: vec4<f32>;
    if (PB(5u)) {
        s = tex(l);
    } else {
        s = tex((id + l) / n);
    }
    if (P(4u) > 0.5) {
        s = tex((id + 0.5) / n);
    }
    let al = select(1.0, k, PB(6u));
    return over(s.rgb, s.a * al, bg);
}

fn e_keystone(uv: vec2<f32>) -> vec4<f32> {
    // 0-7 corners TL,TR,BR,BL (x,y in 0..1), 8 Show Grid, 9 Black BG: picture pinned into the quad
    let s = hmap(inv3(quad_h(0u)), uv);
    let bg = black_or_clear(PB(9u));
    if (!inside01(s)) {
        return bg;
    }
    var c = tex(s);
    if (PB(8u)) {
        c = over(vec3<f32>(1.0), grid_lines(s), c);
    }
    return over(c.rgb, c.a, bg);
}

fn e_keystone_crop(uv: vec2<f32>) -> vec4<f32> {
    // 0-7 corners: that quad of the picture is stretched to the full frame; 8 Show Grid, 9 Black BG
    let s = hmap(quad_h(0u), uv);
    var c = tex_bg(s, PB(9u));
    if (PB(8u)) {
        c = over(vec3<f32>(1.0), grid_lines(uv), c);
    }
    return c;
}

fn e_keystone_mask(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0-7 corners: the quad is cut out of the picture; 8 Show Grid, 9 Black BG
    let s = hmap(inv3(quad_h(0u)), uv);
    if (inside01(s)) {
        let bg = black_or_clear(PB(9u));
        if (PB(8u)) {
            return over(vec3<f32>(1.0), grid_lines(s), bg);
        }
        return bg;
    }
    return c;
}

fn e_shift_glitch(uv: vec2<f32>) -> vec4<f32> {
    // 0 Frequency (re-rolls per second ×30), 1 Vertical, 2 Horizontal (shift amounts), 3 Size (band size), 4 Black BG
    let t = u32(fx.time * P(0u) * 30.0) * 7u;
    let bands = mix(60.0, 3.0, P(3u));
    let by = floor(uv.y * bands);
    let bx = floor(uv.x * bands);
    var q = uv;
    if (rnd(vec2<f32>(by, 1.0), t) < 0.4) {
        q.x += (rnd(vec2<f32>(by, 2.0), t) - 0.5) * P(2u);
    }
    if (rnd(vec2<f32>(bx, 3.0), t) < 0.4) {
        q.y += (rnd(vec2<f32>(bx, 4.0), t) - 0.5) * P(1u);
    }
    if (PB(4u)) {
        return tex_bg(q, true);
    }
    return tex(fract(q));
}

fn e_linear_cloner(uv: vec2<f32>) -> vec4<f32> {
    // 0 Clones (1-50, shared by the tails), 1 Tails (spread round the circle), 2 Center (shift along
    // the tail), 3 Tail Length (±, half-widths ×2), 4/6 Start/End Scale %, 5/7 Start/End Rotation °,
    // 8 Distribution % (50 = even), 9 Distortion (tail bend °), 10 Reverse (draw order), 11 Tail Rotation
    // (twist of the tails %), 12 Master Rotation °, 13 Delay Enabled, 14 Delay Amount (up to 15 frames),
    // 15 Delay Order New To Old|Old To New|Bounce|Bounce Tail|Random, 16 Delay Fill (unused)
    let p = rot(hpos(uv), -radians(P(12u)));
    let a = aspect();
    let tails = max(floor(P(1u) + 0.5), 1.0);
    let per = max(floor(P(0u) / tails + 0.5), 1.0);
    var acc = vec4<f32>(0.0);
    let ex = exp2((P(8u) - 50.0) / 25.0);
    for (var j = 0; j < 8; j++) {
        if (f32(j) >= tails) {
            break;
        }
        let ta = f32(j) / tails * TAU;
        for (var ii = 0; ii < 50; ii++) {
            if (f32(ii) >= per) {
                break;
            }
            // tips first, centre clone last (on top) unless Reverse
            var i = per - 1.0 - f32(ii);
            if (PB(10u)) {
                i = f32(ii);
            }
            let k = select(i / (per - 1.0), 0.0, per <= 1.0);
            let kk = pow(k, ex);
            let along = (kk + P(2u)) * P(3u) * 2.0 * a;
            let bend = radians(P(9u)) * kk * 0.3 + ta + P(11u) / 100.0 * TAU * kk;
            let ctr = vec2<f32>(cos(bend), sin(bend)) * along;
            let sc = mix(P(4u), P(6u), kk) / 100.0;
            let ang = radians(mix(P(5u), P(7u), kk)) + ta;
            var dl = 0u;
            if (PB(13u)) {
                var f = k;
                switch u32(P(15u) + 0.5) {
                    case 1u: { f = 1.0 - k; }
                    case 2u: { f = 1.0 - abs(2.0 * k - 1.0); }
                    case 3u: { f = abs(2.0 * k - 1.0); }
                    case 4u: { f = rnd(vec2<f32>(i, f32(j)), 5u); }
                    default: {}
                }
                dl = u32(round(f * P(14u) * 15.0));
            }
            acc = over_pm(copy_pm_d(p, ctr, vec2<f32>(sc), ang, dl), acc);
        }
    }
    return unpremul(acc);
}

fn e_grid_cloner(uv: vec2<f32>) -> vec4<f32> {
    // Columns×Rows copies on a grid Width×Height (half-width units), each copy the full
    // frame × Input Scale % and Input Rotation; the grid × Scale %, Rotation, shifts (Vertical /
    // Horizontal shift alternate rows / columns, Random Shift jitters); Background colour behind.
    // 0 Columns, 1 Rows, 2 Input Rotation, 3 Input Scale, 4 Width, 5 Height, 6 Scale, 7 Rotation,
    // 8 Vertical Shift, 9 Horizontal Shift, 10 Random Shift, 11 Order Center|Top Right (delay grows from
    // there), 12-15 Background, 16 Delay (older frames further out, up to 15)
    let a = aspect();
    let gs = max(P(6u) / 100.0, 1e-3);
    let p = rot(hpos(uv), -radians(P(7u))) / gs;
    let cols = max(floor(P(0u) + 0.5), 1.0);
    let rows = max(floor(P(1u) + 0.5), 1.0);
    let ext = vec2<f32>(P(4u), P(5u)) * a;
    var acc = vec4<f32>(0.0);
    let isc = P(3u) / 100.0;
    for (var r = 0; r < 10; r++) {
        if (f32(r) >= rows) {
            break;
        }
        for (var c = 0; c < 10; c++) {
            if (f32(c) >= cols) {
                break;
            }
            let id = vec2<f32>(f32(c), f32(r));
            let k = select(id / (vec2<f32>(cols, rows) - 1.0), vec2<f32>(0.5), vec2<f32>(cols, rows) <= vec2<f32>(1.0));
            var ctr = (k - 0.5) * ext;
            ctr.x += P(9u) * a * f32(r % 2) * ext.x / max(cols, 1.0);
            ctr.y += P(8u) * f32(c % 2) * ext.y / max(rows, 1.0);
            ctr += (vec2<f32>(rnd(id, 81u), rnd(id, 82u)) - 0.5) * P(10u) * ext / vec2<f32>(cols, rows);
            var dl = 0u;
            if (PB(16u)) {
                var f = length(k - 0.5) / 0.7072;
                if (P(11u) > 0.5) {
                    f = length(k - vec2<f32>(1.0, 0.0)) / 1.4143;
                }
                dl = u32(round(clamp(f, 0.0, 1.0) * 15.0));
            }
            acc = over_pm(copy_pm_d(p, ctr, vec2<f32>(isc), radians(P(2u)), dl), acc);
        }
    }
    let bg = PC(12u);
    return unpremul(over_pm(acc, premul(bg)));
}

fn e_radial_cloner(uv: vec2<f32>) -> vec4<f32> {
    // A ring of Clones segments between Start and End Radius (half-heights),
    // placed at angles Start→End Rotation; each segment holds the whole picture, zoomed Start→End Scale
    // from inner to outer edge. 0 Clones, 1 Start Radius, 2 Start Scale, 3 Start Rotation, 4 End Radius,
    // 5 End Scale, 6 End Rotation, 7 Fan % (segment width), 8 Fan Offset %, 9 Master Rotation,
    // 10 Delay Enabled, 11 Delay Amount, 12 Delay Order New To Old|Old To New|Bounce|Random, 13 Delay Fill (unused)
    let p = hpos(uv);
    let n = max(floor(P(0u) + 0.5), 1.0);
    let span = radians(P(6u) - P(3u));
    let seg = span / n;
    var a = atan2(p.y, p.x) - radians(P(9u)) - radians(P(3u)) + PI * 0.5;
    a = a - floor(a / TAU) * TAU;
    let i = floor(a / max(abs(seg), 1e-4));
    if (i >= n) {
        return vec4<f32>(0.0);
    }
    let mid = (i + 0.5) * seg;
    let la = a - mid; // angle from the segment centre
    let half = abs(seg) * 0.5 * P(7u) / 100.0;
    let rp = length(p) * cos(la); // straight edges
    let r0 = P(1u);
    let r1 = P(4u);
    let v = (rp - r0) / max(r1 - r0, 1e-4);
    let w = tan(half) * rp;
    let u = (length(p) * sin(la)) / max(w, 1e-4) * 0.5 + 0.5 + P(8u) / 100.0;
    if (v < 0.0 || v > 1.0 || abs(la) > half) {
        return vec4<f32>(0.0);
    }
    let zoom = max(mix(P(2u), P(5u), v) / 100.0, 1e-3);
    let s = (vec2<f32>(fract(u), 1.0 - v) - 0.5) * zoom + 0.5;
    var dl = 0u;
    if (PB(10u)) {
        let k = i / max(n - 1.0, 1.0);
        var f = k;
        switch u32(P(12u) + 0.5) {
            case 1u: { f = 1.0 - k; }
            case 2u: { f = 1.0 - abs(2.0 * k - 1.0); }
            case 3u: { f = rnd(vec2<f32>(i, 0.0), 6u); }
            default: {}
        }
        dl = u32(round(f * P(11u) * 15.0));
    }
    return unpremul(hist_pm(s, dl));
}

// ------------------------------------------------------------------ M5b batch 2: spheres

struct SphereHit {
    uv: vec2<f32>,
    light: f32,
    hit: f32,
    lat: vec2<f32>,
};

fn sph_rot(p: vec3<f32>, r: vec3<f32>) -> vec3<f32> {
    var q = rotx(p, r.x);
    q = roty(q, r.y);
    let z = rot(q.xy, r.z);
    return vec3<f32>(z, q.z);
}
fn sph_unrot(p: vec3<f32>, r: vec3<f32>) -> vec3<f32> {
    let z = rot(p.xy, -r.z);
    var q = roty(vec3<f32>(z, p.z), -r.y);
    return rotx(q, -r.x);
}
fn sph_uv(d: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(atan2(d.x, -d.z) / TAU + 0.5, acos(clamp(d.y, -1.0, 1.0)) / PI);
}
fn sph_h(d: vec3<f32>, inv: bool, namt: f32, phase: f32) -> f32 {
    let suv = sph_uv(d);
    var h = luma(textureSampleLevel(src_tex, samp, suv, 0.0).rgb);
    if (inv) {
        h = 1.0 - h;
    }
    return mix(h, vnoise(suv * vec2<f32>(24.0, 12.0) + phase * 10.0, 91u), namt);
}
fn sph_sdf(p: vec3<f32>, rr: f32, ext: f32, inv: bool, namt: f32, phase: f32) -> f32 {
    let l = max(length(p), 1e-4);
    return (l - rr - ext * sph_h(p / l, inv, namt, phase)) * 0.6;
}
// Raymarched sphere textured with the input and pushed out by its brightness.
fn sphere_trace(uv: vec2<f32>, rr: f32, ext: f32, zoom: f32, r: vec3<f32>, inv: bool, namt: f32, phase: f32) -> SphereHit {
    var o: SphereHit;
    o.hit = 0.0;
    let cam = mix(6.0, 1.6, zoom);
    let ro = vec3<f32>(0.0, 0.0, -cam);
    let rd = normalize(vec3<f32>(hpos(uv) * 0.5, 1.0));
    var t = max(cam - rr - ext - 0.1, 0.0);
    let tmax = cam + rr + ext;
    for (var i = 0; i < 64; i++) {
        let pw = ro + rd * t;
        let pl = sph_rot(pw, r);
        let d = sph_sdf(pl, rr, ext, inv, namt, phase);
        if (d < 0.002) {
            let e = 0.01;
            let n = normalize(vec3<f32>(
                sph_sdf(pl + vec3<f32>(e, 0.0, 0.0), rr, ext, inv, namt, phase) - sph_sdf(pl - vec3<f32>(e, 0.0, 0.0), rr, ext, inv, namt, phase),
                sph_sdf(pl + vec3<f32>(0.0, e, 0.0), rr, ext, inv, namt, phase) - sph_sdf(pl - vec3<f32>(0.0, e, 0.0), rr, ext, inv, namt, phase),
                sph_sdf(pl + vec3<f32>(0.0, 0.0, e), rr, ext, inv, namt, phase) - sph_sdf(pl - vec3<f32>(0.0, 0.0, e), rr, ext, inv, namt, phase)));
            // light from front-top-left, in world space
            let nw = sph_unrot(n, r);
            o.light = max(dot(nw, normalize(vec3<f32>(-0.4, -0.6, -1.0))), 0.0);
            let dl = normalize(pl);
            o.uv = sph_uv(dl);
            o.lat = vec2<f32>(atan2(dl.x, -dl.z), acos(clamp(dl.y, -1.0, 1.0)));
            o.hit = 1.0;
            return o;
        }
        t += d;
        if (t > tmax) {
            break;
        }
    }
    return o;
}

fn e_sphere(uv: vec2<f32>) -> vec4<f32> {
    // 0 Size, 1 Resolution (facets), 2 Extrusion, 3 Invert, 4 NoisePhase, 5 NoiseType (noise mix 0/.5/1),
    // 6-9 ColorValue, 10 ColorMode Add|Multiply|Interpolate, 11 ColorContrast, 12 ThresholdValue,
    // 13 ThresholdMode Color|Greyscale|Transparent, 14 Zoom, 15-17 Rotation X/Y/Z °, 18 Black BG
    let bg = black_or_clear(PB(18u));
    let rr = 0.3 + P(0u) * 2.0;
    let h = sphere_trace(uv, rr, P(2u), P(14u), radians(vec3<f32>(P(15u), P(16u), P(17u))), PB(3u), P(5u) * 0.5, P(4u));
    if (h.hit < 0.5) {
        return bg;
    }
    let steps = mix(256.0, 12.0, P(1u));
    let suv = (floor(h.uv * steps) + 0.5) / steps;
    var s = tex(suv);
    let col = PC(6u).rgb;
    var rgb = s.rgb;
    switch u32(P(10u) + 0.5) {
        case 0u: { rgb = rgb + col; }
        case 1u: { rgb = rgb * col; }
        default: { rgb = mix(rgb, col, 0.5); }
    }
    rgb = (rgb - 0.5) * P(11u) / 1.5 + 0.5;
    rgb *= mix(0.35, 1.0, h.light);
    var a = s.a;
    if (luma(s.rgb) < P(12u)) {
        switch u32(P(13u) + 0.5) {
            case 0u: { rgb = col * mix(0.35, 1.0, h.light); }
            case 1u: { rgb = vec3<f32>(luma(rgb)); }
            default: { a = 0.0; }
        }
    }
    return over(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), a, bg);
}

fn e_stingy_sphere(uv: vec2<f32>) -> vec4<f32> {
    // 0 Sphere size, 1 Extrusion (spikes by brightness), 2 Noise, 3 Noise Amount, 4 Light, 5 Light Flat,
    // 6 Min Light, 7 Saturate, 8 Saturate Amount, 9 Wired (lat/long lines only), 10 Invert, 11 Zoom,
    // 12 Resolution, 13-15 Rotate X/Y/Z °, 16 Black BG. FFT-driven spikes (FFT/Input/Gain) not yet.
    let bg = black_or_clear(PB(16u));
    let rr = 0.3 + P(0u) * 1.0;
    let namt = select(0.0, P(3u), PB(2u));
    let h = sphere_trace(uv, rr, P(1u) * 0.8, P(11u), radians(vec3<f32>(P(13u), P(14u), P(15u))), PB(10u), namt, fx.time * 0.1);
    if (h.hit < 0.5) {
        return bg;
    }
    let steps = mix(12.0, 256.0, P(12u));
    var rgb = tex((floor(h.uv * steps) + 0.5) / steps).rgb;
    if (PB(4u)) {
        let l = select(h.light, step(0.5, h.light) * 0.5 + 0.5, PB(5u));
        rgb *= max(l, P(6u));
    }
    if (PB(7u)) {
        let hs = rgb2hsv(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
        rgb = hsv2rgb(vec3<f32>(hs.x, clamp(hs.y + P(8u), 0.0, 1.0), hs.z));
    }
    var a = 1.0;
    if (PB(9u)) {
        let g = abs(fract(h.lat * vec2<f32>(steps / TAU, steps / PI * 0.5) + 0.5) - 0.5);
        a = 1.0 - smoothstep(0.03, 0.08, min(g.x, g.y));
    }
    return over(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), a, bg);
}

// ------------------------------------------------------------------ M5b batch 3: multi-pass helpers

// Input of a multi-pass effect (the chain's image before the effect), straight colour.
fn orig(uv: vec2<f32>) -> vec4<f32> {
    return unpremul(textureSampleLevel(prev_tex, samp, uv, 0.0));
}
// 1D blur of `src` along `r` (uv units = full radius), 33 taps, straight result.
fn sep_blur(uv: vec2<f32>, r: vec2<f32>, gauss: f32) -> vec4<f32> {
    var acc = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = -16; i <= 16; i++) {
        let o = f32(i) / 16.0;
        let w = mix(1.0, exp(-o * o * 4.0), gauss);
        acc += textureSampleLevel(src_tex, samp, uv + o * r, 0.0) * w;
        wsum += w;
    }
    return unpremul(acc / wsum);
}
// Horizontal pass on even, vertical on odd pass numbers.
fn pass_dir(p: u32, r: f32) -> vec2<f32> {
    return select(vec2<f32>(r / aspect(), 0.0), vec2<f32>(0.0, r), (p & 1u) == 1u);
}

// ------------------------------------------------------------------ M5b batch 3 effects

fn e_radial_blur(uv: vec2<f32>) -> vec4<f32> {
    // 0 Steps (samples), 1 Distance (zoom range), 2 Rotation (spin, 0.5 = none), 3 High Quality, 4 Black BG
    let n = i32(8.0 + P(0u) * 56.0) * select(1, 2, PB(3u));
    let p = hpos(uv);
    var acc = vec4<f32>(0.0);
    for (var i = 0; i < 128; i++) {
        if (i >= n) {
            break;
        }
        let k = f32(i) / f32(max(n - 1, 1));
        let q = rot(p, (P(2u) - 0.5) * k * 0.6) * (1.0 - min(P(1u) * 6.0, 0.95) * k);
        let s = hpos_uv(q);
        if (PB(4u) && !inside01(s)) {
            acc += vec4<f32>(0.0, 0.0, 0.0, 1.0);
        } else {
            acc += textureSampleLevel(src_tex, samp, clamp(s, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
        }
    }
    return unpremul(acc / f32(n));
}

fn e_tilt_shift(uv: vec2<f32>) -> vec4<f32> {
    // blur, a vertical gradient mask round Position (Focus = sharp band, Falloff = its
    // softness) mixes original and blurred, then over-saturation and a vignette.
    // 0 Blur, 1 Position, 2 Focus, 3 Falloff, 4 Saturation, 5 Vignette. Passes: H blur, V blur, combine.
    if (fx.pass_ < 2u) {
        return sep_blur(uv, pass_dir(fx.pass_, P(0u) * 0.06), 1.0);
    }
    let o = orig(uv);
    let b = tex(uv);
    let d = abs(uv.y - P(1u));
    let m = smoothstep(P(2u) * 0.5 * 0.5, P(2u) * 0.5 * 0.5 + P(3u) * 0.3 + 1e-3, d);
    var rgb = mix(o.rgb, b.rgb, m);
    rgb = mix(vec3<f32>(luma(rgb)), rgb, 1.0 + P(4u));
    let v = length((uv - 0.5) * vec2<f32>(aspect(), 1.0)) / length(vec2<f32>(aspect(), 1.0) * 0.5);
    rgb *= 1.0 - P(5u) * smoothstep(0.4, 1.0, v);
    return vec4<f32>(rgb, mix(o.a, b.a, m));
}

fn e_dilate(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Spread, 1 Size (radius = Size × Spread × 20 px), 2 Threshold (only brighter pixels spread),
    // 3 Mode Light (max) | Dark (min), 4 Shape Rectangle|Circle|Triangle|Cross
    let r = P(1u) * P(0u) * 20.0;
    let k = i32(clamp(ceil(r), 0.0, 6.0));
    if (k == 0) {
        return c;
    }
    let st = r / f32(k) / fx.resolution;
    let dark = P(3u) > 0.5;
    let shape = u32(P(4u) + 0.5);
    var best = c;
    var bl = luma(c.rgb);
    for (var y = -k; y <= k; y++) {
        for (var x = -k; x <= k; x++) {
            let o = vec2<f32>(f32(x), f32(y)) / f32(k);
            var ok = true;
            if (shape == 1u) {
                ok = dot(o, o) <= 1.0;
            } else if (shape == 2u) {
                ok = o.y >= -1.0 + abs(o.x) * 2.0 - 1.0 && o.y <= 1.0 - 0.0 && abs(o.x) <= (o.y + 1.0) * 0.5;
            } else if (shape == 3u) {
                ok = x == 0 || y == 0;
            }
            if (!ok) {
                continue;
            }
            let s = tex(uv + vec2<f32>(f32(x), f32(y)) * st);
            let l = luma(s.rgb);
            if (dark) {
                if (l < bl) {
                    best = s;
                    bl = l;
                }
            } else if (l > bl && l >= P(2u)) {
                best = s;
                bl = l;
            }
        }
    }
    return best;
}

fn e_kuwahara(uv: vec2<f32>) -> vec4<f32> {
    // 0 Radius (px): per pixel the quadrant with the least variance gives the mean colour (painterly)
    let r = max(P(0u), 1.0);
    let k = i32(min(r, 6.0));
    let st = r / f32(k) / fx.resolution;
    var best = vec4<f32>(0.0);
    var bv = 1e9;
    for (var q = 0; q < 4; q++) {
        let sx = select(-1.0, 1.0, (q & 1) == 1);
        let sy = select(-1.0, 1.0, (q & 2) == 2);
        var m = vec4<f32>(0.0);
        var m2 = vec3<f32>(0.0);
        for (var y = 0; y <= k; y++) {
            for (var x = 0; x <= k; x++) {
                let s = tex(uv + vec2<f32>(f32(x) * sx, f32(y) * sy) * st);
                m += s;
                m2 += s.rgb * s.rgb;
            }
        }
        let cnt = f32((k + 1) * (k + 1));
        m /= cnt;
        let v = m2 / cnt - m.rgb * m.rgb;
        let vs = v.r + v.g + v.b;
        if (vs < bv) {
            bv = vs;
            best = m;
        }
    }
    return best;
}

fn fbm(p: vec2<f32>, octaves: f32, seed: u32) -> f32 {
    var s = 0.0;
    var a = 0.5;
    var q = p;
    var norm = 0.0;
    for (var i = 0; i < 8; i++) {
        if (f32(i) >= max(octaves, 1.0)) {
            break;
        }
        s += vnoise(q, seed + u32(i)) * a;
        norm += a;
        q = q * 2.03 + 17.0;
        a *= 0.5;
    }
    return s / norm;
}

fn e_acuarela(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // input blended with last output (× 0.98) whose UVs are offset by animated fractal
    // noise. 0 Gain (how much fresh input), 1 Speed, 2 Detail (octaves), 3 Strength (offset),
    // 4 Density (noise frequency), 5/6 Gain X/Y (offset direction). Reset: remove/re-add the effect.
    let t = fx.time * P(1u) * 0.05;
    let g = uv * max(P(4u), 0.1) * vec2<f32>(aspect(), 1.0);
    let n = vec2<f32>(fbm(g + vec2<f32>(t, 0.0), P(2u), 101u), fbm(g + vec2<f32>(0.0, t) + 31.0, P(2u), 111u)) - 0.5;
    let off = (n * 2.0 + vec2<f32>(P(5u), P(6u)) - 0.3) * P(3u) * 0.02;
    let prev = unpremul(textureSampleLevel(prev_tex, samp, uv - off, 0.0)) * 0.98;
    if (prev.a <= 0.001) {
        return c;
    }
    let mixed = max(prev, c);
    return mix(mixed, c, P(0u));
}

fn e_iterate(uv: vec2<f32>) -> vec4<f32> {
    // Copies stacked with a per-step transform (0.5 = none): 0 TransformStrength, 1 Iterations (1-21 copies),
    // 2 StartOpacity, 3 OpacityChange (per step, 0.5 = keep), 4-6 Translate X/Y/Z (Z = depth → size),
    // 7-9 Rotate X/Y/Z, 10 Scale, 11-18 D… = change of each step per step, 19 BlendMode (< 0.5 alpha,
    // else add), 20-22 GRotate X/Y/Z (whole stack), 23 Black BG
    let n = 1 + i32(P(1u) * 20.0);
    let ks = P(0u);
    let p0 = rot(hpos(uv), -(P(22u) - 0.5) * TAU);
    let p = p0 / vec2<f32>(max(abs(cos((P(21u) - 0.5) * PI)), 0.05), max(abs(cos((P(20u) - 0.5) * PI)), 0.05));
    var acc = vec4<f32>(0.0);
    let add = P(19u) >= 0.5;
    for (var ii = 20; ii >= 0; ii--) {
        if (ii >= n) {
            continue;
        }
        let i = f32(ii);
        let acc_i = i * (i - 1.0) * 0.5; // sum of step changes
        let tr = (vec2<f32>(P(4u), P(5u)) - 0.5) * 2.0 * i + (vec2<f32>(P(12u), P(13u)) - 0.5) * 2.0 * acc_i;
        let z = (P(6u) - 0.5) * i + (P(14u) - 0.5) * acc_i;
        let rz = (P(9u) - 0.5) * TAU * i + (P(17u) - 0.5) * TAU * acc_i;
        let rx = (P(7u) - 0.5) * PI * i + (P(15u) - 0.5) * PI * acc_i;
        let ry = (P(8u) - 0.5) * PI * i + (P(16u) - 0.5) * PI * acc_i;
        let sc = pow(exp2((P(10u) - 0.5) * 2.0), i) * pow(exp2((P(18u) - 0.5) * 2.0), acc_i) / max(1.0 + z * 0.5, 0.05);
        let s2 = vec2<f32>(sc * cos(ry * ks), sc * cos(rx * ks));
        let cp = copy_pm(p, tr * ks, s2, rz * ks);
        let op = clamp(P(2u) * pow(P(3u) * 2.0, i) * pow(P(11u) * 2.0, acc_i), 0.0, 1.0);
        if (add) {
            acc = min(acc + cp * op, vec4<f32>(1.0));
        } else {
            acc = over_pm(cp * op, acc);
        }
    }
    let bg = premul(black_or_clear(PB(23u)));
    return unpremul(over_pm(acc, bg));
}

fn shaper_mask(uv: vec2<f32>) -> f32 {
    let s = tex(uv);
    return smoothstep(0.4, 0.6, luma(s.rgb) * s.a);
}

fn e_shaper(uv: vec2<f32>) -> vec4<f32> {
    // The picture's bright/opaque shape as a flat fill with an outline. 0 Scale, 1 Noise Type Grain|Hatched,
    // 2 Noise Amount (±), 3 Noise Speed, 4-7 Color, 8 Outline (width), 9-12 Outline Color
    let q = (uv - 0.5) / max(P(0u), 0.01) + 0.5;
    let m = shaper_mask(q);
    let w = P(8u) * 0.05;
    var edge = 0.0;
    if (w > 0.0) {
        for (var i = 0; i < 12; i++) {
            let a = f32(i) * TAU / 12.0;
            let o = vec2<f32>(cos(a) / aspect(), sin(a)) * w;
            edge = max(edge, abs(shaper_mask(q + o) - m));
        }
    }
    let px = px_coord(uv);
    let t = u32(fx.time * P(3u) * 30.0);
    var nz = rnd(px, t * 3u + 5u) - 0.5;
    if (P(1u) > 0.5) {
        nz = step(0.5, fract((px.x + px.y) / 6.0 + f32(t) * 0.05)) - 0.5;
    }
    let fill = PC(4u);
    let frgb = fill.rgb + nz * P(2u);
    var o = vec4<f32>(frgb, fill.a * m);
    o = over(PC(9u).rgb, PC(9u).a * edge, o);
    return o;
}

fn ngon(p: vec2<f32>, n: f32, apo: f32) -> f32 {
    // Regular n-gon (flat bottom edge), apothem `apo`; negative inside.
    let seg = TAU / n;
    let a = atan2(p.x, p.y) + seg * 0.5;
    let k = a - floor(a / seg) * seg - seg * 0.5;
    return length(p) * cos(k) - apo;
}
fn shape_sdf(p: vec2<f32>, shape: u32, r: f32) -> f32 {
    switch shape {
        case 0u: { let d = abs(p) - r; return max(d.x, d.y); } // square
        case 1u: { return length(p) - r; }
        case 2u: { return ngon(p, 3.0, r * 0.5); }
        case 3u: { return ngon(p, 5.0, r * 0.81); }
        case 4u: { return ngon(p, 6.0, r * 0.87); }
        case 5u: { return ngon(p, 8.0, r * 0.92); }
        default: { return min(ngon(p, 3.0, r * 0.5), ngon(-p, 3.0, r * 0.5)); } // hexagram
    }
}

fn e_shape_mask(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Shape Square|Circle|Triangle|Pentagon|Hexagon|Octagon|Hexagram, 1 Feather (px), 2 Scale %
    // (100 = shape as tall as the frame), 3 Rotation °, 4/5 Position X/Y, 6 Invert
    let p = rot(hpos(uv) - vec2<f32>(P(4u) * aspect(), P(5u)), -radians(P(3u)));
    let d = shape_sdf(p, u32(P(0u) + 0.5), P(2u) / 100.0);
    let f = max(P(1u), 0.5) * 2.0 / fx.resolution.y;
    var m = 1.0 - smoothstep(-f * 0.5, f * 0.5, d);
    if (PB(6u)) {
        m = 1.0 - m;
    }
    return vec4<f32>(c.rgb, c.a * m);
}

fn meta_sdf(p: vec2<f32>, shape: u32, r: f32) -> f32 {
    switch shape {
        case 0u: { return length(p) - r; }
        case 1u: { let d = abs(p) - r; return max(d.x, d.y); }
        case 2u: { return ngon(p, 6.0, r * 0.9); }
        default: {
            let d = abs(p);
            let w = r * 0.35;
            return min(max(d.x - r, d.y - w), max(d.x - w, d.y - r));
        }
    }
}

fn e_meta_shape(uv: vec2<f32>) -> vec4<f32> {
    // Grid of shapes morphing from Shape 1 (dark) to Shape 2 (bright). 0 Resolution (cells across),
    // 1/2 Shape 1/2 Circle|Rectangle|Hexagon|Cross, 3 Color White|Greyscale|Sampled|Original,
    // 4 Mode Fill|Outline|Outline Scaled, 5 Scale (size by brightness), 6 Invert (brightness), 7 Margin
    let cells = max(P(0u), 1.0);
    let cs = fx.resolution.x / cells;
    let px = px_coord(uv);
    let id = floor(px / cs);
    let ctr = (id + 0.5) * cs;
    let s = tex(ctr / fx.resolution);
    var l = luma(s.rgb) * s.a;
    if (PB(6u)) {
        l = 1.0 - l;
    }
    let p = (px - ctr) / (cs * 0.5);
    var r = 1.0 - P(7u);
    if (PB(5u)) {
        r *= l;
    }
    let d = mix(meta_sdf(p, u32(P(1u) + 0.5), r), meta_sdf(p, u32(P(2u) + 0.5), r), l);
    let aa = 2.0 / cs;
    var m = 1.0 - smoothstep(-aa, aa, d);
    let mode = u32(P(4u) + 0.5);
    if (mode >= 1u) {
        var w = 0.12;
        if (mode == 2u) {
            w = 0.04 + 0.3 * l;
        }
        m = 1.0 - smoothstep(w - aa, w + aa, abs(d));
    }
    var col = vec3<f32>(1.0);
    switch u32(P(3u) + 0.5) {
        case 1u: { col = vec3<f32>(l); }
        case 2u: { col = s.rgb; }
        case 3u: { col = tex(uv).rgb; }
        default: {}
    }
    return vec4<f32>(col, m);
}

fn e_bloom(uv: vec2<f32>) -> vec4<f32> {
    // 0 Amount, 1 Threshold, 2 Knee (soft threshold), 3 Iterations (blur rounds, each wider),
    // 4 Size, 5 Gaussian. Passes: extract, Iterations × (H, V) blur, add onto the input.
    if (fx.pass_ == 0u) {
        let c = tex(uv);
        let br = max(max(c.r, c.g), c.b);
        // Strong: bright orange blows out to white at Threshold 0.9.
        let k = smoothstep(P(1u) - P(2u) - 1e-3, P(1u) + P(2u), br);
        return vec4<f32>(c.rgb, c.a * k);
    }
    if (fx.pass_ + 1u < fx.passes) {
        let round = f32((fx.pass_ - 1u) / 2u);
        return sep_blur(uv, pass_dir(fx.pass_ - 1u, P(4u) * 0.25 * exp2(round)), P(5u));
    }
    let o = orig(uv);
    let b = tex(uv);
    let add = b.rgb * b.a * P(0u) * 2.5;
    return vec4<f32>(o.rgb + add, max(o.a, clamp(b.a * P(0u) * 2.0, 0.0, 1.0)));
}

// ------------------------------------------------------------------ M5b batch 4: temporal

// Last output of this effect (feedback), straight; alpha 0 on the first frame.
fn last_out(uv: vec2<f32>) -> vec4<f32> {
    return unpremul(textureSampleLevel(prev_tex, samp, uv, 0.0));
}

fn e_freeze(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0/1 X/Y: the part left of X and above Y holds its picture, 2 Frozen solid (all), 3 Black BG
    // (frozen part before anything was held)
    let frozen = PB(2u) || uv.x < P(0u) || uv.y < P(1u);
    if (!frozen) {
        return c;
    }
    let p = last_out(uv);
    if (fx.prev_time < 0.0) {
        return select(c, vec4<f32>(0.0, 0.0, 0.0, 1.0), PB(3u));
    }
    return p;
}

fn e_stop_motion(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // 0 Frequency (new frames per second, 0.5 → 8/s), 1 Frame Count (holds each one longer), 2 Black BG
    let hz = mix(0.5, 30.0, P(0u) * P(0u)) / (1.0 + P(1u) * 15.0);
    let t = fx.time;
    let pt = fx.prev_time;
    let tick = pt < 0.0 || t < pt || floor(t * hz) != floor(pt * hz);
    if (tick) {
        if (PB(2u)) {
            return over(c.rgb, c.a, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        }
        return c;
    }
    return last_out(uv);
}

fn e_delay_rgb(uv: vec2<f32>) -> vec4<f32> {
    // 0-2 R/G/B delay (0..1 → 0..15 frames): each channel from an older input frame
    let r = unpremul(hist_pm(uv, u32(round(P(0u) * 15.0))));
    let g = unpremul(hist_pm(uv, u32(round(P(1u) * 15.0))));
    let b = unpremul(hist_pm(uv, u32(round(P(2u) * 15.0))));
    return vec4<f32>(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}

// ------------------------------------------------------------------ M5b batch 5: special

// Coverage of the effect's CPU image (rasterised text), 0..1.
fn cov(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(aux_tex, samp, uv, 0.0).r;
}

fn e_text(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // Text Block / Text Animator: the text is laid out and rasterised on the CPU (the app's text.rs,
    // incl. position, size, rotation, animation); here: 0-3 Color, 4-7 Outline Color,
    // 8 Outline Width (up to 24 px at 1080p), 9 Glow In, 10 Glow Out (halo up to 60 px).
    let a = cov(uv);
    let k = fx.resolution.y / 1080.0 / fx.resolution;
    let ow = P(8u) * 24.0;
    var outline = 0.0;
    if (ow > 0.0) {
        for (var i = 0; i < 16; i++) {
            let d = vec2<f32>(cos(f32(i) * TAU / 16.0), sin(f32(i) * TAU / 16.0));
            outline = max(outline, max(cov(uv + d * ow * k), cov(uv + d * ow * 0.5 * k)));
        }
    }
    var glow = 0.0;
    let gr = max(P(9u), P(10u)) * 60.0;
    if (gr > 0.0) {
        for (var i = 0; i < 16; i++) {
            let d = vec2<f32>(cos(f32(i) * TAU / 16.0 + 0.3), sin(f32(i) * TAU / 16.0 + 0.3));
            glow += cov(uv + d * gr * k) + cov(uv + d * gr * 0.5 * k) + cov(uv + d * gr * 0.25 * k);
        }
        glow /= 48.0;
    }
    let col = PC(0u);
    let ocol = PC(4u);
    var o = c;
    // glow out: halo in the outline colour outside the letters
    o = over(ocol.rgb, ocol.a * glow * P(10u) * 2.0 * (1.0 - a), o);
    o = over(ocol.rgb, ocol.a * outline, o);
    // glow in: letters brighter towards their middle (blurred coverage is highest there)
    o = over(col.rgb * (1.0 + (glow - 0.5) * P(9u) * 2.0), col.a * a, o);
    return o;
}

fn e_particles(uv: vec2<f32>) -> vec4<f32> {
    // Stateless: particle i (of Particles, looping every Lifetime s) is a copy of the input.
    // 0 Particles, 1 Lifetime, 2 Spawn Mode (Manual = Auto here), 3-5 X/Y/Z Pos (-1..1, Y up),
    // 6-11 X/Y/Z Vel Min/Max (per frame at 60 fps), 12/13 Rotate Min/Max (°/frame), 14/15 Scale
    // Min/Max (of the frame), 16 Scale Delta, 17 Opacity Delta (per second factors), 18 Inherit Vel
    // (unused), 19 Gravity, 20 Wind (per frame²), 21 Friction (per frame)
    let p = hpos(uv);
    let a = aspect();
    let n = i32(clamp(P(0u), 0.0, 256.0));
    let life = max(P(1u), 0.05);
    var acc = vec4<f32>(0.0);
    for (var i = 0; i < 256; i++) {
        if (i >= n) {
            break;
        }
        let fi = f32(i);
        let ph = fx.time / life - fi / f32(n);
        let cyc = floor(ph);
        let age = (ph - cyc) * life;
        let id = vec2<f32>(fi, cyc);
        let f = age * 60.0;
        let v = vec3<f32>(
            mix(P(6u), P(7u), rnd(id, 201u)),
            mix(P(8u), P(9u), rnd(id, 202u)),
            mix(P(10u), P(11u), rnd(id, 203u)));
        let fr = clamp(P(21u), 0.0, 0.999);
        var travel = f;
        if (fr > 0.0) {
            travel = (1.0 - pow(1.0 - fr, f)) / fr;
        }
        var pos = vec3<f32>(P(3u), P(4u), P(5u)) + v * travel;
        pos.x += 0.5 * P(20u) * f * f;
        pos.y -= 0.5 * P(19u) * f * f;
        let persp = 1.0 / max(1.0 - pos.z, 0.1);
        let ctr = vec2<f32>(pos.x * a, -pos.y) * persp;
        let sc = mix(P(14u), P(15u), rnd(id, 204u)) * pow(P(16u), age) * persp;
        let ang = radians(mix(P(12u), P(13u), rnd(id, 205u)) * f);
        let op = pow(P(17u), age) * smoothstep(0.0, 0.05, age / life);
        acc = over_pm(copy_pm(p, ctr, vec2<f32>(sc), ang) * op, acc);
    }
    return unpremul(acc);
}

fn terrain_h(xz: vec2<f32>, rep: f32, hgt: f32, jag: f32) -> f32 {
    let uv = xz / (60.0 / (1.0 + rep));
    var h = luma(textureSampleLevel(src_tex, samp, mirror_uv(uv), 0.0).rgb);
    h += (vnoise(xz * 0.8, 301u) - 0.5) * jag * 0.5;
    return h * hgt;
}

fn e_terrain(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // Heightfield from the input's brightness, flown over. 0 Tilt (camera pitch °), 1 Direction (yaw °),
    // 2 Position Y (camera height), 3 Height (terrain), 4 Wobble, 5 Speed (forward), 6 Jagginess
    // (noise detail), 7 Attenuation (fog distance), 8 Texture Rep (tiling), 9 Additive (else on black)
    let p = hpos(uv) * 0.6;
    let t = fx.time;
    let yaw = radians(P(1u)) + sin(t * 1.3) * P(4u) * 20.0;
    let pitch = radians(P(0u)) + 0.25;
    var rd = normalize(vec3<f32>(p.x, -p.y, 1.0));
    rd = rotx(rd, pitch);
    let rz = rot(rd.xz, yaw);
    rd = vec3<f32>(rz.x, rd.y, rz.y);
    let ro = vec3<f32>(0.0, P(2u) + P(3u) * 0.5, t * P(5u) * 10.0);
    let far = max(P(7u), 10.0);
    var tt = 0.5;
    var hit = false;
    for (var i = 0; i < 160; i++) {
        let q = ro + rd * tt;
        let h = terrain_h(q.xz, P(8u), P(3u), P(6u));
        if (q.y < h) {
            hit = true;
            break;
        }
        tt += max((q.y - h) * 0.4, 0.02 * tt + 0.05);
        if (tt > far) {
            break;
        }
    }
    var o = vec4<f32>(0.0);
    if (hit) {
        let q = ro + rd * tt;
        let tc = q.xz / (60.0 / (1.0 + P(8u)));
        let col = tex(mirror_uv(tc)).rgb;
        let fog = 1.0 - smoothstep(far * 0.4, far, tt);
        o = vec4<f32>(col * fog, 1.0);
    }
    if (PB(9u)) {
        return vec4<f32>(c.rgb + o.rgb, max(c.a, o.a * luma(o.rgb)));
    }
    return over(o.rgb, o.a, vec4<f32>(0.0, 0.0, 0.0, 1.0));
}

// ------------------------------------------------------------------ sources (generators)
// Sources as shaders on a clear input (kinds 128+ = lavmfx-core `sources::GENERATORS`).
// Params from `crates/core/data/lavmfx-source-params.json`; meaning guessed from names/ranges.

fn g_abstract_field(uv: vec2<f32>) -> vec4<f32> {
    // 0 Octaves (1-6), 1 Amplitude (contrast), 2 Density (scale): drifting smoky noise
    let p = hpos(uv) * mix(1.0, 8.0, P(2u)) + vec2<f32>(fx.time * 0.15, fx.time * 0.07);
    let v = fbm(p + vec2<f32>(fbm(p + fx.time * 0.1, P(0u), 401u), 0.0) * 2.0, P(0u), 402u);
    let k = clamp((v - 0.5) * (1.0 + P(1u) * 6.0) + 0.5, 0.0, 1.0);
    return vec4<f32>(vec3<f32>(k), 1.0);
}

fn g_blank() -> vec4<f32> {
    // 0 Black BG
    return black_or_clear(PB(0u));
}

fn g_checkered(uv: vec2<f32>) -> vec4<f32> {
    // 0/1 Num X/Y, 2-5 Color 1, 6-9 Color 2
    let n = max(vec2<f32>(P(0u), P(1u)), vec2<f32>(1.0));
    let c = floor(uv * n);
    let odd = (i32(c.x + c.y) & 1) == 1;
    return select(PC(2u), PC(6u), odd);
}

fn g_gradient(uv: vec2<f32>) -> vec4<f32> {
    // 0 Method 2 Colors|Palette, 1-4 Color 1, 5-8 Color 2, 9 Mode Smooth|Stepped,
    // 10 Dither (steps when Stepped, 0-32), 11 Type Horizontal|Vertical|Radial|Angular
    let p = hpos(uv);
    var t = uv.x;
    switch u32(P(11u) + 0.5) {
        case 1u: { t = uv.y; }
        case 2u: { t = clamp(length(p) / length(vec2<f32>(aspect(), 1.0)), 0.0, 1.0); }
        case 3u: { t = atan2(p.y, p.x) / TAU + 0.5; }
        default: {}
    }
    if (P(9u) > 0.5) {
        let n = max(P(10u), 2.0);
        t = floor(t * n) / (n - 1.0);
    }
    if (P(0u) > 0.5) {
        return vec4<f32>(palette(0u, t), 1.0);
    }
    return mix(PC(1u), PC(5u), clamp(t, 0.0, 1.0));
}

// Line Scape terrain height at knot `k` of world row `row` (Direction turns
// the terrain under the rows).
fn ls_knot(k: f32, row: f32, yaw: f32, amp: f32, jag: f32) -> f32 {
    let q = rot(vec2<f32>(k * LS_KNOT, row), yaw);
    let base = vnoise(q * vec2<f32>(0.3, 0.3), 411u);
    let fine = vnoise(q * vec2<f32>(1.7, 0.9), 412u) - 0.5;
    return (base + fine * jag * 1.4) * amp;
}

const LS_KNOT: f32 = 0.6;

// Piecewise-straight profile between knots (the jagged wireframe look).
fn ls_height(x: f32, row: f32, yaw: f32, amp: f32, jag: f32) -> f32 {
    let k = floor(x / LS_KNOT);
    return mix(ls_knot(k, row, yaw, amp, jag), ls_knot(k + 1.0, row, yaw, amp, jag), x / LS_KNOT - k);
}

fn g_line_scape(uv: vec2<f32>) -> vec4<f32> {
    // Wireframe landscape seen from above: rows of terrain lines coming at the
    // camera, nearer rows hide the ones behind (hidden lines).
    // 0 Tilt, 1 Direction, 2 Position Y (camera height), 3 Speed, 4 Height,
    // 5 Jagginess, 6 Wobble, 7 Attenuation (fog distance), 8 Line Width,
    // 9 Fuzzyness, 10-13 Color, 14 Show Knots
    let s = hpos(uv);
    let sx = s.x;
    let sy = -s.y; // up
    let t = fx.time;
    let f = 1.6; // focal length
    let amp = P(4u) * 0.15;
    let jag = P(5u);
    let yaw = radians(P(1u));
    let th = radians(P(0u)) + 0.2; // looking down a little
    let cth = cos(th);
    let sth = sin(th);
    let cam = 1.5 + P(2u) * 0.3 + amp;
    let sway = sin(t * 0.9) * P(6u) * 2.0;
    let travel = t * P(3u) * 2.0; // rows per second
    let shift = fract(travel);
    let base_row = floor(travel);
    let far = clamp(P(7u) * 0.3, 4.0, 90.0);
    let w = P(8u) * 0.004;
    let soft = w * (0.3 + P(9u) * 3.0) + 0.002;
    let knots = PB(14u);
    let col = PC(10u);
    for (var i = 0; i < 96; i++) {
        let z = f32(i) + 1.0 - shift + 0.5;
        if (z > far) {
            break;
        }
        let row = base_row + f32(i);
        // Depth along the view (pitched camera), using the mean terrain level.
        let zp0 = z * cth - (amp * 0.5 - cam) * sth;
        let xw = sx * zp0 / f + sway;
        // Screen height of this row's profile at this pixel (+ slope for width).
        let e = 0.002;
        let h = ls_height(xw, row, yaw, amp, jag);
        let h2 = ls_height(xw + e * zp0 / f, row, yaw, amp, jag);
        let yr = h - cam;
        let ys = f * (yr * cth + z * sth) / max(z * cth - yr * sth, 0.05);
        let yr2 = h2 - cam;
        let ys2 = f * (yr2 * cth + z * sth) / max(z * cth - yr2 * sth, 0.05);
        let slope = (ys2 - ys) / e;
        let d = (sy - ys) / sqrt(1.0 + slope * slope);
        let fog = 1.0 - smoothstep(far * 0.4, far, z);
        // Knots: dots at the profile's corners.
        if (knots) {
            let xk = round(xw / LS_KNOT) * LS_KNOT;
            let hk = ls_height(xk, row, yaw, amp, jag) - cam;
            let zk = max(z * cth - hk * sth, 0.05);
            let pk = vec2<f32>(f * (xk - sway) / zp0, f * (hk * cth + z * sth) / zk);
            let r = w * 2.5 + 0.004;
            let dk = length(vec2<f32>(sx, sy) - pk);
            if (dk < r + soft) {
                let a = 1.0 - smoothstep(r, r + soft, dk);
                return vec4<f32>(col.rgb, col.a * a * fog);
            }
        }
        if (abs(d) < w + soft) {
            let a = 1.0 - smoothstep(w, w + soft, abs(d));
            return vec4<f32>(col.rgb, col.a * a * fog);
        }
        if (d < 0.0) {
            // Below this row's line = its ground: hides the rows behind.
            return vec4<f32>(0.0);
        }
    }
    return vec4<f32>(0.0);
}

fn g_lines(uv: vec2<f32>) -> vec4<f32> {
    // 0 Fuzzyness, 1 Amount (lines across), 2 Width (of a line, 0-2), 3 Rotation °, 4 Position (shift), 5-8 Color
    let p = rot(hpos(uv), -radians(P(3u)));
    let n = max(P(1u), 1.0);
    let f = fract(p.x * n * 0.5 / aspect() + P(4u) - 0.5);
    let half = clamp(P(2u) * 0.5, 0.0, 1.0) * 0.5;
    let soft = P(0u) * 0.5 + 1e-3;
    let m = 1.0 - smoothstep(half - soft * 0.5, half + soft * 0.5, abs(f - 0.5));
    let col = PC(5u);
    return vec4<f32>(col.rgb, col.a * m);
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    // Polynomial smooth minimum: a and b melt together within k.
    let h = clamp(0.5 + 0.5 * (b - a) / max(k, 1e-5), 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

fn g_metaballs(uv: vec2<f32>) -> vec4<f32> {
    // Balls that melt into each other where they meet.
    // 0 Grid (balls per side), 1 Blend Minimal (smooth union of the balls) | Average (summed
    // field), 2 Size (ball radius), 3 Distance (orbit), 4 Radius (reach: how far they melt),
    // 5 Fuzzyness (edge), 6 Speed, 7/8 Position X/Y, 9-12 Color, 13-16 BG Color
    let p = hpos(uv) - vec2<f32>(P(7u) * aspect(), -P(8u));
    let n = i32(clamp(floor(P(0u)), 1.0, 20.0));
    let cell = 2.0 / f32(n);
    let r = max(P(2u), 0.01) * cell * 0.5;
    let reach = max(P(4u), 0.01);
    let minimal = P(1u) < 0.5;
    var sd = 1e9;
    var field = 0.0;
    let expo = 2.0 + (1.0 - reach) * 6.0;
    for (var y = 0; y < 20; y++) {
        if (y >= n) {
            break;
        }
        for (var x = 0; x < 20; x++) {
            if (x >= n) {
                break;
            }
            let id = vec2<f32>(f32(x), f32(y));
            let home = ((id + 0.5) / f32(n) - 0.5) * 2.0 * vec2<f32>(aspect(), 1.0) * 0.6;
            let ph = rnd(id, 421u) * TAU;
            let sp = fx.time * P(6u) * (0.5 + rnd(id, 422u));
            let c = home + vec2<f32>(cos(sp + ph), sin(sp * 1.3 + ph)) * P(3u) * cell;
            let d = length(p - c);
            sd = smin(sd, d - r, reach * r * 3.0);
            field += pow(r / max(d, 1e-4), expo);
        }
    }
    let aa = 2.0 / fx.resolution.y;
    var m: f32;
    if (minimal) {
        let f = P(5u) * r * 0.3 + aa;
        m = 1.0 - smoothstep(-f, f, sd);
    } else {
        let f = P(5u) + 0.02;
        m = smoothstep(1.0 - f, 1.0 + f, field);
    }
    return mix(PC(13u), PC(9u), m);
}

fn g_rings(uv: vec2<f32>) -> vec4<f32> {
    // 0 Num Rings, 1 Size, 2 Spacing, 3 Line Width, 4 Style Fill|Stroke|Alternate, 5 Gap °, 6 Rotation °,
    // 7 Rotation Step °, 8 Rotation Alternate, 9-12 Color 1, 13-16 Color 2
    let p = hpos(uv);
    let r = length(p);
    let a = degrees(atan2(p.y, p.x));
    let n = max(floor(P(0u)), 1.0);
    let step = max(P(2u), 0.01) * 0.3;
    let r0 = P(1u) * 0.25;
    let i = floor((r - r0) / step + 0.5);
    if (i < 0.0 || i >= n) {
        return vec4<f32>(0.0);
    }
    let ring_r = r0 + i * step;
    let half = P(3u) * step * 0.5;
    let aa = 1.5 / fx.resolution.y;
    let band = 1.0 - smoothstep(half - aa, half + aa, abs(r - ring_r));
    var dir = 1.0;
    if (PB(8u) && (i32(i) & 1) == 1) {
        dir = -1.0;
    }
    let ra = a - (P(6u) + i * P(7u)) * dir;
    let ang = ra - floor(ra / 360.0) * 360.0;
    let in_gap = ang < P(5u);
    if (in_gap) {
        return vec4<f32>(0.0);
    }
    let style = u32(P(4u) + 0.5);
    var col = PC(9u);
    if (style == 2u && (i32(i) & 1) == 1) {
        col = PC(13u);
    }
    var m = band;
    if (style == 1u) {
        // stroke: just the outline of the band
        let e = abs(abs(r - ring_r) - half);
        m = 1.0 - smoothstep(aa, aa * 2.5, e);
    }
    return vec4<f32>(col.rgb, col.a * m);
}

fn g_shaper(uv: vec2<f32>) -> vec4<f32> {
    // A filled circle with outline and noise (shape choice not exposed).
    // 0 Scale, 1 Noise Type Grain|Hatched, 2 Noise Amount, 3 Noise Speed, 4-7 Color, 8 Outline, 9-12 Outline Color
    let p = hpos(uv);
    let r = 0.5 * P(0u);
    let d = length(p) - r;
    let aa = 2.0 / fx.resolution.y;
    let fill = 1.0 - smoothstep(-aa, aa, d);
    let w = P(8u) * 0.2;
    let edge = (1.0 - smoothstep(w - aa, w + aa, abs(d))) * step(1e-4, w);
    let px = px_coord(uv);
    let t = u32(fx.time * P(3u) * 30.0);
    var nz = rnd(px, t * 3u + 7u) - 0.5;
    if (P(1u) > 0.5) {
        nz = step(0.5, fract((px.x + px.y) / 6.0 + f32(t) * 0.05)) - 0.5;
    }
    var o = vec4<f32>(PC(4u).rgb + nz * P(2u), PC(4u).a * fill);
    return over(PC(9u).rgb, PC(9u).a * edge, o);
}

fn g_sine_wave(uv: vec2<f32>) -> vec4<f32> {
    // 0 Waves, 1 Mode Line|Fill, 2 Line Width, 3 Fuzzyness, 4 Direction Horizontal|Vertical, 5 Mirror,
    // 6 Frequency (extra ripple), 7 Main Speed, 8 Modulator, 9 Modulation Depth, 10 Modulator Speed,
    // 11 Amplitude, 12 Position, 13-16 Color, 17-20 BG Color
    var q = uv;
    if (P(4u) > 0.5) {
        q = vec2<f32>(uv.y, uv.x);
    }
    let t = fx.time;
    let x = q.x;
    let ph = x * TAU * P(0u) + t * P(7u) + P(9u) * sin(x * TAU * P(8u) * 0.25 + t * P(10u)) + sin(x * TAU * P(6u)) * 0.5;
    let y = 1.0 - P(12u) - sin(ph) * P(11u) * 0.5;
    let w = P(2u) * 0.05;
    let soft = P(3u) * 0.05 + 1.0 / fx.resolution.y;
    var m: f32;
    if (P(1u) > 0.5) {
        m = smoothstep(y - soft, y + soft, q.y);
    } else {
        m = 1.0 - smoothstep(w - soft, w + soft, abs(q.y - y));
    }
    if (PB(5u)) {
        let y2 = 1.0 - P(12u) + sin(ph) * P(11u) * 0.5;
        if (P(1u) > 0.5) {
            // filled between the wave and its mirror
            let lo = min(y, y2);
            let hi = max(y, y2);
            m = smoothstep(lo - soft, lo + soft, q.y) * (1.0 - smoothstep(hi - soft, hi + soft, q.y));
        } else {
            m = max(m, 1.0 - smoothstep(w - soft, w + soft, abs(q.y - y2)));
        }
    }
    return mix(PC(17u), PC(13u), m);
}

fn g_spiral(uv: vec2<f32>) -> vec4<f32> {
    // 0 Direction Clockwise|Anti, 1 Control Mode (unused), 2 Spread Even, 3 Detail, 4 Speed, 5 Zoom,
    // 6 Leafs (arms), 7 Distortion, 8 Fuzzyness, 9 Ramification, 10 Button (centre disc),
    // 11 Button Size, 12 Button Fuzzyness, 13-16 Color, 17-20 BG Color
    let p = hpos(uv) / max(P(5u), 0.1);
    let r = max(length(p), 1e-4);
    let dir = select(1.0, -1.0, P(0u) > 0.5);
    var a = atan2(p.y, p.x) * dir / TAU;
    a += sin(r * P(7u) * 0.1) * 0.05 + sin(a * TAU * P(9u) * 3.0) * P(9u) * 0.05;
    let arms = max(floor(P(6u)), 1.0);
    let v = fract(a * arms + log(r) * max(P(2u), 0.05) * P(3u) - fx.time * P(4u) * 0.5);
    let soft = P(8u) * 0.5 + 0.01;
    var m = smoothstep(0.5 - soft, 0.5, v) * (1.0 - smoothstep(1.0 - soft, 1.0, v));
    m = max(m, 1.0 - smoothstep(0.0, soft, v));
    if (PB(10u)) {
        let bs = P(11u) * 0.2;
        let bf = P(12u) * 0.2 + 1e-3;
        m = max(m, 1.0 - smoothstep(bs - bf, bs + bf, r));
    }
    return mix(PC(17u), PC(13u), m);
}

fn g_stroboscope() -> vec4<f32> {
    // 0 Frequency (×30 Hz), 1 Fade, 2-5 Color 1, 6-9 Color 2
    let ph = fract(fx.time * max(P(0u), 0.01) * 30.0);
    var k = select(1.0, 0.0, ph < 0.5);
    if (PB(1u)) {
        k = ph;
    }
    return mix(PC(2u), PC(6u), k);
}

fn g_tunnelines(uv: vec2<f32>) -> vec4<f32> {
    // Rings flying towards the viewer: 0-3 Color 1, 4-7 Color 2, 8 Line Width, 9 Zoom,
    // 10 Vertical / 12 Horizontal (centre sway), 11 Frequency (speed)
    let t = fx.time;
    var p = hpos(uv);
    p -= vec2<f32>(sin(t * 0.7) * P(12u), cos(t * 0.5) * P(10u)) * 2.0;
    let r = max(length(p), 1e-3);
    let z = P(9u) / r + t * P(11u) * 10.0;
    let i = floor(z);
    let f = fract(z);
    let w = P(8u) * 10.0 * r + 0.002;
    let m = 1.0 - smoothstep(w, w * 2.0, min(f, 1.0 - f));
    let col = select(PC(0u), PC(4u), (i32(i) & 1) == 1);
    let fade = smoothstep(0.0, 0.15, r);
    return vec4<f32>(col.rgb, col.a * m * fade);
}

// ------------------------------------------------------------------ dry/wet

fn blend(b: vec3<f32>, s: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 1u: { return b + s; }
        case 2u: { return b - s; }
        case 3u: { return b * s; }
        case 4u: { return b + s - b * s; }
        case 6u: { return min(b, s); }
        case 7u: { return max(b, s); }
        case 8u: { return abs(b - s); }
        case 9u: { return b + s - 2.0 * b * s; }
        default: { return s; }
    }
}


// ------------------------------------------------------------------ M6: slices, UV Map, Mask

fn cross2(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return a.x * b.y - a.y * b.x;
}

// Position (u, v) of `p` in the quad a b c d (TL TR BR BL), bilinear; < 0 when outside.
fn inv_bilinear(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>) -> vec2<f32> {
    let e = b - a;
    let f = d - a;
    let g = a - b + c - d;
    let h = p - a;
    let k2 = cross2(g, f);
    let k1 = cross2(e, f) + cross2(h, g);
    let k0 = cross2(h, e);
    var v: f32;
    if (abs(k2) < 1e-6) {
        if (abs(k1) < 1e-9) {
            return vec2<f32>(-1.0);
        }
        v = -k0 / k1;
    } else {
        let w = k1 * k1 - 4.0 * k0 * k2;
        if (w < 0.0) {
            return vec2<f32>(-1.0);
        }
        let sw = sqrt(w);
        v = (-k1 - sw) / (2.0 * k2);
        if (v < 0.0 || v > 1.0) {
            v = (-k1 + sw) / (2.0 * k2);
        }
    }
    let dx = e.x + g.x * v;
    let dy = e.y + g.y * v;
    var u: f32;
    if (abs(dx) > abs(dy)) {
        u = (h.x - f.x * v) / dx;
    } else {
        u = (h.y - f.y * v) / dy;
    }
    if (u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0) {
        return vec2<f32>(-1.0);
    }
    return vec2<f32>(u, v);
}

fn sl_valid(i: u32) -> bool {
    return slices_buf[i].info.z > 0.5;
}

fn sl_count() -> u32 {
    var n = 0u;
    for (var i = 0u; i < arrayLength(&slices_buf); i++) {
        if (sl_valid(i)) {
            n += 1u;
        }
    }
    return n;
}

// Topmost slice under uv: (u, v, index, 1) or (.., .., .., 0).
fn slice_at(uv: vec2<f32>) -> vec4<f32> {
    var hit = vec4<f32>(0.0);
    for (var i = 0u; i < arrayLength(&slices_buf); i++) {
        if (!sl_valid(i)) {
            continue;
        }
        let s = slices_buf[i];
        let st = inv_bilinear(uv, s.p01.xy, s.p01.zw, s.p23.xy, s.p23.zw);
        if (st.x >= 0.0) {
            hit = vec4<f32>(st, f32(i), 1.0);
        }
    }
    return hit;
}

// Bounding box (min, max) of slice i in pixels.
fn sl_box(i: u32) -> vec4<f32> {
    let s = slices_buf[i];
    let lo = min(min(s.p01.xy, s.p01.zw), min(s.p23.xy, s.p23.zw)) * fx.resolution;
    let hi = max(max(s.p01.xy, s.p01.zw), max(s.p23.xy, s.p23.zw)) * fx.resolution;
    return vec4<f32>(lo, hi);
}

// Sort key of slice i for Slice Delay's Order (Original Size Width Height X Y Name).
fn sl_key(i: u32, order: u32) -> f32 {
    let b = sl_box(i);
    let sz = b.zw - b.xy;
    switch order {
        case 1u: { return sz.x * sz.y; }
        case 2u: { return sz.x; }
        case 3u: { return sz.y; }
        case 4u: { return (b.x + b.z) * 0.5; }
        case 5u: { return (b.y + b.w) * 0.5; }
        case 6u: { return slices_buf[i].info.x; }
        default: { return f32(i); }
    }
}

// Rank of slice i among the real slices (ties by index).
fn sl_rank(i: u32, order: u32) -> u32 {
    let k = sl_key(i, order);
    var r = 0u;
    for (var j = 0u; j < arrayLength(&slices_buf); j++) {
        if (j == i || !sl_valid(j)) {
            continue;
        }
        let kj = sl_key(j, order);
        if (kj < k || (kj == k && j < i)) {
            r += 1u;
        }
    }
    return r;
}

// Content uv for a point `st` (0..1) of a container: mode 0 Stretch, 1 Fit
// (all content visible), 2 Fill (container covered). `ca` = container
// aspect, `ka` = content aspect. Outside the content: < 0 or > 1.
fn fit_uv(st: vec2<f32>, ca: f32, ka: f32, mode: u32) -> vec2<f32> {
    var t = st;
    let wider = ca > ka;
    if (mode == 1u) {
        if (wider) {
            t.x = (st.x - 0.5) * ca / ka + 0.5;
        } else {
            t.y = (st.y - 0.5) * ka / ca + 0.5;
        }
    } else if (mode == 2u) {
        if (wider) {
            t.y = (st.y - 0.5) * ka / ca + 0.5;
        } else {
            t.x = (st.x - 0.5) * ca / ka + 0.5;
        }
    }
    return t;
}

fn sl_aspect(i: u32) -> f32 {
    let b = sl_box(i);
    return max(b.z - b.x, 1.0) / max(b.w - b.y, 1.0);
}

fn e_slice_transform(uv: vec2<f32>) -> vec4<f32> {
    // Slice Transform: the layer drawn into every slice of the
    // advanced output (input rects). 0 Black BG, 1 Scaling Fill|Fit|Stretch|Mask|Mask Invert
    let black = PB(0u);
    let mode = u32(P(1u) + 0.5);
    let h = slice_at(uv);
    if (mode >= 3u) {
        let keep = (h.w > 0.5) != (mode == 4u);
        if (keep) {
            return tex(uv);
        }
        return black_or_clear(black);
    }
    if (h.w < 0.5) {
        return black_or_clear(black);
    }
    let m = select(select(0u, 2u, mode == 0u), 1u, mode == 1u); // Fill→2, Fit→1, Stretch→0
    let t = fit_uv(h.xy, sl_aspect(u32(h.z)), aspect(), m);
    if (!inside01(t)) {
        return black_or_clear(black);
    }
    let c = tex(t);
    if (black) {
        return vec4<f32>(c.rgb * c.a, 1.0);
    }
    return c;
}

fn e_slice_delay(uv: vec2<f32>) -> vec4<f32> {
    // Each slice shows an older frame, the delay spread over the slices in
    // the chosen Order (ring buffer + sort).
    // 0 Order Original|Size|Width|Height|X|Y|Name, 1 Scaling None|Fit|Fill|Stretch
    let h = slice_at(uv);
    if (h.w < 0.5) {
        return vec4<f32>(0.0);
    }
    let i = u32(h.z);
    let n = sl_count();
    let r = sl_rank(i, u32(P(0u) + 0.5));
    let span = f32(max(fx.depth, 1u) - 1u);
    let d = u32(round(f32(r) * span / f32(max(n, 2u) - 1u)));
    let sc = u32(P(1u) + 0.5);
    var t = uv;
    if (sc > 0u) {
        let m = select(select(0u, 2u, sc == 2u), 1u, sc == 1u); // Fit→1, Fill→2, Stretch→0
        t = fit_uv(h.xy, sl_aspect(i), aspect(), m);
        if (!inside01(t)) {
            return vec4<f32>(0.0);
        }
    }
    return unpremul(hist_pm(t, d));
}

fn e_uv_map(c: vec4<f32>) -> vec4<f32> {
    // The layer is a UV map: red/green pick where to read the layers below.
    // 0 Flip U, 1 Flip V, 2 Offset X, 3 Offset Y, 4 Filter Strength, 5-8 Filter Color, 9 Black Is Transparent
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    var t = c.rg;
    if (PB(0u)) {
        t.x = 1.0 - t.x;
    }
    if (PB(1u)) {
        t.y = 1.0 - t.y;
    }
    t = fract(t + vec2<f32>(P(2u), P(3u)));
    let s = unpremul(textureSampleLevel(below_tex, samp, t, 0.0));
    var rgb = s.rgb;
    let fc = PC(5u);
    rgb = mix(rgb, fc.rgb, clamp(P(4u) * fc.a, 0.0, 1.0));
    var a = c.a * max(s.a, select(0.0, 1.0, s.a <= 0.0 && fc.a > 0.0));
    if (PB(9u) && max(max(c.r, c.g), c.b) < 0.004) {
        a = 0.0;
    }
    return vec4<f32>(rgb, a);
}

fn e_mask(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> {
    // An image file as the mask. 0 Resize Fill|Fit|Stretch, 1 Channel Luminance|Red|Green|Blue|Alpha, 2 Invert
    let dim = vec2<f32>(textureDimensions(aux_tex));
    if (dim.x <= 1.0 && dim.y <= 1.0) {
        return c; // no image loaded
    }
    let rs = u32(P(0u) + 0.5);
    let m = select(select(0u, 1u, rs == 1u), 2u, rs == 0u); // Fill→2, Fit→1, Stretch→0
    let t = fit_uv(uv, aspect(), dim.x / dim.y, m);
    var v = 0.0;
    if (inside01(t)) {
        let s = textureSampleLevel(aux_tex, samp, t, 0.0);
        switch u32(P(1u) + 0.5) {
            case 1u: { v = s.r; }
            case 2u: { v = s.g; }
            case 3u: { v = s.b; }
            case 4u: { v = s.a; }
            default: { v = luma(s.rgb) * s.a; }
        }
    }
    if (PB(2u)) {
        v = 1.0 - v;
    }
    return vec4<f32>(c.rgb, c.a * v);
}

fn seg_dist(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - (a + ab * t));
}

// ------------------------------------------------------------------ Slice Chase (LAVMFX)

fn chase_at(i: u32) -> vec4<f32> {
    if (i >= arrayLength(&chase_buf)) {
        return vec4<f32>(0.0);
    }
    return chase_buf[i];
}

fn e_slice_chase(uv: vec2<f32>) -> vec4<f32> {
    // Each slice of the sequence shows the layer at its chase level.
    // 0 Scaling Mask|Fit|Fill|Stretch, 1 Unlit (level of dark slices),
    // 2 Outside Clear|Black|Keep (pixels in no slice of the sequence).
    let outside = u32(P(2u) + 0.5);
    let h = slice_at(uv);
    var d = vec4<f32>(0.0);
    if (h.w > 0.5) {
        d = chase_at(u32(h.z));
    }
    if (d.w < 0.5) {
        if (outside == 2u) {
            return tex(uv);
        }
        return black_or_clear(outside == 1u);
    }
    let level = mix(clamp(P(1u), 0.0, 1.0), 1.0, clamp(d.x, 0.0, 1.0));
    let sc = u32(P(0u) + 0.5);
    var t = uv;
    if (sc > 0u) {
        let m = select(select(0u, 2u, sc == 2u), 1u, sc == 1u); // Fit→1, Fill→2, Stretch→0
        t = fit_uv(h.xy, sl_aspect(u32(h.z)), aspect(), m);
        if (!inside01(t)) {
            return black_or_clear(outside == 1u);
        }
    }
    let c = tex(t);
    if (outside == 1u) {
        return vec4<f32>(c.rgb * c.a * level, 1.0);
    }
    return vec4<f32>(c.rgb, c.a * level);
}

// Point (s, t) of slice i's input quad (composition uv).
fn sl_point(i: u32, st: vec2<f32>) -> vec2<f32> {
    let s = slices_buf[i];
    let top = mix(s.p01.xy, s.p01.zw, st.x);
    let bottom = mix(s.p23.zw, s.p23.xy, st.x);
    return mix(top, bottom, st.y);
}

// The layer's colour in slice i: its centre, or the average of 5 × 5 points.
fn sl_sample(i: u32, average: bool) -> vec4<f32> {
    if (!average) {
        return tex(sl_point(i, vec2<f32>(0.5)));
    }
    var acc = vec4<f32>(0.0);
    for (var y = 0; y < 5; y++) {
        for (var x = 0; x < 5; x++) {
            let st = (vec2<f32>(f32(x), f32(y)) + 0.5) / 5.0;
            acc += textureSampleLevel(src_tex, samp, sl_point(i, st), 0.0);
        }
    }
    return unpremul(acc / 25.0);
}

// Slice Chase Wipe / Runner / Tiles / Sample (kinds 104–107): the slice under
// uv with its chase values and the look from chase.wgsl. `base` = slot of
// Unlit (Outside follows).
fn e_slice_chase_look(uv: vec2<f32>, look: u32, base: u32) -> vec4<f32> {
    let outside = u32(P(base + 1u) + 0.5);
    let h = slice_at(uv);
    var d = vec4<f32>(0.0);
    if (h.w > 0.5) {
        d = chase_at(u32(h.z));
    }
    if (d.w < 0.5) {
        if (outside == 2u) {
            return tex(uv);
        }
        return black_or_clear(outside == 1u);
    }
    let i = u32(h.z);
    let lvl = mix(clamp(P(base), 0.0, 1.0), 1.0, clamp(d.x, 0.0, 1.0));
    var smp = vec4<f32>(0.0);
    if (look == 4u) {
        smp = sl_sample(i, PB(0u));
    }
    let p = array<vec4<f32>, 3>(fx.p[0], fx.p[1], fx.p[2]);
    let r = chase_look(look, h.xy, sl_aspect(i), d, lvl, p, tex(uv), smp);
    if (outside == 1u) {
        return vec4<f32>(r.rgb * r.a, 1.0);
    }
    return r;
}

// Slice Chase Delay (kind 108): each step of the sequence shows the layer
// `Delay` frames later than the step before. 0 Delay, 1 Reverse (CPU), 2 Unlit, 3 Outside.
fn e_slice_chase_delay(uv: vec2<f32>) -> vec4<f32> {
    let outside = u32(P(3u) + 0.5);
    let h = slice_at(uv);
    var d = vec4<f32>(0.0);
    if (h.w > 0.5) {
        d = chase_at(u32(h.z));
    }
    if (d.w < 0.5) {
        if (outside == 2u) {
            return tex(uv);
        }
        return black_or_clear(outside == 1u);
    }
    let frames = u32(round(max(d.z, 0.0) * max(P(0u), 0.0)));
    let c = unpremul(hist_pm(uv, frames));
    let lvl = mix(clamp(P(2u), 0.0, 1.0), 1.0, clamp(d.x, 0.0, 1.0));
    if (outside == 1u) {
        return vec4<f32>(c.rgb * c.a * lvl, 1.0);
    }
    return vec4<f32>(c.rgb, c.a * lvl);
}

fn g_slice_outline(uv: vec2<f32>) -> vec4<f32> {
    // Outline of every slice.
    // 0-3 Color, 4 Thickness (of the slice's smaller side). Draws the slices'
    // OUTPUT quads (entries with info.w = 1, screen uv): the output lays this
    // layer over the screen, so no slice picks up another's outline.
    // Entries run bottom → top: inside a slice only the topmost slice's own
    // stroke shows (a slice above hides the outlines below it, like its picture).
    // Polygon slices come as runs of EDGE entries (info.w = 2, info.y = the
    // polygon's index, p01 = edge a → b, p23 = the polygon's bounding box):
    // inside = even-odd crossing test, stroke = distance to the nearest edge.
    let px = uv * fx.resolution;
    var top_k = -1.0;
    var outer = 0.0;
    // info.x = 1: hide-only (a slice above, per-slice passes): its inside
    // hides the stroke below, it draws none itself.
    var g = -1.0;
    var g_hide = false;
    var g_in = false;
    var g_d = 1e9;
    var g_w = 1.0;
    let n = arrayLength(&slices_buf);
    for (var i = 0u; i <= n; i++) {
        // A polygon run ended: apply it like a quad.
        var next_g = -2.0;
        if (i < n && slices_buf[i].info.w > 1.5) {
            next_g = slices_buf[i].info.y;
        }
        if (g >= 0.0 && next_g != g) {
            if (g_in && g_hide) {
                top_k = 0.0;
            } else if (g_in) {
                top_k = 1.0 - smoothstep(g_w - 0.5, g_w + 0.5, g_d);
            } else if (!g_hide) {
                outer = max(outer, 1.0 - smoothstep(0.0, 1.0, g_d));
            }
            g = -1.0;
        }
        if (i == n) {
            break;
        }
        if (slices_buf[i].info.w > 1.5) {
            let e = slices_buf[i];
            if (g < 0.0) {
                g = e.info.y;
                g_hide = e.info.x > 0.5;
                g_in = false;
                g_d = 1e9;
                let lo = e.p23.xy * fx.resolution;
                let hi = e.p23.zw * fx.resolution;
                g_w = max(P(4u) * 0.25 * min(hi.x - lo.x, hi.y - lo.y), 1.0);
            }
            let a = e.p01.xy * fx.resolution;
            let b = e.p01.zw * fx.resolution;
            g_d = min(g_d, seg_dist(px, a, b));
            if ((a.y > px.y) != (b.y > px.y)) {
                let x = a.x + (px.y - a.y) * (b.x - a.x) / (b.y - a.y);
                if (px.x < x) {
                    g_in = !g_in;
                }
            }
            continue;
        }
        if (slices_buf[i].info.w < 0.5) {
            continue;
        }
        let s = slices_buf[i];
        let q0 = s.p01.xy * fx.resolution;
        let q1 = s.p01.zw * fx.resolution;
        let q2 = s.p23.xy * fx.resolution;
        let q3 = s.p23.zw * fx.resolution;
        let b = sl_box(i);
        let w = max(P(4u) * 0.25 * min(b.z - b.x, b.w - b.y), 1.0);
        let d = min(min(seg_dist(px, q0, q1), seg_dist(px, q1, q2)), min(seg_dist(px, q2, q3), seg_dist(px, q3, q0)));
        let inside = inv_bilinear(uv, s.p01.xy, s.p01.zw, s.p23.xy, s.p23.zw).x >= 0.0;
        if (s.info.x > 0.5) {
            if (inside) {
                top_k = 0.0;
            }
        } else if (inside) {
            // Inner stroke, anti-aliased on its inner edge.
            top_k = 1.0 - smoothstep(w - 0.5, w + 0.5, d);
        } else {
            // 1 px anti-aliasing just outside the edge.
            outer = max(outer, 1.0 - smoothstep(0.0, 1.0, d));
        }
    }
    let a = select(outer, top_k, top_k >= 0.0);
    let col = PC(0u);
    return vec4<f32>(col.rgb, col.a * a);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy / fx.resolution;
    let c = tex(uv);
    var o: vec4<f32>;
    switch fx.kind {
        case 0u: { o = e_transform(uv); }
        case 1u: { o = e_blur(uv); }
        case 2u: { o = e_brightness_contrast(c); }
        case 3u: { o = e_hue_rotate(c); }
        case 4u: { o = e_saturation(c); }
        case 5u: { o = e_invert(c); }
        case 6u: { o = e_colorize(c); }
        case 7u: { o = e_tint(c); }
        case 8u: { o = e_threshold(c); }
        case 9u: { o = e_posterize(c); }
        case 10u: { o = e_exposure(c); }
        case 11u: { o = e_vignette(uv, c); }
        case 12u: { o = e_mirror(uv); }
        case 13u: { o = e_mirror_quad(uv); }
        case 14u: { o = e_flip(uv); }
        case 15u: { o = e_kaleidoscope(uv); }
        case 16u: { o = e_polar_kaleido(uv); }
        case 17u: { o = e_shift_rgb(uv); }
        case 18u: { o = e_wave_warp(uv); }
        case 19u: { o = e_strobe(c); }
        case 20u: { o = e_trails(uv); }
        case 21u: { o = e_fish_eye(uv); }
        case 22u: { o = e_tile(uv); }
        case 23u: { o = e_crop(uv, c); }
        case 24u: { o = e_edge(uv, c); }
        case 25u: { o = e_sharpen(uv, c); }
        case 26u: { o = e_solid(c); }
        case 27u: { o = e_auto_mask(c); }
        case 28u: { o = e_vibrance(c); }
        case 29u: { o = e_displace(uv); }
        case 30u: { o = e_twitch(uv); }
        case 31u: { o = e_drop_shadow(uv, c); }
        case 32u: { o = e_levels(c); }
        case 33u: { o = e_recolour(c); }
        case 34u: { o = e_color_pass(c); }
        case 35u: { o = e_add_subtract(c); }
        case 36u: { o = e_chroma_key(uv, c); }
        case 37u: { o = e_dither(uv, c); }
        case 38u: { o = e_dot_screen(uv, c); }
        case 39u: { o = e_hatched(uv, c); }
        case 40u: { o = e_lorez(uv); }
        case 41u: { o = e_pixel_blur(uv); }
        case 42u: { o = e_pixel_high_pass(uv, c); }
        case 43u: { o = e_polkadot(uv); }
        case 44u: { o = e_raster(uv, c); }
        case 45u: { o = e_noisy(uv); }
        case 46u: { o = e_static(uv); }
        case 47u: { o = e_snow(uv, c); }
        case 48u: { o = e_snowy(uv, c); }
        case 49u: { o = e_sparkles(uv, c); }
        case 50u: { o = e_grid(uv, c); }
        case 51u: { o = e_point_grid(uv); }
        case 52u: { o = e_circles(uv, c); }
        case 53u: { o = e_stripper(uv, c); }
        case 54u: { o = e_heat(c); }
        case 55u: { o = e_bendoscope(uv); }
        case 56u: { o = e_distortion(uv); }
        case 57u: { o = e_goo(uv); }
        case 58u: { o = e_ripples(uv); }
        case 59u: { o = e_sphere(uv); }
        case 60u: { o = e_stingy_sphere(uv); }
        case 61u: { o = e_twisted(uv); }
        case 62u: { o = e_tunnel(uv); }
        case 63u: { o = e_suckr(uv); }
        case 64u: { o = e_blow(uv); }
        case 65u: { o = e_slide(uv); }
        case 66u: { o = e_fragment(uv); }
        case 67u: { o = e_triangulate(uv); }
        case 68u: { o = e_cube_tiles(uv); }
        case 69u: { o = e_video_wall(uv); }
        case 70u: { o = e_infinite_zoom(uv); }
        case 71u: { o = e_warp_speed(uv); }
        case 72u: { o = e_radar(uv, c); }
        case 73u: { o = e_search_light(uv, c); }
        case 74u: { o = e_reveal(uv); }
        case 75u: { o = e_keystone(uv); }
        case 76u: { o = e_keystone_crop(uv); }
        case 77u: { o = e_keystone_mask(uv, c); }
        case 78u: { o = e_shift_glitch(uv); }
        case 79u: { o = e_linear_cloner(uv); }
        case 80u: { o = e_grid_cloner(uv); }
        case 81u: { o = e_radial_cloner(uv); }
        case 82u: { o = e_radial_blur(uv); }
        case 83u: { o = e_tilt_shift(uv); }
        case 84u: { o = e_dilate(uv, c); }
        case 85u: { o = e_kuwahara(uv); }
        case 86u: { o = e_acuarela(uv, c); }
        case 87u: { o = e_iterate(uv); }
        case 88u: { o = e_shaper(uv); }
        case 89u: { o = e_shape_mask(uv, c); }
        case 90u: { o = e_meta_shape(uv); }
        case 91u: { o = e_bloom(uv); }
        case 92u: { o = e_freeze(uv, c); }
        case 93u: { o = e_stop_motion(uv, c); }
        case 94u: { o = e_delay_rgb(uv); }
        case 95u, 96u: { o = e_text(uv, c); }
        case 97u: { o = e_particles(uv); }
        case 98u: { o = e_terrain(uv, c); }
        case 128u: { o = g_abstract_field(uv); }
        case 129u: { o = g_blank(); }
        case 130u: { o = g_checkered(uv); }
        case 131u: { o = g_gradient(uv); }
        case 132u: { o = g_line_scape(uv); }
        case 133u: { o = g_lines(uv); }
        case 134u: { o = g_metaballs(uv); }
        case 135u: { o = g_rings(uv); }
        case 136u: { o = g_shaper(uv); }
        case 137u: { o = g_sine_wave(uv); }
        case 138u: { o = g_spiral(uv); }
        case 139u: { o = g_stroboscope(); }
        case 140u: { o = g_tunnelines(uv); }
        case 99u: { o = e_slice_transform(uv); }
        case 100u: { o = e_slice_delay(uv); }
        case 101u: { o = e_uv_map(c); }
        case 102u: { o = e_mask(uv, c); }
        case 103u: { o = e_slice_chase(uv); }
        case 104u: { o = e_slice_chase_look(uv, 1u, 5u); }
        case 105u: { o = e_slice_chase_look(uv, 2u, 12u); }
        case 106u: { o = e_slice_chase_look(uv, 3u, 12u); }
        case 107u: { o = e_slice_chase_look(uv, 4u, 3u); }
        case 108u: { o = e_slice_chase_delay(uv); }
        case 141u: { o = g_slice_outline(uv); }
        // ISF shader output (straight alpha), mixed like any effect.
        case 999u: { o = textureSampleLevel(prev_tex, samp, uv, 0.0); }
        default: { o = c; }
    }
    o = clamp(o, vec4<f32>(0.0), vec4<f32>(1.0));
    if (fx.pass_ + 1u < fx.passes) {
        return vec4<f32>(o.rgb * o.a, o.a); // intermediate pass of a multi-pass effect
    }
    var dry = c;
    if (fx.passes > 1u) {
        dry = orig(uv); // dry = the effect's input
    }
    // Dry/wet: blend the effect over the original by Opacity.
    let k = clamp(fx.opacity, 0.0, 1.0);
    var wet = o;
    if (fx.blend != 0u) {
        wet = vec4<f32>(clamp(blend(dry.rgb, o.rgb, fx.blend), vec3<f32>(0.0), vec3<f32>(1.0)), max(dry.a, o.a));
    }
    let r = mix(dry, wet, k);
    return vec4<f32>(r.rgb * r.a, r.a);
}
