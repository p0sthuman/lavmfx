// Advanced output: draws slice meshes (composition → screen).

struct ScreenU {
    // screen size px, opacity, show test card (unused here)
    size: vec4<f32>,
    // brightness, contrast, 0, 0
    adjust: vec4<f32>,
    rgb: vec4<f32>,
};

struct SliceP {
    // brightness, contrast, black bg, enabled soft edge
    adjust: vec4<f32>,
    rgb: vec4<f32>,
    // left, right, top, bottom
    edge: vec4<f32>,
    // luminance, power, gamma, 0
    curve: vec4<f32>,
    // x = alpha multiplier
    mult: vec4<f32>,
    // input quad corners (composition uv): TL TR, BR BL
    inq: array<vec4<f32>, 2>,
    // size on the screen (w, h px), number of looks
    size: vec4<f32>,
    looks: array<ChaseLook, 4>,
};

// A Slice Chase look in Output space (the pattern code is chase.wgsl).
struct ChaseLook {
    // look (1 Wipe 2 Runner 3 Tiles 4 Sample), level, phase, step
    a: vec4<f32>,
    // level with Unlit, dry/wet, Outside (0 Clear 1 Black 2 Keep), in the sequence
    b: vec4<f32>,
    p: array<vec4<f32>, 3>,
};

@group(0) @binding(0) var<uniform> screen: ScreenU;
@group(0) @binding(1) var<storage, read> slices: array<SliceP>;
@group(0) @binding(2) var comp: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
// Key slice rendered just before ("Is Key"), sampled in screen space.
@group(0) @binding(4) var keytex: texture_2d<f32>;

struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) local: vec2<f32>,
    @location(3) slice: u32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) slice: u32,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    let ndc = v.pos / screen.size.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = v.uv;
    o.local = v.local;
    o.slice = v.slice;
    return o;
}

fn adjust(c: vec3<f32>, a: vec4<f32>, rgb: vec4<f32>) -> vec3<f32> {
    var x = (c - 0.5) * (1.0 + a.y) + 0.5 + a.x;
    x = x + rgb.xyz;
    return clamp(x, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn curve(x: f32, k: vec4<f32>) -> f32 {
    let a = clamp(k.x, 0.0, 1.0);
    let p = max(k.y, 0.01);
    var y: f32;
    if (x < 0.5) {
        y = a * pow(2.0 * x, p);
    } else {
        y = 1.0 - (1.0 - a) * pow(2.0 * (1.0 - x), p);
    }
    return pow(max(y, 0.0), 1.0 / max(k.z, 0.01));
}

fn ramp(x: f32, w: f32, k: vec4<f32>) -> f32 {
    if (w <= 0.0) {
        return 1.0;
    }
    return curve(clamp(x / w, 0.0, 1.0), k);
}

// Slice colour before the screen look: adjust, Black BG, soft edge.
fn shade(i: VsOut) -> vec4<f32> {
    let s = slices[i.slice];
    let t = textureSample(comp, samp, i.uv);
    var rgb = adjust(t.rgb, s.adjust, s.rgb);
    var a = t.a;
    if (s.adjust.z > 0.5) {
        // Black BG: transparent composition pixels become black.
        rgb = rgb * a;
        a = 1.0;
    }
    if (s.adjust.w > 0.5) {
        let e = s.edge;
        let f = ramp(i.local.x, e.x, s.curve) * ramp(1.0 - i.local.x, e.y, s.curve)
              * ramp(i.local.y, e.z, s.curve) * ramp(1.0 - i.local.y, e.w, s.curve);
        rgb = rgb * f;
    }
    return vec4<f32>(rgb, a);
}

// Point (s, t) of the slice's input quad (composition uv).
fn in_point(s: SliceP, st: vec2<f32>) -> vec2<f32> {
    let top = mix(s.inq[0].xy, s.inq[0].zw, st.x);
    let bottom = mix(s.inq[1].zw, s.inq[1].xy, st.x);
    return mix(top, bottom, st.y);
}

// The slice's input colour (straight alpha): its centre, or 5 × 5 points averaged.
fn in_sample(s: SliceP, average: bool) -> vec4<f32> {
    var acc = vec4<f32>(0.0);
    var n = 1.0;
    if (!average) {
        acc = textureSampleLevel(comp, samp, in_point(s, vec2<f32>(0.5)), 0.0);
    } else {
        for (var y = 0; y < 5; y++) {
            for (var x = 0; x < 5; x++) {
                let st = (vec2<f32>(f32(x), f32(y)) + 0.5) / 5.0;
                let t = textureSampleLevel(comp, samp, in_point(s, st), 0.0);
                acc += vec4<f32>(t.rgb * t.a, t.a);
            }
        }
        n = 25.0;
        acc = acc / n;
        if (acc.a > 1e-5) {
            acc = vec4<f32>(acc.rgb / acc.a, acc.a);
        }
    }
    return acc;
}

// Slice Chase looks (Output space) over the slice's colour.
fn with_looks(i: VsOut, c0: vec4<f32>) -> vec4<f32> {
    let s = slices[i.slice];
    var c = c0;
    let n = min(u32(s.size.z + 0.5), 4u);
    let asp = max(s.size.x, 1.0) / max(s.size.y, 1.0);
    for (var k = 0u; k < n; k++) {
        let l = s.looks[k];
        var r: vec4<f32>;
        if (l.b.w < 0.5) {
            // Not in the sequence: Outside.
            r = select(vec4<f32>(c.rgb, 0.0), c, l.b.z > 1.5);
        } else {
            let look = u32(l.a.x + 0.5);
            var smp = vec4<f32>(0.0);
            if (look == 4u) {
                smp = in_sample(s, l.p[0].x > 0.5);
            }
            r = chase_look(look, i.local, asp, vec4<f32>(l.a.y, l.a.z, l.a.w, 1.0), l.b.x, l.p, c, smp);
        }
        c = mix(c, r, clamp(l.b.y, 0.0, 1.0));
    }
    return c;
}

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
    let c = with_looks(i, shade(i));
    var a = c.a * slices[i.slice].mult.x;
    if (slices[i.slice].rgb.w > 0.5) {
        let k = textureSampleLevel(keytex, samp, i.pos.xy / screen.size.xy, 0.0);
        a = a * dot(k.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)) * k.a;
    }
    let rgb = adjust(c.rgb, screen.adjust, screen.rgb) * screen.size.z;
    return vec4<f32>(rgb, a);
}

// Key slice: luminance × alpha into the key texture.
@fragment
fn fs_key(i: VsOut) -> @location(0) vec4<f32> {
    let c = shade(i);
    let l = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)) * c.a;
    return vec4<f32>(l, l, l, 1.0);
}

// Output masks (and the stencil-only mask passes).
@fragment
fn fs_black(i: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
