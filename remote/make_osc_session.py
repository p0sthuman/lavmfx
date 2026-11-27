#!/usr/bin/env python3
"""Build lavmfx-control.json, the Open Stage Control session for our app (v5).

Usage: python3 make_osc_session.py [rows] [columns]   (grid window, default 4 x 8)

Tabs: Clips (paged grid), Mixer (layer + group strips over the four
dashboards, 2 x 2) and Chase (one Slice Chase effect: sequence, speed,
direction, attack / trail, Next / Previous / Blast / Reset;
the first chase effect that isn't bypassed unless one is picked). Names are written on the faders:
no rows for labels that can't be tapped. The composition OSC map plus the app's /lavmfx/... tablet
addresses (see lavmfx/src/app/tablet.rs); the app feeds back names, selection,
playing state and values (Preferences > OSC > Output, port 7001). The grid
window follows the app's clip grid when that scrolls, and paging here scrolls
the app.

Layers get one wide V (opacity) fader; the Mixer's "M V A" switch shows
master and audio volume next to it. Tap a layer / group name to select it.
Effect params go through the dashboards (8 wide faders each), not a small
fader per param. BLACKOUT = /composition/bypassed (black picture, sound on).
"""
import json, sys

ROWS = int(sys.argv[1]) if len(sys.argv) > 1 else 4
COLS = int(sys.argv[2]) if len(sys.argv) > 2 else 8
MIXER = 8          # layers on the Mixer tab
GROUPS = 4         # layer group strips on the Mixer tab (shown when the group exists)
MVA = "mixer_mva"  # id of the Mixer's M V A switch (off = V only)
ACCENT, DIM, EMPTY, RED = "#62e2ba", "#4a4a4a", "#262626", "#c0504d"
GROUP = "#c88c3c"  # group strips (the app's first group colour)
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


# Text written on top of a fader (no row of its own; touches go through it).
OVERLAY_CSS = (":host{position:relative} .html{position:absolute;left:6px;right:6px;top:4px;z-index:2;"
               "pointer-events:none;font-size:75%;line-height:1.25;opacity:.9;white-space:nowrap;overflow:hidden;"
               "text-overflow:ellipsis;text-align:center}")
# Dashboard titles: may wrap (the picked name on its own line).
TITLE_CSS = OVERLAY_CSS.replace("white-space:nowrap", "white-space:normal;overflow-wrap:anywhere").replace(
    "text-align:center", "text-align:left")


def fader(address, label=False, overlay=None, **p):
    """Compact fader (the whole fader is the touch area, no thin track);
    `overlay` = text written on top of it."""
    d = {"range": {"min": 0, "max": 1}, "colorWidget": ACCENT, "doubleTap": True, "design": "compact"}
    if overlay:
        d.update(html=f"<div>{overlay}</div>", css=OVERLAY_CSS)
    d.update(p)
    return w("fader", label=label, address=address, **d)


def hfader(label, address, width=None, **p):
    """Horizontal fader with its name written on it."""
    size = {"width": width} if width else {"expand": True}
    return fader(address, horizontal=True, overlay=label, **size, **p)


def name_button(base, select, on_color, **p):
    """A layer / group name: tap = select it in the app; lit while selected."""
    sel = f"OSC{{{base}/selected, 0}} == 1"
    return tap(f"#{{OSC{{{base}/name}} || ''}}", select,
               colorWidget=f"#{{{sel} ? '{on_color}' : '{DIM}'}}",
               alphaFillOff=f"#{{{sel} ? 0.85 : 0.15}}",
               colorText=f"#{{{sel} ? '#000000' : 'auto'}}",
               css="font-size:80%", **p)


def topbar():
    return panel("horizontal", [
        tap("TAP", "/composition/tempocontroller/tempotap", width=100, colorWidget=ACCENT),
        text(f"#{{Math.round({BPM}*10)/10}}", width=80, css="font-size:130%"),
        tap("−1", T, width=54, on=f"#{{(Math.round({BPM})-1-20)/480}}"),
        tap("+1", T, width=54, on=f"#{{(Math.round({BPM})+1-20)/480}}"),
        tap("/2", T, width=54, on=f"#{{Math.max(0, ({BPM}/2-20)/480)}}"),
        tap("×2", T, width=54, on=f"#{{Math.min(1, ({BPM}*2-20)/480)}}"),
        tap("RESYNC", "/composition/tempocontroller/resync", width=100),
        tap("◀ COL", "/composition/connectprevcolumn", width=84),
        tap("COL ▶", "/composition/connectnextcolumn", width=84),
        toggle("BLACKOUT", "/composition/bypassed", RED, width=120),
        hfader("MASTER", "/composition/master"),
    ], height=44)


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
            name_button(a, f"{a}/select", ACCENT, expand=True),
            panel("horizontal", [
                toggle("B", f"{a}/bypassed", RED, expand=True),
                toggle("S", f"{a}/solo", ACCENT, expand=True),
                tap("X", f"{a}/clear", expand=True),
            ], expand=True),
        ], expand=True),
        fader(f"{a}/video/opacity", width=64),
    ], width=210)
    return panel("horizontal", [head] + [clip(r, c) for c in range(1, COLS + 1)], expand=True)


def clips_tab():
    nav = panel("horizontal", [scroll("◀", -COLS, 0), scroll("▶", COLS, 0), scroll("▲", 0, ROWS),
                               scroll("▼", 0, -ROWS)], width=210)
    cols = [tap(f"#{{OSC{{/lavmfx/grid/col/{c}/name}} || '{c}'}}", f"/lavmfx/grid/col/{c}/connect", expand=True)
            for c in range(1, COLS + 1)]
    grid = panel("vertical", [grid_row(r) for r in range(ROWS, 0, -1)], expand=True)
    return w("tab", id="tab_clips", label="Clips", layout="vertical",
             widgets=[topbar(), panel("horizontal", [nav] + cols, height=50), grid])


# ------------------------------------------- Mixer (layers, groups, dashboards)

def layer_strip(r):
    a = f"/lavmfx/grid/row/{r}"
    mva = f"#{{@{{{MVA}}} == 1}}"
    faders = panel("horizontal", [
        fader(f"{a}/master", expand=True, colorWidget="#9a9a9a", visible=mva),
        fader(f"{a}/video/opacity", expand=True),
        fader(f"{a}/audio/volume", expand=True, colorWidget="#d66296", visible=mva),
    ], expand=True)
    return panel("vertical", [
        name_button(a, f"{a}/select", ACCENT, height=30),
        faders,
        panel("horizontal", [toggle("B", f"{a}/bypassed", RED, expand=True),
                             toggle("S", f"{a}/solo", ACCENT, expand=True)], height=34),
    ], expand=True)


def group_strip(g):
    a = f"/lavmfx/group/{g}"
    return panel("vertical", [
        name_button(a, f"{a}/select", GROUP, height=30),
        fader(f"{a}/master", expand=True, colorWidget=GROUP),
        panel("horizontal", [toggle("B", f"{a}/bypassed", RED, expand=True),
                             toggle("S", f"{a}/solo", ACCENT, expand=True)], height=34),
    ], expand=True, visible=f"#{{(OSC{{{a}/name}} || '') != ''}}")


def selector(kind):
    return [tap("▲", f"/lavmfx/select/{kind}", preArgs=[1], expand=True),
            tap("▼", f"/lavmfx/select/{kind}", preArgs=[-1], expand=True)]


def dash_block(title, owner, picker=None):
    """8 dashboard faders; the title (with the picked layer / group / clip) is
    written on the first one, the picker is a narrow ▲ ▼ column."""
    name = f"{title}<br>#{{OSC{{/lavmfx/selected/{picker}}} || '—'}}" if picker else title
    faders = [fader(f"{owner}/link{i}", expand=True, **({"overlay": name, "css": TITLE_CSS} if i == 1 else {}))
              for i in range(1, 9)]
    left = [panel("vertical", selector(picker), width=44)] if picker else []
    return panel("horizontal", left + faders, expand=True)


def mixer_tab():
    strips = panel("horizontal", [layer_strip(r) for r in range(1, MIXER + 1)]
                   + [group_strip(g) for g in range(1, GROUPS + 1)], expand=5)
    # Dashboards 2 x 2: Composition | Layer over Group | Clip.
    dash = panel("vertical", [
        panel("horizontal", [dash_block("Composition", "/composition/dashboard"),
                             dash_block("Layer", "/composition/selectedlayer/dashboard", "layer")], expand=True),
        panel("horizontal", [dash_block("Group", "/lavmfx/dash/group", "group"),
                             dash_block("Clip", "/composition/selectedclip/dashboard", "clip")], expand=True),
    ], expand=5)
    comp = panel("horizontal", [
        hfader("CROSSFADER", "/composition/crossfader/phase", origin=0.5),
        hfader("SPEED", "/composition/speed", width=360),
        w("button", id=MVA, label="M V A", mode="toggle", on=1, off=0, value=0, width=100,
          colorWidget=ACCENT, address="/lavmfx/ui/mixer_mva", bypass=True),
    ], height=44)
    return w("tab", id="tab_mixer", label="Mixer", layout="vertical", widgets=[topbar(), strips, dash, comp])


# ------------------------------------------------------------------ Chase

C = "/lavmfx/chase"


def choices(key, labels, **p):
    """One button per option of a chase choice; the current one lit."""
    # `false`: listen whatever the preArgs (OSC{} matches the widget's own by default).
    cur = f"OSC{{{C}/{key}, -1, false}}"
    return [tap(lab, f"{C}/{key}/set", preArgs=[i], expand=True,
                colorWidget=f"#{{{cur} == {i} ? '{ACCENT}' : '{DIM}'}}",
                alphaFillOff=f"#{{{cur} == {i} ? 0.85 : 0.15}}",
                colorText=f"#{{{cur} == {i} ? '#000000' : 'auto'}}", **p)
            for i, lab in enumerate(labels)]


def picker(address, label_address, **p):
    """◀ name ▶ — the arrows step, the name shows what is picked."""
    return panel("horizontal", [
        tap("◀", address, preArgs=[-1], width=90),
        tap(f"#{{OSC{{{label_address}, '—', false}}}}", address, preArgs=[1], expand=True, css="font-size:90%"),
        tap("▶", address, preArgs=[1], width=90),
    ], **p)


def chase_tab():
    adv = f"OSC{{{C}/advance, -1}}"
    rows = [
        # Which chase effect (tap the name: next one), its bypass.
        panel("horizontal", [picker(f"{C}/pick", f"{C}/name", expand=True),
                             toggle("BYPASS", f"{C}/bypassed", RED, width=120)], height=56),
        # Sequence (tap the name: next one).
        picker(f"{C}/sequence/step", f"{C}/sequence/name", height=56),
        panel("horizontal", choices("advance", ["BPM", "Time", "Position", "FFT", "Manual"]), height=56),
        # Speed: step length in beats (BPM) or step time (Time).
        panel("horizontal", choices("steplength", ["1/16", "1/8", "1/4", "1/2", "1 beat", "2", "4", "8", "16"]),
              height=56, visible=f"#{{{adv} == 0}}"),
        panel("horizontal", [hfader(f"STEP TIME #{{OSC{{{C}/steptime/text}} || ''}}", f"{C}/steptime")],
              height=56, visible=f"#{{{adv} == 1}}"),
        panel("horizontal", choices("direction", ["→ Forward", "← Backward", "↔ Ping-Pong", "Random step"])
              + choices("pattern", ["Sequence", "Random slices"]), height=56),
        panel("horizontal", [hfader(f"ATTACK #{{OSC{{{C}/attack/text}} || ''}}", f"{C}/attack"),
                             hfader(f"TRAIL #{{OSC{{{C}/trail/text}} || ''}}", f"{C}/trail"),
                             hfader("OPACITY", f"{C}/opacity")], height=56),
        panel("horizontal", [tap("◀ PREVIOUS", f"{C}/previous", expand=True),
                             tap("NEXT ▶", f"{C}/next", expand=True, colorWidget=ACCENT),
                             tap("BLAST", f"{C}/blast", expand=True, colorWidget="#e0c060"),
                             tap("RESET", f"{C}/reset", expand=True)], expand=True),
    ]
    return w("tab", id="tab_chase", label="Chase", layout="vertical", widgets=[topbar()] + rows)


session = {
    "createdWith": "Open Stage Control",
    "version": "1.31.2",
    "type": "session",
    "content": w("root", colorBg="#111111", tabs=[clips_tab(), mixer_tab(), chase_tab()]),
}
with open("lavmfx-control.json", "w") as f:
    json.dump(session, f, indent=1, ensure_ascii=False)
print(f"lavmfx-control.json: grid {ROWS} x {COLS}, mixer {MIXER} layers + {GROUPS} groups, chase tab")
