
// ------------------------------------------------------------------ Slice Chase looks (LAVMFX)
// Patterns drawn inside each slice. Shared by the effect shader (Input
// space: appended to effects.wgsl) and the output slice shader (Output
// space: appended to slices.wgsl), so both draw the same thing.
//
// Arguments: `look` 1 Wipe, 2 Runner, 3 Tiles, 4 Sample (effect kind − 103);
// `st` the position in the slice, 0..1 (y down); `asp` the slice's width /
// height; `v` its chase values (level, phase in its step, step, in the
// sequence); `lvl` the level with Unlit applied; `p` the effect's first 12
// shader params (its look params, in `SHADER_EFFECTS` order); `src` the
// picture there (straight alpha); `smp` the slice's sampled colour (Sample).
// Returns the slice's colour (straight alpha).

fn ch_hash(n: f32) -> f32 {
    return fract(sin(n * 127.1 + 311.7) * 43758.5453);
}

fn ch_rainbow(h: f32) -> vec3<f32> {
    let p = abs(fract(vec3<f32>(h) + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0));
}

// How far the step's animation is (0..1): `per_step` runs per step; once
// the slice is released (phase 1) it is done.
fn ch_progress(phase: f32, per_step: f32) -> f32 {
    if (phase >= 0.999) {
        return 1.0;
    }
    let q = phase * max(per_step, 0.01);
    if (per_step <= 1.0) {
        return min(q, 1.0);
    }
    return fract(q);
}

// Odd steps flip when Alternate is on.
fn ch_flip(alt: f32, step: f32) -> bool {
    return alt > 0.5 && (u32(step + 0.5) % 2u) == 1u;
}

// Wipe: 0 Direction (L→R R→L T→B B→T Centre→Out Out→Centre Diagonal),
// 1 Style (Reveal Hide Bar), 2 Softness, 3 Wipes per Step, 4 Alternate.
fn ch_wipe(st: vec2<f32>, v: vec4<f32>, q: array<vec4<f32>, 3>) -> f32 {
    let p0 = q[0];
    let p1 = q[1];
    let dir = u32(p0.x + 0.5);
    let style = u32(p0.y + 0.5);
    let w = max(p0.z, 0.001);
    var c: f32;
    switch dir {
        case 0u: { c = st.x; }
        case 1u: { c = 1.0 - st.x; }
        case 2u: { c = st.y; }
        case 3u: { c = 1.0 - st.y; }
        case 4u: { c = max(abs(st.x - 0.5), abs(st.y - 0.5)) * 2.0; }
        case 5u: { c = 1.0 - max(abs(st.x - 0.5), abs(st.y - 0.5)) * 2.0; }
        default: { c = (st.x + st.y) * 0.5; }
    }
    if (ch_flip(p1.x, v.z)) {
        c = 1.0 - c;
    }
    // The front runs from 0 to 1 + softness, so it starts and ends off the slice.
    let f = ch_progress(v.y, p0.w) * (1.0 + w);
    switch style {
        case 1u: { return smoothstep(f - w, f, c); }
        case 2u: {
            let b = abs(c - (f - w * 0.5));
            return 1.0 - smoothstep(w * 0.25, w * 0.5, b);
        }
        default: { return 1.0 - smoothstep(f - w, f, c); }
    }
}

// Runner: 0-3 Line Color, 4 Line Width (of the slice's smaller side),
// 5 Feather, 6 Segments, 7 Segment Length (of the outline), 8 Laps per
// Step, 9 Mode (Run Outline Fill Up), 10 Content (Color Layer), 11 Alternate.
fn ch_runner(st: vec2<f32>, asp: f32, v: vec4<f32>, q: array<vec4<f32>, 3>) -> f32 {
    let p1 = q[1];
    let p2 = q[2];
    let w = max(asp, 0.001);
    let x = st.x * w;
    let y = st.y;
    let lw = max(p1.x, 0.0005) * min(w, 1.0);
    let d = min(min(x, w - x), min(y, 1.0 - y));
    let line = 1.0 - smoothstep(lw * (1.0 - clamp(p1.y, 0.0, 1.0)), lw, d);
    if (line <= 0.0) {
        return 0.0;
    }
    // Position along the outline, clockwise from the top-left corner, 0..1.
    var t: f32;
    if (d == y) {
        t = x;
    } else if (d == w - x) {
        t = w + y;
    } else if (d == 1.0 - y) {
        t = w + 1.0 + (w - x);
    } else {
        t = 2.0 * w + 1.0 + (1.0 - y);
    }
    t = t / (2.0 * w + 2.0);
    if (ch_flip(p2.w, v.z)) {
        t = 1.0 - t;
    }
    let mode = u32(p2.y + 0.5);
    if (mode == 1u) {
        return line;
    }
    if (mode == 2u) {
        return line * step(t, ch_progress(v.y, 1.0));
    }
    let segs = max(round(p1.z), 1.0);
    let len = clamp(p1.w, 0.001, 1.0);
    let head = fract(v.y * p2.x);
    var best = 0.0;
    for (var s = 0.0; s < segs; s += 1.0) {
        let start = fract(head + s / segs);
        // How far behind this segment's head the point is.
        let behind = fract(start - t + 1.0);
        if (behind < len) {
            best = max(best, 1.0 - behind / len);
        }
    }
    return line * best;
}

// Tiles: 0-3 Tile Color, 4 Tiles X, 5 Tiles Y, 6 Shape (Square Circle
// Diamond), 7 Size, 8 Softness, 9 Build (All Sweep Random Twinkle),
// 10 Coloring (Layer Color Rainbow), 11 Edges Only.
// Returns (coverage, the tile's place 0..1 for Rainbow).
fn ch_tiles(st: vec2<f32>, asp: f32, v: vec4<f32>, q: array<vec4<f32>, 3>) -> vec2<f32> {
    let p1 = q[1];
    let p2 = q[2];
    let n = vec2<f32>(max(round(p1.x), 1.0), max(round(p1.y), 1.0));
    let g = st * n;
    let id = min(floor(g), n - 1.0);
    // Tile-local position in units of the slice height (circles stay round).
    let tile = vec2<f32>(max(asp, 0.001) / n.x, 1.0 / n.y);
    let lp = (g - id - 0.5) * tile;
    let size = clamp(p1.w, 0.0, 1.0);
    let half = size * tile * 0.5;
    let r = min(half.x, half.y);
    var dd: f32;
    switch u32(p1.z + 0.5) {
        case 1u: { dd = length(lp) - r; }
        case 2u: { dd = abs(lp.x) + abs(lp.y) - r; }
        default: { dd = max(abs(lp.x) - half.x, abs(lp.y) - half.y); }
    }
    let soft = clamp(p2.x, 0.0, 1.0) * r + 1e-4;
    var m = 1.0 - smoothstep(-soft, 0.0, dd);
    if (p2.w > 0.5) {
        let lw = 0.12 * r + soft * 0.5;
        m = 1.0 - smoothstep(lw * 0.5, lw, abs(dd));
    }
    let idx = id.y * n.x + id.x;
    let place = (idx + 0.5) / (n.x * n.y);
    var on = 1.0;
    switch u32(p2.y + 0.5) {
        case 1u: { on = step(place, ch_progress(v.y, 1.0) + 1e-4); }
        case 2u: { on = step(ch_hash(idx * 13.1 + v.z * 7.7 + 1.0), ch_progress(v.y, 1.0) + 1e-4); }
        case 3u: { on = step(0.5, ch_hash(idx * 3.3 + floor(v.y * 8.0) * 17.0 + v.z * 5.1)); }
        default: {}
    }
    return vec2<f32>(m * on, place);
}

fn chase_look(
    look: u32,
    st: vec2<f32>,
    asp: f32,
    v: vec4<f32>,
    lvl: f32,
    p: array<vec4<f32>, 3>,
    src: vec4<f32>,
    smp: vec4<f32>,
) -> vec4<f32> {
    switch look {
        case 1u: {
            return vec4<f32>(src.rgb, src.a * ch_wipe(st, v, p) * lvl);
        }
        case 2u: {
            var c = p[0];
            if (u32(p[2].z + 0.5) == 1u) {
                c = src;
            }
            return vec4<f32>(c.rgb, c.a * ch_runner(st, asp, v, p) * lvl);
        }
        case 3u: {
            let t = ch_tiles(st, asp, v, p);
            var c = src;
            switch u32(p[2].z + 0.5) {
                case 1u: { c = p[0]; }
                case 2u: { c = vec4<f32>(ch_rainbow(t.y + v.z * 0.13), 1.0); }
                default: {}
            }
            return vec4<f32>(c.rgb, c.a * t.x * lvl);
        }
        case 4u: {
            // 0 Sample Point (the caller samples), 1 As (Color Luminance), 2 Mix.
            var s = smp;
            if (p[0].y > 0.5) {
                s = vec4<f32>(vec3<f32>(dot(s.rgb, vec3<f32>(0.2126, 0.7152, 0.0722))), s.a);
            }
            let c = mix(src, s, clamp(p[0].z, 0.0, 1.0));
            return vec4<f32>(c.rgb, c.a * lvl);
        }
        default: {
            return vec4<f32>(src.rgb, src.a * lvl);
        }
    }
}
