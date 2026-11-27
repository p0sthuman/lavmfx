/*{
    "DESCRIPTION": "Zooming, turning echoes of the image (persistent buffer)",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Feedback"],
    "INPUTS": [
        { "NAME": "inputImage", "TYPE": "image" },
        { "NAME": "zoom", "LABEL": "Zoom", "TYPE": "float", "MIN": 0.9, "MAX": 1.1, "DEFAULT": 1.02 },
        { "NAME": "rotate", "LABEL": "Rotate", "TYPE": "float", "MIN": -5.0, "MAX": 5.0, "DEFAULT": 0.5 },
        { "NAME": "decay", "LABEL": "Decay", "TYPE": "float", "MIN": 0.0, "MAX": 0.99, "DEFAULT": 0.9 },
        { "NAME": "hueDrift", "LABEL": "Hue Drift", "TYPE": "bool", "DEFAULT": true }
    ],
    "PASSES": [
        { "TARGET": "trail", "PERSISTENT": true },
        {}
    ]
}*/

vec2 echo(vec2 uv) {
    float a = radians(rotate);
    vec2 p = (uv - 0.5) / zoom;
    p = mat2(cos(a), sin(a), -sin(a), cos(a)) * p;
    return p + 0.5;
}

void main() {
    vec4 src = IMG_THIS_NORM_PIXEL(inputImage);
    if (PASSINDEX == 0) {
        vec4 old = IMG_NORM_PIXEL(trail, echo(isf_FragNormCoord)) * decay;
        if (hueDrift) {
            old.rgb = old.gbr * 0.1 + old.rgb * 0.9;
        }
        gl_FragColor = max(src, old);
    } else {
        gl_FragColor = IMG_THIS_NORM_PIXEL(trail);
    }
}
