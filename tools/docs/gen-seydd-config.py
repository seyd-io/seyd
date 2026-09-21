#!/usr/bin/env python3
"""Render seydd's configuration schema from packages/seydd/src/config.rs (PLAN.md §2.8).

The serde structs are the schema: each `pub struct` becomes a TOML table with
one row per field — its type, whether it is required, its default (read from
the `#[serde(default = "fn")]` function bodies) and its `///` comment. A field
added to config.rs appears here on the next build without anyone remembering.

    gen-seydd-config.py [--out FILE]   # default: web/docs/src/content/docs/reference/seydd-config.md
"""
import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "packages/seydd/src/config.rs"
DEFAULT_OUT = ROOT / "web/docs/src/content/docs/reference/seydd-config.md"

# Struct → the TOML table it deserialises from, in the order the page lists them.
TABLES = [
    ("Agent", "[agent]", "The robot's identity, transport and policy."),
    ("Channel", "[[channel]]", "One per channel; numbered from 1 in file order, and that numbering is what the pilot sees."),
    ("Layer", "[[channel.layer]]", "Simulcast layers of a video channel (ADR 0008), lowest first."),
    ("PublisherControl", "[publisher_control]", "Where seydd posts `video-config`, `recovery-request`, `layer` and `session` messages for the publisher."),
]

TYPE_NAMES = {
    "String": "string", "PathBuf": "path", "u16": "integer", "u32": "integer", "bool": "boolean",
    "Option<String>": "string", "Vec<Layer>": "array of tables", "ChannelKind": '`"video"` \\| `"sensor"` \\| `"command"`',
}


def default_values(text: str) -> dict[str, str]:
    """`fn name() -> T { literal }` → {name: literal}."""
    out = {}
    for m in re.finditer(r"fn (\w+)\(\) -> [^{]+\{\s*(.*?)\s*\}", text, re.S):
        body = m.group(2).strip()
        lit = re.search(r'"([^"]*)"', body)
        out[m.group(1)] = f'"{lit.group(1)}"' if lit else body.rstrip(";")
    return out


def parse_struct(text: str, name: str) -> list[dict]:
    m = re.search(rf"pub struct {name} \{{(.*?)\n\}}", text, re.S)
    if not m:
        sys.exit(f"gen-seydd-config: struct {name} not found")
    fields, doc, attrs = [], [], []
    for line in m.group(1).splitlines():
        s = line.strip()
        if s.startswith("///"):
            doc.append(s[3:].strip())
        elif s.startswith("#["):
            attrs.append(s)
        elif s.startswith("pub "):
            fm = re.match(r"pub (\w+): (.+),", s)
            if fm:
                fields.append({"name": fm.group(1), "type": fm.group(2), "doc": " ".join(doc), "attrs": list(attrs)})
            doc, attrs = [], []
    return fields


def field_row(f: dict, defaults: dict[str, str]) -> str:
    name = f["name"]
    serde = " ".join(f["attrs"])
    rename = re.search(r'rename = "(\w+)"', serde)
    if rename:
        name = rename.group(1)
    if 'default = "' in serde:
        fn = re.search(r'default = "(\w+)"', serde).group(1)
        default = f"`{defaults.get(fn, fn)}`"
        required = "no"
    elif "default" in serde:
        default = "empty" if f["type"].startswith(("Vec", "Option")) else "`0`/`false`"
        required = "no"
    else:
        default, required = "—", "**yes**"
    typ = TYPE_NAMES.get(f["type"], f["type"])
    if f["type"] == "Option<String>" or (f["type"].startswith("Option") and "Vec" not in f["type"]):
        typ = "string"
    return f"| `{name}` | {typ} | {required} | {default} | {f['doc'].replace('|', chr(92) + '|')} |"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args()
    text = SOURCE.read_text()
    defaults = default_values(text)
    out = [
        "---",
        "title: seydd.toml",
        "description: Every key of the daemon's configuration file, rendered from its serde structs.",
        "---",
        "",
        "Generated from `packages/seydd/src/config.rs`: the structs the daemon deserialises the",
        "file into are the schema, so this page cannot disagree with the binary. Pass the file with",
        "`seydd --config /etc/seyd/seydd.toml`. See [Integrate the daemon](/docs/robot/daemon/) for",
        "a walkthrough and the [protocol contract](/docs/reference/protocol/seydd/) for the",
        "publisher-control messages.",
        "",
    ]
    for struct, table, blurb in TABLES:
        fields = parse_struct(text, struct)
        out += [f"## `{table}`", "", blurb, "", "| Key | Type | Required | Default | Meaning |", "|---|---|---|---|---|"]
        out += [field_row(f, defaults) for f in fields]
        out.append("")
    # Hand the ChannelKind enum and the Layer doc-comment example through verbatim.
    m = re.search(r"/// One simulcast layer.*?(```toml.*?```)", text, re.S)
    if m:
        example = "\n".join(l.strip()[4:] if l.strip().startswith("/// ") else l.strip()[3:] for l in m.group(1).splitlines())
        out += ["## Simulcast example", "", "From the `Layer` doc comment in `config.rs`:", "", example, ""]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(out))
    print(f"gen-seydd-config: {len(TABLES)} tables → {args.out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
