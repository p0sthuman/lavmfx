// Draws one clip's video frame into a cleared layer texture, letterboxed
// ("fit"), premultiplied alpha. Compositing happens in blend.wgsl.
// Modes: 0 RGBA, 1 planar YUV (t0=Y t1=U t2=V), 2 NV12 (t0=Y t1=UV),
//        3 HAP Q scaled YCoCg in BC3 (t0), 4 planar YCgCo (t0=Y t1=Cg t2=Co, DXV3 HQ).

struct Video {
    // rv, gu, gv, bu: YUV → RGB coefficients
    coeffs: vec4<f32>,
    // y offset, y scale, chroma scale, unused
    range: vec4<f32>,
    // fit scale (xy), uv scale for BC padding (zw)
    scale: vec4<f32>,
    mode: u32,
    // clip alpha (premultiplied into the output)
    opacity: f32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> v: Video;
@group(0) @binding(1) var t0: texture_2d<f32>;
@group(0) @binding(2) var t1: texture_2d<f32>;
@group(0) @binding(3) var t2: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn yuv_to_rgb(y: f32, u: f32, w: f32) -> vec3<f32> {
    let yy = (y - v.range.x) * v.range.y;
    let cb = (u - 0.5) * v.range.z;
    let cr = (w - 0.5) * v.range.z;
    return vec3<f32>(
        yy + v.coeffs.x * cr,
        yy - v.coeffs.y * cb - v.coeffs.z * cr,
        yy + v.coeffs.w * cb,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = (in.uv - vec2<f32>(0.5)) / v.scale.xy + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return vec4<f32>(0.0); // letterbox: transparent
    }
    let tuv = uv * v.scale.zw;

    var rgb: vec3<f32>;
    switch v.mode {
        case 1u: {
            rgb = yuv_to_rgb(textureSample(t0, samp, tuv).r, textureSample(t1, samp, tuv).r, textureSample(t2, samp, tuv).r);
        }
        case 2u: {
            let c = textureSample(t1, samp, tuv).rg;
            rgb = yuv_to_rgb(textureSample(t0, samp, tuv).r, c.x, c.y);
        }
        case 3u: {
            // Hap Q: scaled YCoCg (HAP spec, "Hap Q" pixel format).
            let c = textureSample(t0, samp, tuv);
            let s = c.z * (255.0 / 8.0) + 1.0;
            let co = (c.x - 128.0 / 255.0) / s;
            let cg = (c.y - 128.0 / 255.0) / s;
            rgb = vec3<f32>(c.w + co - cg, c.w + cg, c.w - co - cg);
        }
        case 4u: {
            let y = textureSample(t0, samp, tuv).r;
            let cg = textureSample(t1, samp, tuv).r - 128.0 / 255.0;
            let co = textureSample(t2, samp, tuv).r - 128.0 / 255.0;
            let t = y - cg;
            rgb = vec3<f32>(t + co, y + cg, t - co);
        }
        default: {
            rgb = textureSample(t0, samp, tuv).rgb;
        }
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * v.opacity, v.opacity);
}
