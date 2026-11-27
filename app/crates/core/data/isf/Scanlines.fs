/*{
    "DESCRIPTION": "CRT-style scanlines with a rolling bar",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Stylize"],
    "INPUTS": [
        { "NAME": "inputImage", "TYPE": "image" },
        { "NAME": "lines", "LABEL": "Lines", "TYPE": "float", "MIN": 50.0, "MAX": 1080.0, "DEFAULT": 360.0 },
        { "NAME": "darkness", "LABEL": "Darkness", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
        { "NAME": "roll", "LABEL": "Roll Speed", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.2 },
        { "NAME": "glitch", "LABEL": "Glitch", "TYPE": "event" }
    ]
}*/

float hash(float n) { return fract(sin(n) * 43758.5453); }

void main() {
    vec2 uv = isf_FragNormCoord;
    if (glitch) {
        float row = floor(uv.y * 24.0);
        uv.x += (hash(row + floor(TIME * 20.0)) - 0.5) * 0.08;
    }
    vec4 c = IMG_NORM_PIXEL(inputImage, uv);
    float s = 0.5 + 0.5 * sin(uv.y * lines * 3.14159);
    float bar = smoothstep(0.0, 0.1, abs(fract(uv.y + TIME * roll) - 0.5));
    float k = 1.0 - darkness * (1.0 - s) - 0.15 * darkness * (1.0 - bar);
    gl_FragColor = vec4(c.rgb * k, c.a);
}
