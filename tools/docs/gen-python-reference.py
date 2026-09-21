#!/usr/bin/env python3
"""Render the `seyd` Python package as a Starlight page (PLAN.md §2.8).

Walks the AST of sdks/python/seyd (no import, so libseyd need not be built)
and lays out every public class, method, property and handler attribute with
its signature and docstring — what Sphinx autodoc would show, in the one
site. Handler attributes (`self.on_command: … = None`) carry their `#:`
comments, the Sphinx convention the package already uses.

    gen-python-reference.py [--out FILE]   # default: web/docs/src/content/docs/reference/python.md
"""
import argparse
import ast
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "sdks/python/seyd"
DEFAULT_OUT = ROOT / "web/docs/src/content/docs/reference/python.md"


def dedent_doc(doc: str | None) -> str:
    if not doc:
        return ""
    text = ast.get_docstring(ast.parse(f'"""{doc}"""')) if False else doc  # already cleaned by ast.get_docstring
    # Sphinx roles → code; indented example blocks → fenced code.
    text = re.sub(r":meth:`([^`]+)`", r"`\1()`", text)
    text = re.sub(r":class:`([^`]+)`", r"`\1`", text)
    text = text.replace("``", "`")
    out, in_block = [], False
    for line in text.splitlines():
        if line.startswith("    ") and not in_block:
            out += ["", "```python"]
            in_block = True
        if in_block and line.strip() and not line.startswith("    "):
            out += ["```", ""]
            in_block = False
        out.append(line[4:] if in_block else line)
    if in_block:
        out.append("```")
    return "\n".join(out).strip()


def signature(fn: ast.FunctionDef) -> str:
    a = fn.args
    parts = []
    positional = a.posonlyargs + a.args
    defaults = [None] * (len(positional) - len(a.defaults)) + list(a.defaults)
    for arg, default in zip(positional, defaults):
        if arg.arg == "self":
            continue
        s = arg.arg + (f": {ast.unparse(arg.annotation)}" if arg.annotation else "")
        if default is not None:
            s += f" = {ast.unparse(default)}"
        parts.append(s)
    if a.vararg:
        parts.append("*" + a.vararg.arg)
    elif a.kwonlyargs:
        parts.append("*")
    for arg, default in zip(a.kwonlyargs, a.kw_defaults):
        s = arg.arg + (f": {ast.unparse(arg.annotation)}" if arg.annotation else "")
        if default is not None:
            s += f" = {ast.unparse(default)}"
        parts.append(s)
    ret = f" -> {ast.unparse(fn.returns)}" if fn.returns else ""
    return f"{fn.name}({', '.join(parts)}){ret}"


def handler_attributes(source: str, init: ast.FunctionDef) -> list[tuple[str, str, str]]:
    """(name, type, doc) for `#:`-documented `self.x: T = …` in __init__."""
    lines = source.splitlines()
    found, doc = [], []
    for lineno in range(init.lineno, init.end_lineno + 1):
        line = lines[lineno - 1]
        stripped = line.strip()
        if stripped.startswith("#:"):
            doc.append(stripped[2:].strip())
            continue
        m = re.match(r"self\.(\w+)\s*:\s*(.+?)\s*=\s*None$", stripped)
        if m and doc:
            typ = m.group(2).replace("|", "\\|")
            found.append((m.group(1), typ, " ".join(doc)))
        doc = []
    return found


def render_class(cls: ast.ClassDef, source: str) -> list[str]:
    out = [f"## `{cls.name}`", ""]
    bases = [ast.unparse(b) for b in cls.bases]
    if bases:
        out += [f"*Bases: {', '.join(f'`{b}`' for b in bases)}*", ""]
    doc = dedent_doc(ast.get_docstring(cls))
    if doc:
        out += [doc, ""]
    # Enum members and class-level constants.
    members = [(n.targets[0].id, ast.unparse(n.value)) for n in cls.body if isinstance(n, ast.Assign) and isinstance(n.targets[0], ast.Name) and not n.targets[0].id.startswith("_")]
    if members and any("Enum" in b for b in bases):
        out += ["| Member | Value |", "|---|---|"]
        out += [f"| `{n}` | `{v}` |" for n, v in members]
        out.append("")
    init = next((n for n in cls.body if isinstance(n, ast.FunctionDef) and n.name == "__init__"), None)
    if init:
        out += ["```python", f"{cls.name}({signature(init)[len('__init__('):]}", "```", ""]
        d = dedent_doc(ast.get_docstring(init))
        if d:
            out += [d, ""]
        attrs = handler_attributes(source, init)
        if attrs:
            out += ["### Handlers", "", "Assign a callable; all are optional. Handlers run on one Seyd thread, in order, and must not block.", "", "| Attribute | Type | Fires |", "|---|---|---|"]
            out += [f"| `{n}` | `{t}` | {d} |" for n, t, d in attrs]
            out.append("")
    for node in cls.body:
        if not isinstance(node, ast.FunctionDef) or node.name.startswith("_"):
            continue
        is_prop = any(isinstance(d, ast.Name) and d.id == "property" for d in node.decorator_list)
        if is_prop:
            ret = f": {ast.unparse(node.returns)}" if node.returns else ""
            out += [f"### `{node.name}`{ret}", "", "*Property.*", ""]
        else:
            out += [f"### `{signature(node)}`", ""]
        d = dedent_doc(ast.get_docstring(node))
        if d:
            out += [d, ""]
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = ap.parse_args()
    init_src = (PACKAGE / "__init__.py").read_text()
    agent_src = (PACKAGE / "agent.py").read_text()
    ffi_src = (PACKAGE / "_ffi.py").read_text()
    init_mod, agent_mod, ffi_mod = ast.parse(init_src), ast.parse(agent_src), ast.parse(ffi_src)

    exported = []
    for node in init_mod.body:
        if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name) and node.targets[0].id == "__all__":
            exported = [ast.literal_eval(e) for e in node.value.elts]
    out = [
        "---",
        "title: Python (seyd)",
        "description: The seyd package, rendered from its docstrings.",
        "---",
        "",
        "Generated from the docstrings of `sdks/python/seyd`. The package is a thin wrapper over",
        "`libseyd` (ADR 0004): every call maps onto one C ABI function, and no protocol logic lives",
        "in Python. See [A robot in Python](/docs/robot/python/) for the guide and",
        "[C ABI](/docs/reference/c/) for what each call does underneath.",
        "",
        dedent_doc(ast.get_docstring(init_mod)),
        "",
        f"Exported names: {', '.join(f'`{n}`' for n in exported)}.",
        "",
    ]
    classes = {n.name: n for n in agent_mod.body if isinstance(n, ast.ClassDef)}
    classes.update({n.name: n for n in ffi_mod.body if isinstance(n, ast.ClassDef) and n.name in exported})
    order = [n for n in exported if n in classes]
    for name in order:
        out += render_class(classes[name], agent_src if name in [c.name for c in agent_mod.body if isinstance(c, ast.ClassDef)] else ffi_src)
    if "ABI_VERSION" in exported:
        out += ["## `ABI_VERSION`", "", "The `SEYD_ABI_VERSION` of the header the package was generated against; compared with `seyd_abi_version()` of the loaded library at import.", ""]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(out))
    print(f"gen-python-reference: {len(order)} classes → {args.out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
