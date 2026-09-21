#!/usr/bin/env python3
"""Render the QoS profiles from packages/seyd-qos/src/lib.rs as one table (PLAN.md §2.8).

Reads the `LATENCY`, `BALANCED` and `QUALITY` constants and the `Profile`
struct's field doc comments. A tuned number changes the page on the next
build; nothing is typed twice.

    gen-qos-profiles.py [--out FILE]   # default: web/docs/src/content/docs/reference/qos-profiles.md
"""
import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "packages/seyd-qos/src/lib.rs"
DEFAULT_OUT = ROOT / "web/docs/src/content/docs/reference/qos-profiles.md"

GROUPS = [
    ("Targets the publisher must honour (sent as `video-config`)", ["max_bitrate_kbps", "latency_budget_ms", "max_gop_ms", "prefer_intra_refresh"]),
    ("Seyd's own transport policy", ["fec_delta_pct", "fec_key_pct", "backlog_drop_frames", "recovery_grace_ms"]),
    ("Pilot policy", ["pilot_deadline_delta_ms", "pilot_deadline_key_ms", "pilot_presentation_delay_ms", "on_loss"]),
]


def field_docs(text: str) -> dict[str, str]:
    m = re.search(r"pub struct Profile \{(.*?)\n\}", text, re.S)
    if not m:
        sys.exit("gen-qos-profiles: Profile struct not found")
    docs, doc = {}, []
    for line in m.group(1).splitlines():
        s = line.strip()
        if s.startswith("///"):
            doc.append(s[3:].strip())
        elif s.startswith("pub "):
            name = re.match(r"pub (\w+):", s).group(1)
            docs[name] = " ".join(doc)
            doc = []
        elif not s.startswith("//"):
            doc = []
    return docs


def profiles(text: str) -> list[tuple[str, dict[str, str]]]:
    out = []
    for m in re.finditer(r"pub const (\w+): Profile = Profile \{(.*?)\n\};", text, re.S):
        fields = {}
        for line in m.group(2).splitlines():
            s = line.strip()
            fm = re.match(r"(\w+): (.+),$", s)
            if fm:
                v = fm.group(2).replace("_", "").strip('"')
                v = v.split("::")[-1] if "::" in v else v
                fields[fm.group(1)] = v
        out.append((fields.get("name", m.group(1).lower()), fields))
    if not out:
        sys.exit("gen-qos-profiles: no Profile constants found")
    return out


def first_sentence(doc: str) -> str:
    return doc.split(". ")[0].rstrip(".") + ("." if doc else "")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args()
    text = SOURCE.read_text()
    docs = field_docs(text)
    profs = profiles(text)
    names = [n for n, _ in profs]
    out = [
        "---",
        "title: QoS profiles",
        "description: The three profiles, rendered from the constants in seyd-qos.",
        "---",
        "",
        "Generated from `packages/seyd-qos/src/lib.rs`. A profile is a *ceiling*: the closed-loop",
        "controller moves the bitrate request and the FEC rates inside it, never above it. The",
        "robot picks a profile (`qos_profile` in `seydd.toml`, `seyd_set_qos_profile`,",
        "`Agent.set_qos_profile`) and a driver may ask for another (`SeydSession.setQos`).",
        "Degrade resolution first and frame rate last: for a remote pilot, the interval between",
        "frames is latency, not quality.",
        "",
    ]
    for title, fields in GROUPS:
        out += [f"## {title}", "", "| Field | " + " | ".join(f"`{n}`" for n in names) + " | Meaning |", "|---|" + "---|" * len(names) + "---|"]
        for f in fields:
            vals = " | ".join(f"`{p.get(f, '')}`" for _, p in profs)
            out.append(f"| `{f}` | {vals} | {first_sentence(docs.get(f, ''))} |")
        out.append("")
    out += ["## Field notes", ""]
    for f, d in docs.items():
        if d:
            out += [f"### `{f}`", "", d, ""]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(out))
    print(f"gen-qos-profiles: {len(profs)} profiles → {args.out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
