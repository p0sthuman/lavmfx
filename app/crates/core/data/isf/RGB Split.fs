/*{
    "DESCRIPTION": "Shifts the red and blue channels apart",
    "CREDIT": "LAVMFX (built-in example)",
    "ISFVSN": "2",
    "CATEGORIES": ["Color", "Distortion"],
    "INPUTS": [
        { "NAME": "inputImage", "TYPE": "image" },
        { "NAME": "amount", "LABEL": "Amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.1, "DEFAULT": 0.01 },
        { "NAME": "angle", "LABEL": "Angle", "TYPE": "float", "MIN": 0.0, "MAX": 360.0, "DEFAULT": 0.0 },
        { "NAME": "wobble", "LABEL": "Wobble", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
    ]
}*/

void main() {
    float a = radians(angle) + sin(TIME * 3.0) * wobble * 3.14159;
    vec2 off = vec2(cos(a), sin(a)) * amount;
    vec4 c = IMG_THIS_NORM_PIXEL(inputImage);
    float r = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + off).r;
    float b = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord - off).b;
    gl_FragColor = vec4(r, c.g, b, c.a);
}
