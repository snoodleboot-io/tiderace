"""`TmpPath` — a fresh, per-test temporary directory, injected **by type** (`p: TmpPath`).

Why a subclass of `pathlib.Path` rather than plain `Path`? riptide wires by type, and a bare
`Path` parameter would (a) collide with any user provider that also returns `Path` and (b) wrongly
capture *every* `Path`-annotated param. `TmpPath` is a distinct type for unambiguous type-DI while
still being a real `Path` (`isinstance(p, pathlib.Path)` holds), so existing path code just works.
The builtin provider creates the dir and removes it on teardown."""
from __future__ import annotations

import pathlib

# `pathlib.Path.__new__` dispatches to the OS-specific flavour; subclass that concrete type so
# instances are real, fully-functional paths (Path subclassing is supported on the engine's 3.12+).
_Base = type(pathlib.Path())


class TmpPath(_Base):  # type: ignore[misc,valid-type]
    """A `pathlib.Path` to a fresh temp directory. Distinct type ⇒ unambiguous riptide type-DI."""

    __slots__ = ()
