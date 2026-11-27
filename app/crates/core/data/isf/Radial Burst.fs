/*{
    "DESCRIPTION": "Rays from a movable centre",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        { "NAME": "center", "LABEL": "Center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5], "MIN": [0.0, 0.0], "MAX": [1.0, 1.0] },
        { "NAME": "rays", "LABEL": "Rays", "TYPE": "long", "VALUES": [4, 8, 16, 32], "LABELS": ["4", "8", "16", "32"], "DEFAULT": 16 },
        { "NAME": "spin", "LABEL": "Spin", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.25 },
        { "NAME": "softness", "LABEL": "Softness", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2 },
        { "NAME": "color", "LABEL": "Color", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] }
    ]
}*/

const float TAU = 6.2831853;

void main() {
    vec2 d = (isf_FragNormCoord - center) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0);
    float a = atan(d.y, d.x) / TAU + TIME * spin;
    float w = fract(a * float(rays));
    // 0 at a ray's centre line, 1 halfway to the next ray.
    float band = abs(w - 0.5) * 2.0;
    float k = 1.0 - smoothstep(0.5 - softness * 0.5, 0.5 + softness * 0.5 + 0.001, band);
    float fade = 1.0 - smoothstep(0.0, 1.2, length(d));
    gl_FragColor = vec4(color.rgb * k * fade, color.a);
}
