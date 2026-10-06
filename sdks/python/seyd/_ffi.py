# Copyright 2026 Anton Gravestam
# SPDX-License-Identifier: Apache-2.0
"""Binding to `libseyd` through cffi in ABI mode.

ABI mode means the wheel needs no compiler and no Rust toolchain at install
time: it dlopens a prebuilt `libseyd` and reads the declarations out of the
`seyd.h` that cbindgen generated for that same library.

The declarations are *parsed from the header*, never retyped here. A wrapper
that hand-copies a struct layout drifts from the core the first time a field is
added, which is exactly the failure ADR 0004 exists to prevent.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

from cffi import FFI

_PACKAGE_DIR = Path(__file__).resolve().parent
_REPO_ROOT = _PACKAGE_DIR.parents[2]

#: Library filename for this platform.
_LIB_NAME = {
    "darwin": "libseyd.dylib",
    "win32": "seyd.dll",
}.get(sys.platform, "libseyd.so")


class SeydLibraryNotFound(RuntimeError):
    """`libseyd` could not be located. Set `SEYD_LIBRARY` to its full path."""


def _header_path() -> Path:
    """`seyd.h`, from the installed package or from a source checkout."""
    candidates = [
        _PACKAGE_DIR / "include" / "seyd.h",  # bundled in the wheel
        _REPO_ROOT / "sdks" / "c" / "include" / "seyd.h",  # working tree
    ]
    for path in candidates:
        if path.is_file():
            return path
    raise SeydLibraryNotFound(f"seyd.h not found; looked in {candidates}")


def _library_path() -> str:
    """`libseyd`, preferring an explicit override, then the wheel, then the
    workspace's own build output so the SDK is usable straight from a checkout.
    """
    override = os.environ.get("SEYD_LIBRARY")
    if override:
        if not Path(override).is_file():
            raise SeydLibraryNotFound(f"SEYD_LIBRARY={override} does not exist")
        return override
    candidates = [
        _PACKAGE_DIR / _LIB_NAME,
        _REPO_ROOT / "target" / "release" / _LIB_NAME,
        _REPO_ROOT / "target" / "debug" / _LIB_NAME,
    ]
    for path in candidates:
        if path.is_file():
            return str(path)
    # Fall back to the loader's search path (a system-installed libseyd).
    return _LIB_NAME


def _cdef_from_header(text: str) -> str:
    """Reduce `seyd.h` to what cffi's parser accepts.

    cffi runs no C preprocessor, so the include guard, the `#include`s and the
    `extern "C"` wrapper have to go. Everything that carries meaning — types,
    struct layouts, function signatures — is passed through untouched.
    """
    lines = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        if 'extern "C"' in line:
            continue
        lines.append(line)
    return "\n".join(lines)


def _abi_version_from_header(text: str) -> int:
    m = re.search(r"^#define\s+SEYD_ABI_VERSION\s+(\d+)", text, re.MULTILINE)
    if not m:
        raise SeydLibraryNotFound("seyd.h has no SEYD_ABI_VERSION")
    return int(m.group(1))


_header = _header_path().read_text()

#: ABI version this wrapper was built against.
HEADER_ABI_VERSION = _abi_version_from_header(_header)

ffi = FFI()
ffi.cdef(_cdef_from_header(_header))
lib = ffi.dlopen(_library_path())

if lib.seyd_abi_version() != HEADER_ABI_VERSION:
    raise SeydLibraryNotFound(
        f"libseyd reports ABI {lib.seyd_abi_version()}, "
        f"but this wrapper was built for ABI {HEADER_ABI_VERSION}"
    )


def last_error() -> str:
    """The reason for this thread's most recent failed call."""
    return ffi.string(lib.seyd_last_error()).decode("utf-8", "replace")


def to_c(value: str | os.PathLike | None) -> object:
    """A `const char*` that stays alive as long as the returned object does."""
    if value is None:
        return ffi.NULL
    return ffi.new("char[]", os.fspath(value).encode("utf-8"))
