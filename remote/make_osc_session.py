#!/usr/bin/env python3
"""Build lavmfx-control.json, the Open Stage Control session for our app (v2).

Usage: python3 make_osc_session.py [rows] [columns]   (grid window, default 4 x 8)

Tabs: Clips (paged grid), Mixer (the grid's layers), Dashboards, Clip FX,
Layer FX, Comp FX. The composition OSC map plus the app's /lavmfx/... tablet
addresses (see lavmfx/src/app/tablet.rs); the app feeds back names, playing
state, values and the effect lists (Preferences > OSC > Output, port 7001).
"""
import json, sys

ROWS = int(sys.argv[1]) if len(sys.argv) > 1 else 4
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 8
MIXER = 8          # layers on the Mixer tab
FX_COLS = 6        # faders per row on the FX tabs
ACCENT, DIM, EMPTY, RED = "#62e2ba", "#4a4a4a", "#262626", "#c0504d"
T = "/composition/tempocontroller/tempo"
BPM = f"(OSC{{{T}, 0.2083}}*480+20)"  # tempo 20..500 BPM as 0..1


def w(kind, **p):
    d = {"type": kind}
    d.update(p)
    return d


def panel(layout, widgets, **p):
    d = {"layout": layout, "widgets": widgets, "padding": 2, "innerPadding": False}
    d.update(p)
    return w("panel", **d)


def tap(label, address, **p):
    d = {"mode": "tap", "on": 1, "off": 0}
    d.update(p)
    return w("button", label=label, address=address, **d)


def toggle(label, address, color, **p):
    return w("button", label=label, address=address, mode="toggle", on=1, off=0, colorWidget=color, **p)


def text(value=None, address=None, **p):
    d = {"label": False}
    if value is not None:
        d["value"] = value
    if address:
        d["address"] = address
    d.update(p)
    return w("text", **d)


def fader(address, label=False, **p):
    d = {"range": {"min": 0, "max": 1}, "colorWidget": ACCENT, "doubleTap": True}
    d.update(p)
    return w("fader", label=label, address=address, **d)


def topbar():
    return panel("horizontal", [
        tap("TAP", "/composition/tempocontroller/tempotap", width=110, colorWidget=ACCENT),
        text(f"#{{Math.round({BPM}*10)/10}}", width=90, css="font-size:150%"),
        tap("−1", T, width=60, on=f"#{{(Math.round({BPM})-1-20)/480}}"),
        tap("+1", T, width=60, on=f"#{{(Math.round({BPM})+1-20)/480}}"),
        tap("RESYNC", "/composition/tempocontroller/resync", width=110),
        tap("◀ COL", "/composition/connectprevcolumn", width=90),
        tap("COL ▶", "/composition/connectnextcolumn", width=90),
        tap("CLEAR ALL", "/composition/disconnectall", width=120, colorWidget=RED),
        fader("/composition/master", label="MASTER", horizontal=True, expand=True),
    ], height=70)


def scroll(label, dx, dy):
    return tap(label, "/lavmfx/grid/scroll", preArgs=[dx, dy], expand=True)


# ------------------------------------------------------------------ Clips

def clip(r, c):
    a = f"/lavmfx/grid/row/{r}/clip/{c}"
    state = f"OSC{{{a}/connected, 0}}"
    return tap(f"#{{OSC{{{a}/name}} || ''}}", f"{a}/connect", expand=True, wrap=True,
               colorWidget=f"#{{{state} == 3 ? '{ACCENT}' : {state} == 0 ? '{EMPTY}' : '{DIM}'}}",
               alphaFillOff=f"#{{{state} == 3 ? 0.85 : 0.15}}",
               colorText=f"#{{{state} == 3 ? '#000000' : 'auto'}}")


def grid_row(r):
    a = f"/lavmfx/grid/row/{r}"
    head = panel("horizontal", [
        panel("vertical", [
            text(address=f"{a}/name", value="", expand=True),
            panel("horizontal", [
                toggle("B", f"{a}/bypassed", RED, expand=True),
                toggle("S", f"{a}/solo", ACCENT, expand=True),
                tap("X", f"{a}/clear", expand=True),
            ], expand=True),
        ], expand=True),
        fader(f"{a}/master", width=34),
    ], width=170)
    return panel("horizontal", [head] + [clip(r, c) for c in range(1, COLS + 1)], expand=True)


def clips_tab():
    nav = panel("vertical", [
        panel("horizontal", [scroll("◀", -COLS, 0), scroll("▶", COLS, 0), scroll("▲", 0, ROWS), scroll("▼", 0, -ROWS)],
              expand=True),
        text(address="/lavmfx/grid/pos", value="", height=18, css="font-size:75%"),
    ], width=170)
    cols = [tap(f"#{{OSC{{/lavmfx/grid/col/{c}/name}} || '{c}'}}", f"/lavmfx/grid/col/{c}/connect", expand=True)
            for c in range(1, COLS + 1)]
    grid = panel("vertical", [grid_row(r) for r in range(ROWS, 0, -1)], expand=True)
    return w("tab", id="tab_clips", label="Clips", layout="vertical",
             widgets=[topbar(), panel("horizontal", [nav] + cols, height=50), grid])


# ------------------------------------------------------------------ Mixer

def mixer_strip(r):
    a = f"/lavmfx/grid/row/{r}"
    faders = panel("horizontal", [
        panel("vertical", [text("M"), fader(f"{a}/master", expand=True, colorWidget="#9a9a9a")], expand=True),
        panel("vertical", [text("V"), fader(f"{a}/video/opacity", expand=True)], expand=True),
        panel("vertical", [text("A"), fader(f"{a}/audio/volume", expand=True, colorWidget="#d66296")], expand=True),
    ], expand=True)
    return panel("vertical", [
        text(address=f"{a}/name", value="", height=40),
        faders,
        panel("horizontal", [toggle("B", f"{a}/bypassed", RED, expand=True),
                             toggle("S", f"{a}/solo", ACCENT, expand=True)], height=50),
    ], expand=True)


def mixer_tab():
    comp = panel("horizontal", [
        fader("/composition/crossfader/phase", label="CROSSFADER", horizontal=True, expand=True, origin=0.5),
        fader("/composition/speed", label="SPEED", horizontal=True, width=300),
    ], height=70)
    strips = panel("horizontal", [mixer_strip(r) for r in range(1, MIXER + 1)], expand=True)
    return w("tab", id="tab_mixer", label="Mixer", layout="vertical", widgets=[topbar(), strips, comp])


# ------------------------------------------------------------- Dashboards

def selector(kind):
    return [tap("◀", f"/lavmfx/select/{kind}", preArgs=[-1], width=50),
            tap("▶", f"/lavmfx/select/{kind}", preArgs=[1], width=50)]


def dash_row(title, owner, picker=None):
    head = [text(title, width=120)]
    if picker:
        head += selector(picker) + [text(address=f"/lavmfx/selected/{picker}", value="—", width=200)]
    knobs = [w("knob", label=f"Link {i}", address=f"/composition{owner}/dashboard/link{i}",
               range={"min": 0, "max": 1}, colorWidget=ACCENT, expand=True, doubleTap=True)
             for i in range(1, 9)]
    return panel("vertical", [panel("horizontal", head, height=50), panel("horizontal", knobs, expand=True)],
                 expand=True)


def dash_tab():
    return w("tab", id="tab_dash", label="Dashboards", layout="vertical", widgets=[
        topbar(),
        dash_row("Composition", ""),
        dash_row("Layer", "/selectedlayer", "layer"),
        dash_row("Clip", "/selectedclip", "clip"),
    ])


# -------------------------------------------------------------------- FX

def fx_tab(label, scope, picker):
    src = f"/lavmfx/fx/{scope}"
    # O-S-C hands JSON strings over already parsed; accept both.
    lst = lambda a: f"((v) => typeof v === 'string' ? JSON.parse(v || '[]') : (v || []))(OSC{{{a}}})"
    ranges, toggles = lst(f"{src}/ranges"), lst(f"{src}/toggles")
    head = [text(label, width=120)]
    if picker:
        head += selector(picker) + [text(address=f"/lavmfx/selected/{picker}", value="—", expand=True)]
    # Each cell: the param's name over its fader (faders don't show labels here).
    sliders = w("matrix", widgetType="panel", expand=True, layout="grid", gridTemplate=FX_COLS,
                quantity=f"#{{{ranges}.length}}",
                props=("JS{ const p = " + ranges + "[$] || {}; return {layout: 'vertical', padding: 2, "
                       "innerPadding: false, widgets: ["
                       "{type: 'text', label: false, value: p.l || '', height: 34, css: 'font-size:75%'}, "
                       "{type: 'fader', label: false, address: p.a || '/lavmfx/none', range: {min: 0, max: 1}, "
                       f"expand: true, doubleTap: true, colorWidget: '{ACCENT}'}}]}} }}"))
    buttons = w("matrix", widgetType="button", height=60, layout="horizontal",
                quantity=f"#{{{toggles}.length}}",
                props=("JS{ const p = " + toggles + "[$] || {}; return {label: p.l || '', address: p.a || '/lavmfx/none', "
                       f"mode: 'toggle', on: 1, off: 0, colorWidget: '{ACCENT}', css: 'font-size:80%'}} }}"))
    return w("tab", id=f"tab_fx_{scope}", label=label, layout="vertical",
             widgets=[topbar(), panel("horizontal", head, height=50), buttons, sliders])


session = {
    "createdWith": "Open Stage Control",
    "version": "1.31.2",
    "type": "session",
    "content": w("root", colorBg="#111111", tabs=[
        clips_tab(), mixer_tab(), dash_tab(),
        fx_tab("Clip FX", "clip", "clip"), fx_tab("Layer FX", "layer", "layer"), fx_tab("Comp FX", "comp", None),
    ]),
}
with open("lavmfx-control.json", "w") as f:
    json.dump(session, f, indent=1, ensure_ascii=False)
print(f"lavmfx-control.json: grid {ROWS} x {COLS}, mixer {MIXER} layers")
