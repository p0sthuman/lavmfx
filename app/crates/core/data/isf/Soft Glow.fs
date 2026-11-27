/*{
    "DESCRIPTION": "Bright parts bloom: downscaled two-pass blur added back",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Blur", "Glow"],
    "INPUTS": [
        { "NAME": "inputImage", "TYPE": "image" },
        { "NAME": "threshold", "LABEL": "Threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 },
        { "NAME": "radius", "LABEL": "Radius", "TYPE": "float", "MIN": 0.5, "MAX": 8.0, "DEFAULT": 2.0 },
        { "NAME": "intensity", "LABEL": "Intensity", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.0 }
    ],
    "PASSES": [
        { "TARGET": "bright", "WIDTH": "floor($WIDTH/4.0)", "HEIGHT": "floor($HEIGHT/4.0)" },
        { "TARGET": "blurH", "WIDTH": "floor($WIDTH/4.0)", "HEIGHT": "floor($HEIGHT/4.0)" },
        { "TARGET": "blurV", "WIDTH": "floor($WIDTH/4.0)", "HEIGHT": "floor($HEIGHT/4.0)" },
        {}
    ]
}*/

vec4 blur(vec2 dir) {
    vec2 stp = dir * radius / RENDERSIZE;
    vec4 acc = vec4(0.0);
    float wsum = 0.0;
    for (int i = -6; i <= 6; i++) {
        float w = exp(-float(i * i) / 18.0);
        vec2 uv = isf_FragNormCoord + stp * float(i);
        acc += (PASSINDEX == 1 ? IMG_NORM_PIXEL(bright, uv) : IMG_NORM_PIXEL(blurH, uv)) * w;
        wsum += w;
    }
    return acc / wsum;
}

void main() {
    if (PASSINDEX == 0) {
        vec4 c = IMG_THIS_NORM_PIXEL(inputImage);
        float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
        gl_FragColor = vec4(c.rgb * smoothstep(threshold, threshold + 0.1, l), 1.0);
    } else if (PASSINDEX == 1) {
        gl_FragColor = blur(vec2(1.0, 0.0));
    } else if (PASSINDEX == 2) {
        gl_FragColor = blur(vec2(0.0, 1.0));
    } else {
        vec4 c = IMG_THIS_NORM_PIXEL(inputImage);
        vec3 g = IMG_THIS_NORM_PIXEL(blurV).rgb * intensity;
        gl_FragColor = vec4(c.rgb + g, c.a);
    }
}
