/*{
    "DESCRIPTION": "Flowing plasma between two colours",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        { "NAME": "speed", "LABEL": "Speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.5 },
        { "NAME": "scale", "LABEL": "Scale", "TYPE": "float", "MIN": 0.5, "MAX": 20.0, "DEFAULT": 4.0 },
        { "NAME": "colorA", "LABEL": "Color A", "TYPE": "color", "DEFAULT": [0.05, 0.0, 0.35, 1.0] },
        { "NAME": "colorB", "LABEL": "Color B", "TYPE": "color", "DEFAULT": [1.0, 0.35, 0.1, 1.0] },
        { "NAME": "bands", "LABEL": "Bands", "TYPE": "bool", "DEFAULT": false }
    ]
}*/

void main() {
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0) * scale;
    float t = TIME * speed;
    float v = sin(p.x + t) + sin(p.y * 1.3 - t * 0.7) + sin(length(p + vec2(sin(t * 0.3), cos(t * 0.4))) * 1.7 + t);
    v = 0.5 + 0.5 * sin(v * 1.5);
    if (bands) {
        v = floor(v * 6.0) / 5.0;
    }
    gl_FragColor = mix(colorA, colorB, v);
}
