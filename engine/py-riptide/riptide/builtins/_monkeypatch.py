"""`MonkeyPatch` — riptide's native, function-scoped patcher (the analogue of pytest's `monkeypatch`).

Wired **by type** (`mp: MonkeyPatch`), not by name. Every mutation records its inverse; `undo()` replays
them in reverse at teardown, so a test that patches env/attrs/items is fully isolated from the next.
No pytest import — this is a plain object the riptide builtin provider yields."""
from __future__ import annotations

import os
import sys
from typing import Any

_SENTINEL = object()  # marks "attribute/key did not exist" so undo deletes instead of restoring


class MonkeyPatch:
    """Record-and-undo mutations for the duration of one test.

    Mirrors the subset of pytest's `MonkeyPatch` API that the conformance corpus actually uses:
    `setattr`/`delattr`/`setitem`/`delitem`/`setenv`/`delenv`/`syspath_prepend`/`chdir`. Each call
    appends an undo thunk; `undo()` (called by the provider's teardown) runs them last-in-first-out."""

    def __init__(self) -> None:
        self._undo: list = []  # list[Callable[[], None]] — inverse ops, replayed in reverse

    # ---- attributes ----
    def setattr(self, target: Any, name: str, value: Any = _SENTINEL) -> None:
        """`setattr(obj, "attr", value)` or the string-target form `setattr("pkg.mod.attr", value)`."""
        if value is _SENTINEL:
            target, name, value = self._resolve_target(target, name)
        old = getattr(target, name, _SENTINEL)
        self._undo.append(
            (lambda: setattr(target, name, old)) if old is not _SENTINEL
            else (lambda: delattr(target, name))
        )
        setattr(target, name, value)

    def delattr(self, target: Any, name: str = _SENTINEL) -> None:
        if name is _SENTINEL:
            target, name, _ = self._resolve_target(target, _SENTINEL)
        old = getattr(target, name, _SENTINEL)
        if old is not _SENTINEL:
            self._undo.append(lambda: setattr(target, name, old))
            delattr(target, name)

    # ---- mapping items ----
    def setitem(self, mapping: Any, key: Any, value: Any) -> None:
        old = mapping.get(key, _SENTINEL) if hasattr(mapping, "get") else _SENTINEL
        self._undo.append(
            (lambda: mapping.__setitem__(key, old)) if old is not _SENTINEL
            else (lambda: mapping.__delitem__(key))
        )
        mapping[key] = value

    def delitem(self, mapping: Any, key: Any) -> None:
        if key in mapping:
            old = mapping[key]
            self._undo.append(lambda: mapping.__setitem__(key, old))
            del mapping[key]

    # ---- environment ----
    def setenv(self, name: str, value: str, prepend: str | None = None) -> None:
        if prepend is not None and name in os.environ:
            value = value + prepend + os.environ[name]
        self.setitem(os.environ, name, str(value))

    def delenv(self, name: str, raising: bool = True) -> None:
        if name not in os.environ and raising:
            raise KeyError(name)
        if name in os.environ:
            self.delitem(os.environ, name)

    # ---- sys.path / cwd ----
    def syspath_prepend(self, path: Any) -> None:
        saved = list(sys.path)
        self._undo.append(lambda: sys.path.__setitem__(slice(None), saved))
        sys.path.insert(0, str(path))

    def chdir(self, path: Any) -> None:
        old = os.getcwd()
        self._undo.append(lambda: os.chdir(old))
        os.chdir(str(path))

    # ---- teardown ----
    def undo(self) -> None:
        """Replay every recorded inverse, newest first; idempotent (the queue empties)."""
        while self._undo:
            self._undo.pop()()

    @staticmethod
    def _resolve_target(dotted: str, name: Any) -> tuple:
        """Support pytest's string-target form: `setattr("os.path.join", fn)` → (os.path, "join", fn)."""
        import importlib

        if not isinstance(dotted, str):
            return dotted, name, _SENTINEL
        module_path, _, attr = dotted.rpartition(".")
        obj = importlib.import_module(module_path)
        # `name` here is actually the *value* in the two-arg string form.
        return obj, attr, name
