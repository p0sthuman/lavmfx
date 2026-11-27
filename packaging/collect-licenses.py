#!/usr/bin/env python3
"""Write the licences of every Rust crate compiled into the lavmfx binary.

Usage: collect-licenses.py <out-file> <target-triple>

Walks the normal (not dev / build) dependencies of the `lavmfx` package for
the given target with `cargo metadata`, and writes each crate's name,
version, SPDX licence and its licence / notice files (from the local cargo
source cache). Identical licence texts are printed once.
"""
import hashlib
import json
import os
import subprocess
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
out_path = sys.argv[1]
cmd = ["cargo", "metadata", "--format-version", "1", "--locked",
       "--manifest-path", os.path.join(root, "app", "Cargo.toml")]
cmd += ["--filter-platform", sys.argv[2]]
meta = json.loads(subprocess.check_output(cmd))

pkgs = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
workspace = set(meta["workspace_members"])
start = next(i for i in workspace if pkgs[i]["name"] == "lavmfx")

# Crates reachable through normal dependencies only.
seen, todo = set(), [start]
while todo:
    cur = todo.pop()
    if cur in seen:
        continue
    seen.add(cur)
    for d in nodes[cur]["deps"]:
        if any(k["kind"] is None for k in d["dep_kinds"]):
            todo.append(d["pkg"])

LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "copyright", "unlicense")
crates = sorted((pkgs[i] for i in seen if i not in workspace), key=lambda p: (p["name"], p["version"]))
texts = {}  # hash -> (text, [crate labels])
lines = []
for p in crates:
    label = f'{p["name"]} {p["version"]}'
    lic = p.get("license") or (f'see {p["license_file"]}' if p.get("license_file") else "unknown")
    pdir = os.path.dirname(p["manifest_path"])
    refs = []
    for f in sorted(os.listdir(pdir)):
        full = os.path.join(pdir, f)
        if os.path.isfile(full) and f.lower().startswith(LICENSE_PREFIXES):
            with open(full, encoding="utf-8", errors="replace") as fh:
                text = fh.read().strip()
            h = hashlib.sha256(text.encode()).hexdigest()[:12]
            texts.setdefault(h, (text, []))[1].append(label)
            refs.append(f"{f} [{h}]")
    lines.append(f"{label}: {lic}" + (f'  ({", ".join(refs)})' if refs else ""))

with open(out_path, "w", encoding="utf-8") as out:
    out.write("Rust crates compiled into LAVMFX, with their licences.\n")
    out.write("Each crate: name version: SPDX licence (licence files [text id]).\n")
    out.write("The licence texts follow, each once, under its text id.\n\n")
    out.write("\n".join(lines))
    out.write("\n")
    for h, (text, users) in texts.items():
        out.write(f"\n\n==================== [{h}] used by: {', '.join(users)}\n\n{text}\n")
print(f"{len(crates)} crates, {len(texts)} licence texts -> {out_path}")
