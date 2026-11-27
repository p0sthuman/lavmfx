// Layer compositing.
//
// fs_layer: composites one layer over the accumulated composition `dst`.
//   The layer is `cur` (premultiplied), or during a transition a mix of the
//   outgoing clip `prev` and `cur`. Result = mix(dst, blend(dst, layer), coverage).
//   With `alpha_dst` (layer groups) `dst` may be transparent and is stored
//   premultiplied; the result is premultiplied with alpha (W3C compositing).
// fs_present: `dst` × master → composition texture (opaque).
//
// Mode indices follow lavmfx-core `mix::SHADER_BLENDS`: 0–22 per-pixel blends,
// 23+ spatial / transition-style mixers (Push, Wipe, Zoom, Cube, "50"
// modes…) driven by a phase t (layer opacity, or transition progress).
// The block between MIXER BEGIN / MIXER END is instantiated twice by the
// renderer (`_X` → `_L`: base = composition, top = layer; `_X` → `_T`: base =
// outgoing clip, top = incoming clip): WGSL has no recursion, and the layer's
// own image (`layer_color`) uses the transition copy.

struct Mix {
    opacity: f32,
    mode: u32,
    trans_mode: u32,
    // transition progress 0..1; < 0 = no transition
    progress: f32,
    master: f32,
    alpha_dst: f32,
    // composition time in seconds (TimeSwitcher, Static, JitterBug, Twitch)
    time: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> m: Mix;
@group(0) @binding(1) var dst: texture_2d<f32>;
@group(0) @binding(2) var prev: texture_2d<f32>;
@group(0) @binding(3) var cur: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.3, 0.59, 0.11));
}

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    var o = c;
    if (n < 0.0) {
        o = l + (o - l) * l / max(l - n, 1e-5);
    }
    if (x > 1.0) {
        o = l + (o - l) * (1.0 - l) / max(x - l, 1e-5);
    }
    return o;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let n = min(min(c.r, c.g), c.b);
    let range = max(max(c.r, c.g), c.b) - n;
    if (range <= 1e-5) {
        return vec3<f32>(0.0);
    }
    return (c - n) * s / range;
}

fn hard_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let lo = b * 2.0 * s;
    let s2 = 2.0 * s - 1.0;
    let hi = b + s2 - b * s2;
    return select(hi, lo, s <= vec3<f32>(0.5));
}

fn soft_light(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let d = select(sqrt(b), ((16.0 * b - 12.0) * b + 4.0) * b, b <= vec3<f32>(0.25));
    let lo = b - (1.0 - 2.0 * s) * b * (1.0 - b);
    let hi = b + (2.0 * s - 1.0) * (d - b);
    return select(hi, lo, s <= vec3<f32>(0.5));
}

fn dodge(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return select(min(vec3<f32>(1.0), b / max(1.0 - s, vec3<f32>(1e-5))), vec3<f32>(0.0), b <= vec3<f32>(0.0));
}

fn burn(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    let v = 1.0 - min(vec3<f32>(1.0), (1.0 - b) / max(s, vec3<f32>(1e-5)));
    return select(v, vec3<f32>(1.0), b >= vec3<f32>(1.0));
}

// Blend `s` (top) onto `b` (base), both straight colour.
fn blend(mode: u32, b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return b + s; }
        case 2u: { return b - s; }
        case 3u: { return b * s; }
        case 4u: { return b + s - b * s; }
        case 5u: { return hard_light(s, b); }
        case 6u: { return min(b, s); }
        case 7u: { return max(b, s); }
        case 8u: { return abs(b - s); }
        case 9u: { return b + s - 2.0 * b * s; }
        case 10u: { return hard_light(b, s); }
        case 11u: { return soft_light(b, s); }
        case 12u: { return dodge(b, s); }
        case 13u: { return burn(b, s); }
        case 14u: { return b + 2.0 * s - 1.0; }
        case 15u: { return set_lum(set_sat(s, sat(b)), lum(b)); }
        case 16u: { return set_lum(set_sat(b, sat(s)), lum(b)); }
        case 17u: { return set_lum(s, lum(b)); }
        case 18u: { return set_lum(b, lum(s)); }
        // Difference I: difference against the inverted top (guess).
        case 20u: { return abs(1.0 - b - s); }
        // 0 Alpha, 19 Luma Key, 21 Luma Is Alpha, 22 Luma Key I: the caller
        // scales the coverage by `luma_cover`.
        default: { return s; }
    }
}

fn unpremul(c: vec4<f32>) -> vec3<f32> {
    return select(vec3<f32>(0.0), c.rgb / c.a, c.a > 0.0);
}

// Coverage factor of the luma modes (1 for the others): 19 Luma Key keys out
// dark parts (soft threshold), 21 Luma Is Alpha = luma as alpha, 22 Luma Key I
// keys out bright parts.
fn luma_cover(mode: u32, top: vec3<f32>) -> f32 {
    let l = clamp(lum(top), 0.0, 1.0);
    switch mode {
        case 19u: { return smoothstep(0.02, 0.3, l); }
        case 21u: { return l; }
        case 22u: { return 1.0 - smoothstep(0.7, 0.98, l); }
        default: { return 1.0; }
    }
}

fn is_luma(mode: u32) -> bool {
    return mode == 19u || mode == 21u || mode == 22u;
}

fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(123.34, 456.21));
    let r = q + dot(q, q + 45.32);
    return fract(r.x * r.y);
}

fn gray(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(vec3<f32>(lum(c.rgb)), c.a);
}

// Three-step mix base → mid (at t = 0.5) → top.
fn via(b: vec4<f32>, mid: vec4<f32>, s: vec4<f32>, t: f32) -> vec4<f32> {
    if (t < 0.5) {
        return mix(b, mid, t * 2.0);
    }
    return mix(mid, s, t * 2.0 - 1.0);
}

// `src` over `dst`, premultiplied.
fn over(src: vec4<f32>, dst: vec4<f32>) -> vec4<f32> {
    return src + dst * (1.0 - src.a);
}

// `top` over `base` (both premultiplied) with `mode`, at coverage `k`.
fn mix_premul(mode: u32, base: vec4<f32>, top: vec4<f32>, k: f32) -> vec4<f32> {
    if (mode == 0u) {
        return mix(base, top, k);
    }
    let ub = unpremul(base);
    let ut = unpremul(top);
    var c = clamp(blend(mode, ub, ut), vec3<f32>(0.0), vec3<f32>(1.0));
    if (is_luma(mode)) {
        c = mix(ub, ut, luma_cover(mode, ut));
    }
    let a = max(base.a, top.a);
    return mix(base, vec4<f32>(c * a, a), k);
}

// The layer's image: `cur`, or the transition from `prev` to `cur`.
// Non-alpha transitions pass through blend(prev, cur) halfway.
fn layer_color(p: vec2<i32>) -> vec4<f32> {
    let b = textureLoad(cur, p, 0);
    if (m.progress < 0.0) {
        return b;
    }
    let a = textureLoad(prev, p, 0);
    let t = clamp(m.progress, 0.0, 1.0);
    if (m.trans_mode == 0u) {
        return mix(a, b, t);
    }
    if (m.trans_mode >= SPATIAL) {
        return spatial_T(m.trans_mode, vec2<f32>(p) + 0.5, t);
    }
    let mid = mix_premul(m.trans_mode, a, b, 1.0);
    if (t < 0.5) {
        return mix(a, mid, t * 2.0);
    }
    return mix(mid, b, t * 2.0 - 1.0);
}

@fragment
fn fs_layer(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let d = textureLoad(dst, p, 0);
    let s = layer_color(p);
    if (m.mode >= SPATIAL) {
        // Transition-style mode: the layer opacity is the phase.
        let r = clamp(spatial_L(m.mode, pos.xy, clamp(m.opacity, 0.0, 1.0)), vec4<f32>(0.0), vec4<f32>(1.0));
        if (m.alpha_dst > 0.5) {
            return r;
        }
        return vec4<f32>(r.rgb, 1.0);
    }
    let top = unpremul(s);
    var k = s.a * m.opacity * luma_cover(m.mode, top);
    if (m.alpha_dst > 0.5) {
        // Group buffer: Cs' = (1-ab)·Cs + ab·B(Cb, Cs); co = k·Cs' + (1-k)·ab·Cb.
        let ab = d.a;
        let cb = unpremul(d);
        let cs = mix(top, blend(m.mode, cb, top), ab);
        let co = k * cs + (1.0 - k) * d.rgb;
        return vec4<f32>(clamp(co, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(k + ab * (1.0 - k), 0.0, 1.0));
    }
    // Clamp after mixing so Add/Subtract scale with opacity (d ± s·k).
    let c = mix(d.rgb, blend(m.mode, d.rgb, top), k);
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

@fragment
fn fs_present(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let d = textureLoad(dst, vec2<i32>(pos.xy), 0);
    return vec4<f32>(d.rgb * m.master, 1.0);
}

// ------------------------------------------------------------ spatial mixers

const SPATIAL: u32 = 23u;

// Layer over composition.
fn dims_L() -> vec2<f32> {
    return vec2<f32>(textureDimensions(dst));
}
fn base_L(q: vec2<f32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(dst));
    return textureLoad(dst, clamp(vec2<i32>(floor(q)), vec2<i32>(0), d - 1), 0);
}
// The layer as seen over the composition (its transparent parts show the base).
// During a transition this reads a plain crossfade, not the transition's own
// spatial mixer: nesting the two made the shader ~60× bigger (minutes to
// compile).
fn top_L(q: vec2<f32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(dst));
    let p = clamp(vec2<i32>(floor(q)), vec2<i32>(0), d - 1);
    var c = textureLoad(cur, p, 0);
    if (m.progress >= 0.0) {
        c = mix(textureLoad(prev, p, 0), c, clamp(m.progress, 0.0, 1.0));
    }
    return over(c, textureLoad(dst, p, 0));
}

// Outgoing → incoming clip.
fn dims_T() -> vec2<f32> {
    return vec2<f32>(textureDimensions(cur));
}
fn base_T(q: vec2<f32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(prev));
    return textureLoad(prev, clamp(vec2<i32>(floor(q)), vec2<i32>(0), d - 1), 0);
}
fn top_T(q: vec2<f32>) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(cur));
    return textureLoad(cur, clamp(vec2<i32>(floor(q)), vec2<i32>(0), d - 1), 0);
}

// MIXER BEGIN
// Spatial mixer `mode` at pixel `q` (pixel centre), phase `t` (0 = base,
// 1 = top), premultiplied. Looks follow the mode names.
fn spatial_X(mode: u32, q: vec2<f32>, t: f32) -> vec4<f32> {
    let size = dims_X();
    let uv = q / size;
    let aspect = size.x / size.y;
    let w = sin(t * 3.14159265); // 0 at both ends, 1 halfway
    let none = vec4<f32>(0.0);
    switch mode {
        // Cut: hard switch halfway.
        case 23u: { return select(base_X(q), top_X(q), t >= 0.5); }
        // Dissolve: random pixels switch.
        case 24u: { return select(base_X(q), top_X(q), hash(floor(q)) < t); }
        // Push Left / Right / Up / Down: the top pushes the base out.
        case 25u: {
            if (uv.x < 1.0 - t) { return base_X(q + vec2<f32>(t * size.x, 0.0)); }
            return top_X(q - vec2<f32>((1.0 - t) * size.x, 0.0));
        }
        case 26u: {
            if (uv.x >= t) { return base_X(q - vec2<f32>(t * size.x, 0.0)); }
            return top_X(q + vec2<f32>((1.0 - t) * size.x, 0.0));
        }
        case 27u: {
            if (uv.y < 1.0 - t) { return base_X(q + vec2<f32>(0.0, t * size.y)); }
            return top_X(q - vec2<f32>(0.0, (1.0 - t) * size.y));
        }
        case 28u: {
            if (uv.y >= t) { return base_X(q - vec2<f32>(0.0, t * size.y)); }
            return top_X(q + vec2<f32>(0.0, (1.0 - t) * size.y));
        }
        // Wipe Ellipse: the top grows from the centre in an ellipse.
        case 29u: {
            let d = length((uv - 0.5) * 2.0) / 1.4143;
            let e = 2.0 / size.y;
            return mix(base_X(q), top_X(q), 1.0 - smoothstep(t - e, t + e, d));
        }
        // Zoom In: the top grows from the centre. Zoom Out: the base shrinks away.
        case 30u: {
            if (t > 0.0 && all(abs(uv - 0.5) <= vec2<f32>(t * 0.5))) {
                return top_X(((uv - 0.5) / t + 0.5) * size);
            }
            return base_X(q);
        }
        case 31u: {
            let s = 1.0 - t;
            if (s > 0.0 && all(abs(uv - 0.5) <= vec2<f32>(s * 0.5))) {
                return base_X(((uv - 0.5) / s + 0.5) * size);
            }
            return top_X(q);
        }
        // Cube: rotate a cube to the left; faces shaded by their angle.
        case 32u: {
            let edge = 1.0 - t;
            if (uv.x < edge) {
                let u = uv.x / max(edge, 1e-4);
                let squeeze = 1.0 - 0.15 * w * u;
                let v = (uv.y - 0.5) / squeeze + 0.5;
                if (v < 0.0 || v > 1.0) { return none; }
                return base_X(vec2<f32>(u, v) * size) * (1.0 - 0.5 * t);
            }
            let u = (uv.x - edge) / max(t, 1e-4);
            let squeeze = 1.0 - 0.15 * w * (1.0 - u);
            let v = (uv.y - 0.5) / squeeze + 0.5;
            if (v < 0.0 || v > 1.0) { return none; }
            return top_X(vec2<f32>(u, v) * size) * (0.5 + 0.5 * t);
        }
        // Rotate X / Y: flip around the horizontal / vertical axis.
        case 33u, 34u: {
            let c = abs(cos(t * 3.14159265));
            let axis = select(uv.y, uv.x, mode == 34u);
            let a = (axis - 0.5) / max(c, 1e-4) + 0.5;
            if (a < 0.0 || a > 1.0) { return none; }
            var r = uv;
            if (mode == 34u) { r.x = a; } else { r.y = a; }
            return select(base_X(r * size), top_X(r * size), t >= 0.5);
        }
        // to Black / to White / to Color: through a colour halfway.
        case 35u: { return via(base_X(q), vec4<f32>(0.0, 0.0, 0.0, 1.0), top_X(q), t); }
        case 36u: { return via(base_X(q), vec4<f32>(1.0), top_X(q), t); }
        case 37u: { return via(base_X(q), vec4<f32>(1.0, 0.0, 0.0, 1.0), top_X(q), t); }
        // B&W: through greyscale.
        case 38u: {
            let b = base_X(q);
            let s = top_X(q);
            return via(b, mix(gray(b), gray(s), t), s, t);
        }
        // RGB: red, then green, then blue switch.
        case 39u: {
            let b = base_X(q);
            let s = top_X(q);
            let k = vec3<f32>(smoothstep(0.0, 0.34, t), smoothstep(0.33, 0.67, t), smoothstep(0.66, 1.0, t));
            return vec4<f32>(mix(b.rgb, s.rgb, k), mix(b.a, s.a, t));
        }
        // Shift RGB: channels drift apart halfway.
        case 40u: {
            let o = vec2<f32>(0.04 * w * size.x, 0.0);
            let r = mix(base_X(q + o), top_X(q + o), t);
            let g = mix(base_X(q), top_X(q), t);
            let b = mix(base_X(q - o), top_X(q - o), t);
            return vec4<f32>(r.r, g.g, b.b, g.a);
        }
        // LoRez: pixelate up to halfway, then sharpen into the top.
        case 41u: {
            let cell = 1.0 + 63.0 * w;
            let c = (floor(q / cell) + 0.5) * cell;
            return mix(base_X(c), top_X(c), t);
        }
        // Noisy: noise added halfway.
        case 42u: {
            let n = hash(floor(q) + fract(m.time) * 91.7) - 0.5;
            let c = mix(base_X(q), top_X(q), t);
            return vec4<f32>(c.rgb + n * w * 0.6, c.a);
        }
        // Static: through TV static.
        case 43u: {
            let n = hash(floor(q) + floor(m.time * 30.0) * 17.3);
            return mix(mix(base_X(q), top_X(q), t), vec4<f32>(vec3<f32>(n), 1.0), w);
        }
        // Parts: blocks switch in random order.
        case 44u: {
            let cell = floor(uv * vec2<f32>(16.0, 9.0));
            return select(base_X(q), top_X(q), hash(cell + 3.1) < t);
        }
        // Tile: the top comes in tiled, the tiles growing until it fills the frame.
        case 45u: {
            let n = max(1.0, floor(8.0 * (1.0 - t)) + 1.0);
            return mix(base_X(q), top_X(fract(uv * n) * size), t);
        }
        // PiP: the top as a picture in the top-right corner, growing.
        case 46u: {
            let lo = vec2<f32>(1.0 - t, 0.0);
            if (t > 0.0 && uv.x >= lo.x && uv.y <= t) {
                return top_X((uv - lo) / t * size);
            }
            return base_X(q);
        }
        // Side by Side: base squeezed left, top squeezed right.
        case 47u: {
            let edge = 1.0 - t;
            if (uv.x < edge) { return base_X(vec2<f32>(uv.x / max(edge, 1e-4), uv.y) * size); }
            return top_X(vec2<f32>((uv.x - edge) / max(t, 1e-4), uv.y) * size);
        }
        // Displace: the top's luma displaces both halfway.
        case 48u: {
            let d = (lum(top_X(q).rgb) - 0.5) * 0.15 * w * size.y;
            let o = vec2<f32>(d, d);
            return mix(base_X(q + o), top_X(q + o), t);
        }
        // JitterBug: rows jitter sideways while random rows switch.
        case 49u: {
            let row = floor(q.y / 24.0);
            let tick = floor(m.time * 15.0);
            let o = vec2<f32>((hash(vec2<f32>(row, tick)) - 0.5) * 0.2 * w * size.x, 0.0);
            return select(base_X(q + o), top_X(q + o), hash(vec2<f32>(row, tick + 7.0)) < t);
        }
        // Twitch: the frame jumps and flickers halfway.
        case 50u: {
            let tick = floor(m.time * 20.0);
            let o = (vec2<f32>(hash(vec2<f32>(tick, 1.0)), hash(vec2<f32>(tick, 2.0))) - 0.5) * 0.08 * w * size;
            let c = mix(base_X(q + o), top_X(q + o), t);
            let f = 1.0 + (hash(vec2<f32>(tick, 3.0)) - 0.5) * w;
            return vec4<f32>(c.rgb * f, c.a);
        }
        // TimeSwitcher: flips between base and top (4 per second), the top's
        // share of the time = t.
        case 51u: { return select(base_X(q), top_X(q), fract(m.time * 4.0) < t); }
        // Meta Mix: luma wipe, brightest parts of the top first.
        case 52u: {
            let s = top_X(q);
            let l = lum(s.rgb);
            return mix(base_X(q), s, smoothstep(1.0 - t - 0.05, 1.0 - t + 0.05, l * (1.0 - 1e-3) + t * 1e-3));
        }
        // Multi Task: 2×2 miniatures, quarters switching one by one.
        case 53u: {
            let cell = floor(uv * 2.0);
            let i = cell.x + cell.y * 2.0;
            let r = fract(uv * 2.0) * size;
            return select(base_X(r), top_X(r), i < floor(t * 4.999));
        }
        // "50" modes: the blend at t = 0.5, base / top at the ends.
        default: {
            let b = base_X(q);
            let s = top_X(q);
            let ub = unpremul(b);
            let us = unpremul(s);
            var c: vec3<f32>;
            switch mode {
                case 54u: { c = ub + us; }
                case 55u: { c = burn(ub, us); }
                case 56u: { c = abs(ub - us); }
                case 57u: { c = vec3<f32>(length(ub - us) / 1.732); }
                case 58u: { c = dodge(ub, us); }
                case 59u: { c = max(ub, us); }
                case 60u: { c = ub * lum(us); }
                case 61u: { c = ub * us; }
                case 62u: { c = hard_light(us, ub); }
                case 63u: { c = ub - us; }
                default: { c = us; }
            }
            let a = max(b.a, s.a);
            return via(b, vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * a, a), s, t);
        }
    }
}
// MIXER END
