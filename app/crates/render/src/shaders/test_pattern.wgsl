// Test card (Output › Show Test Card): replaces the composition picture so
// every output and slice shows it. Numbered grid (columns 1…, rows A…),
// big + corner circles, diagonals, colour bars, a grey ramp with % labels,
// corner brackets, edge arrows, a 1 px border, a moving bar (stutter check),
// a beat lamp, and two text lines from the app (title, resolution).

struct Frame {
    resolution: vec2<f32>,
    time: f32,
    beat: f32,
    master: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

// Text: lens.x / lens.y = length of line 0 / 1; chars = glyph indices,
// line 0 at 0…31, line 1 at 32…63 (four per vec4).
struct Card {
    lens: vec4<u32>,
    chars: array<vec4<u32>, 16>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<uniform> card: Card;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Full-screen triangle, no vertex buffer.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

// 5x7 font, 7 rows per glyph (bit 4 = left). Glyphs: ' ' 0-9 A-Z % : . - x /
const FONT = array<u32, 301>(
    0u, 0u, 0u, 0u, 0u, 0u, 0u, 14u, 17u, 19u, 21u, 25u, 17u, 14u,
    4u, 12u, 4u, 4u, 4u, 4u, 14u, 14u, 17u, 1u, 2u, 4u, 8u, 31u,
    31u, 2u, 4u, 2u, 1u, 17u, 14u, 2u, 6u, 10u, 18u, 31u, 2u, 2u,
    31u, 16u, 30u, 1u, 1u, 17u, 14u, 6u, 8u, 16u, 30u, 17u, 17u, 14u,
    31u, 1u, 2u, 4u, 8u, 8u, 8u, 14u, 17u, 17u, 14u, 17u, 17u, 14u,
    14u, 17u, 17u, 15u, 1u, 2u, 12u, 14u, 17u, 17u, 31u, 17u, 17u, 17u,
    30u, 17u, 17u, 30u, 17u, 17u, 30u, 14u, 17u, 16u, 16u, 16u, 17u, 14u,
    28u, 18u, 17u, 17u, 17u, 18u, 28u, 31u, 16u, 16u, 30u, 16u, 16u, 31u,
    31u, 16u, 16u, 30u, 16u, 16u, 16u, 14u, 17u, 16u, 23u, 17u, 17u, 15u,
    17u, 17u, 17u, 31u, 17u, 17u, 17u, 14u, 4u, 4u, 4u, 4u, 4u, 14u,
    7u, 2u, 2u, 2u, 2u, 18u, 12u, 17u, 18u, 20u, 24u, 20u, 18u, 17u,
    16u, 16u, 16u, 16u, 16u, 16u, 31u, 17u, 27u, 21u, 21u, 17u, 17u, 17u,
    17u, 17u, 25u, 21u, 19u, 17u, 17u, 14u, 17u, 17u, 17u, 17u, 17u, 14u,
    30u, 17u, 17u, 30u, 16u, 16u, 16u, 14u, 17u, 17u, 17u, 21u, 18u, 13u,
    30u, 17u, 17u, 30u, 20u, 18u, 17u, 15u, 16u, 16u, 14u, 1u, 1u, 30u,
    31u, 4u, 4u, 4u, 4u, 4u, 4u, 17u, 17u, 17u, 17u, 17u, 17u, 14u,
    17u, 17u, 17u, 17u, 17u, 10u, 4u, 17u, 17u, 17u, 21u, 21u, 21u, 10u,
    17u, 17u, 10u, 4u, 10u, 17u, 17u, 17u, 17u, 10u, 4u, 4u, 4u, 4u,
    31u, 1u, 2u, 4u, 8u, 16u, 31u, 24u, 25u, 2u, 4u, 8u, 19u, 3u,
    0u, 4u, 4u, 0u, 4u, 4u, 0u, 0u, 0u, 0u, 0u, 0u, 12u, 12u,
    0u, 0u, 0u, 31u, 0u, 0u, 0u, 0u, 0u, 17u, 10u, 4u, 10u, 17u,
    0u, 1u, 2u, 4u, 8u, 16u, 0u,
);
// ORDER: ' 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ%:.-x/'

const BARS = array<vec3<f32>, 8>(
    vec3<f32>(1.0, 1.0, 1.0),
    vec3<f32>(1.0, 1.0, 0.0),
    vec3<f32>(0.0, 1.0, 1.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(1.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, 0.0),
);

const DIGIT0: u32 = 1u;
const LETTER_A: u32 = 11u;
const PERCENT: u32 = 37u;

fn glyph_on(g: u32, x: i32, y: i32) -> bool {
    if (x < 0 || x > 4 || y < 0 || y > 6 || g > 42u) {
        return false;
    }
    return ((FONT[g * 7u + u32(y)] >> u32(4 - x)) & 1u) == 1u;
}

fn card_char(i: u32) -> u32 {
    let v = card.chars[i / 4u];
    return v[i % 4u];
}

/// Line `line` of the app's text, centred on `cx`, top at `top`.
fn text_line(p: vec2<f32>, line: u32, cx: f32, top: f32, k: f32) -> bool {
    let len = select(card.lens.x, card.lens.y, line == 1u);
    if (len == 0u) {
        return false;
    }
    let adv = 6.0 * k;
    let w = f32(len) * adv - k;
    let l = p - vec2<f32>(floor(cx - w * 0.5), top);
    if (l.x < 0.0 || l.y < 0.0 || l.x >= w || l.y >= 7.0 * k) {
        return false;
    }
    let i = u32(l.x / adv);
    let g = card_char(line * 32u + i);
    return glyph_on(g, i32(floor((l.x - f32(i) * adv) / k)), i32(floor(l.y / k)));
}

/// A number (0…999), optionally with a "%" after it, left/top at `o`.
fn number(p: vec2<f32>, value: u32, pct: bool, o: vec2<f32>, k: f32) -> bool {
    var digits = 1u;
    if (value >= 10u) {
        digits = 2u;
    }
    if (value >= 100u) {
        digits = 3u;
    }
    let n = digits + select(0u, 1u, pct);
    let adv = 6.0 * k;
    let l = p - o;
    if (l.x < 0.0 || l.y < 0.0 || l.x >= f32(n) * adv || l.y >= 7.0 * k) {
        return false;
    }
    let i = u32(l.x / adv);
    var g = PERCENT;
    if (i < digits) {
        var d = value;
        for (var j = 0u; j < digits - 1u - i; j++) {
            d = d / 10u;
        }
        g = DIGIT0 + d % 10u;
    }
    return glyph_on(g, i32(floor((l.x - f32(i) * adv) / k)), i32(floor(l.y / k)));
}

/// Width of `number`'s text in px.
fn number_w(value: u32, pct: bool, k: f32) -> f32 {
    var n = 1u;
    if (value >= 10u) {
        n = 2u;
    }
    if (value >= 100u) {
        n = 3u;
    }
    n += select(0u, 1u, pct);
    return f32(n) * 6.0 * k - k;
}

fn ring(p: vec2<f32>, c: vec2<f32>, r: f32, w: f32) -> bool {
    return abs(length(p - c) - r) < w * 0.5;
}

/// Distance from `p` to the line through `a` and `b`.
fn line_d(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = b - a;
    return abs(d.x * (p.y - a.y) - d.y * (p.x - a.x)) / max(length(d), 1e-3);
}

fn inside(p: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>) -> bool {
    return all(p >= lo) && all(p < hi);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = frame.resolution;
    let p = floor(in.uv * res) + 0.5; // pixel centre
    let ctr = floor(res * 0.5);
    let cell = max(floor(res.y / 12.0), 8.0);
    let ks = max(floor(cell / 36.0), 1.0); // small labels
    let kt = max(floor(cell / 14.0), 1.0); // title
    let kr = max(floor(cell / 22.0), 1.0); // resolution line

    var c = vec3<f32>(0.11);

    // Grid: square cells from the centre out; the centre lines brighter.
    let g = (p - ctr) / cell;
    let gd = abs(fract(g + 0.5) - 0.5) * cell;
    if (gd.x < 0.5 || gd.y < 0.5) {
        c = vec3<f32>(0.42);
    }
    // Cell labels: column numbers along the top / bottom rows, row letters
    // down the left / right columns.
    let ci = floor(g);
    let c_min = floor(-ctr / cell);
    let cell_lo = ctr + ci * cell;
    // The visible part of this cell (edge cells are cut by the screen).
    let vis_lo = max(cell_lo, vec2<f32>(0.0));
    let vis_hi = min(cell_lo + cell, res);
    let first_row = ci.y == c_min.y;
    let last_row = cell_lo.y + cell >= res.y;
    let first_col = ci.x == c_min.x;
    let last_col = cell_lo.x + cell >= res.x;
    let pad = 4.0 * ks;
    // Columns 1… (the left edge column holds the row letters), rows A….
    if ((first_row || last_row) && !first_col && !last_col) {
        let col = u32(ci.x - c_min.x);
        let y = select(vis_lo.y + pad, vis_hi.y - pad - 7.0 * ks, last_row && !first_row);
        if (number(p, col, false, vec2<f32>(vis_lo.x + pad, y), ks)) {
            c = vec3<f32>(0.85);
        }
    }
    if ((first_col || last_col) && !first_row && !last_row) {
        let row = u32(ci.y - c_min.y) - 1u;
        let x = select(vis_lo.x + pad, vis_hi.x - pad - 5.0 * ks, last_col && !first_col);
        if (row < 26u && glyph_on(LETTER_A + row, i32(floor((p.x - x) / ks)), i32(floor((p.y - vis_lo.y - pad) / ks)))) {
            c = vec3<f32>(0.85);
        }
    }

    // Diagonals corner to corner.
    if (line_d(p, vec2<f32>(0.0), res) < 0.5 || line_d(p, vec2<f32>(res.x, 0.0), vec2<f32>(0.0, res.y)) < 0.5) {
        c = vec3<f32>(0.55);
    }
    if (abs(p.x - ctr.x - 0.5) < 0.5 || abs(p.y - ctr.y - 0.5) < 0.5) {
        c = vec3<f32>(0.9);
    }
    // Big circle (fits the height), inner circle, corner circles.
    let big = res.y * 0.5 - cell * 0.25;
    if (ring(p, ctr, big, 3.0)) {
        c = vec3<f32>(1.0);
    }
    if (ring(p, ctr, big * 0.5, 1.0)) {
        c = vec3<f32>(0.8);
    }
    let cr = cell * 1.25;
    let cc = cell * 1.75;
    for (var i = 0u; i < 4u; i++) {
        let corner = vec2<f32>(select(cc, res.x - cc, (i & 1u) == 1u), select(cc, res.y - cc, (i & 2u) == 2u));
        if (ring(p, corner, cr, 2.0)) {
            c = vec3<f32>(1.0);
        }
        if (length(p - corner) < cr && (abs(p.x - corner.x) < 0.5 || abs(p.y - corner.y) < 0.5)) {
            c = vec3<f32>(1.0);
        }
    }

    // Colour bars above the centre, grey ramp below it, a moving bar under that.
    let bw = cell * 8.0;
    let bx0 = ctr.x - bw * 0.5;
    if (inside(p, vec2<f32>(bx0, ctr.y - cell * 2.5), vec2<f32>(bx0 + bw, ctr.y - cell * 0.5))) {
        c = BARS[min(u32((p.x - bx0) / bw * 8.0), 7u)];
    }
    let ry0 = ctr.y + cell * 0.5;
    let ry1 = ctr.y + cell * 2.0;
    if (inside(p, vec2<f32>(bx0, ry0), vec2<f32>(bx0 + bw, ry1))) {
        let s = min(u32((p.x - bx0) / bw * 11.0), 10u);
        let v = f32(s) / 10.0;
        c = vec3<f32>(v);
        let pct = s * 10u;
        let sw = bw / 11.0;
        let o = vec2<f32>(floor(bx0 + (f32(s) + 0.5) * sw - number_w(pct, true, ks) * 0.5), ry1 - 10.0 * ks);
        if (number(p, pct, true, o, ks)) {
            c = select(vec3<f32>(1.0), vec3<f32>(0.0), v >= 0.5);
        }
    }
    let ty0 = ry1 + cell * 0.25;
    let ty1 = ty0 + cell * 0.3;
    if (inside(p, vec2<f32>(bx0, ty0), vec2<f32>(bx0 + bw, ty1))) {
        c = vec3<f32>(0.2);
        let x = bx0 + fract(frame.time * 0.25) * (bw - cell * 0.5);
        if (p.x >= x && p.x < x + cell * 0.5) {
            c = vec3<f32>(1.0);
        }
    }

    // Text: title above the bars, resolution under the moving bar.
    if (text_line(p, 0u, ctr.x, ctr.y - cell * 2.5 - 11.0 * kt, kt)) {
        c = vec3<f32>(1.0);
    }
    if (text_line(p, 1u, ctr.x, ty1 + cell * 0.35, kr)) {
        c = vec3<f32>(1.0);
    }

    // Centre: beat lamp (flashes on each beat) and a fine cross.
    let pulse = exp(-fract(frame.beat) * 6.0);
    let lamp = length(p - ctr);
    if (lamp < cell * 0.18) {
        c = mix(vec3<f32>(0.15), vec3<f32>(1.0, 0.35, 0.1), pulse);
    }
    if (lamp < cell * 0.4 && (abs(p.x - ctr.x - 0.5) < 0.5 || abs(p.y - ctr.y - 0.5) < 0.5)) {
        c = vec3<f32>(1.0);
    }

    // Edge markings: corner brackets, arrows at the edge midpoints, 1 px border.
    let e = min(p, res - p); // distance to the left/top and right/bottom edges
    let bt = max(3.0, ks * 2.0);
    let near_x = p.x < cell || p.x > res.x - cell;
    let near_y = p.y < cell || p.y > res.y - cell;
    if ((e.x < bt && near_y) || (e.y < bt && near_x)) {
        c = vec3<f32>(1.0);
    }
    let ah = cell * 0.45;
    let ax = abs(p.x - ctr.x);
    let ay = abs(p.y - ctr.y);
    if ((e.y < ah && ax < ah - e.y) || (e.x < ah && ay < ah - e.x)) {
        c = vec3<f32>(1.0);
    }
    if (e.x < 1.0 || e.y < 1.0) {
        c = vec3<f32>(1.0, 0.25, 0.25);
    }

    return vec4<f32>(c * frame.master, 1.0);
}
