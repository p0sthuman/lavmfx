/*{
    "DESCRIPTION": "Spectrum bars from the External FFT input",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator", "Audio"],
    "INPUTS": [
        { "NAME": "spectrum", "TYPE": "audioFFT" },
        { "NAME": "bars", "LABEL": "Bars", "TYPE": "float", "MIN": 4.0, "MAX": 32.0, "DEFAULT": 16.0 },
        { "NAME": "gap", "LABEL": "Gap", "TYPE": "float", "MIN": 0.0, "MAX": 0.8, "DEFAULT": 0.2 },
        { "NAME": "gain", "LABEL": "Gain", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.0 },
        { "NAME": "low", "LABEL": "Low Color", "TYPE": "color", "DEFAULT": [0.0, 0.9, 0.6, 1.0] },
        { "NAME": "high", "LABEL": "High Color", "TYPE": "color", "DEFAULT": [1.0, 0.2, 0.5, 1.0] },
        { "NAME": "mirror", "LABEL": "Mirror", "TYPE": "bool", "DEFAULT": false }
    ]
}*/

void main() {
    vec2 uv = isf_FragNormCoord;
    float n = floor(bars);
    float i = floor(uv.x * n);
    float level = IMG_NORM_PIXEL(spectrum, vec2((i + 0.5) / n, 0.5)).r * gain;
    float y = mirror ? abs(uv.y - 0.5) * 2.0 : uv.y;
    float inBar = step(gap * 0.5, fract(uv.x * n)) * step(fract(uv.x * n), 1.0 - gap * 0.5);
    float lit = inBar * step(y, level);
    vec4 c = mix(low, high, y);
    gl_FragColor = vec4(c.rgb * lit, lit * c.a);
}
