"""`MonkeyPatch` — tiderace's native, function-scoped patcher (the analogue of pytest's `monkeypatch`).

Wired **by type** (`mp: MonkeyPatch`), not by name. Every mutation records its inverse; `undo()` replays
them in reverse at teardown, so a test that patches env/attrs/items is fully isolated from the next.
No pytest import — this is a plain object the tiderace builtin provider yields."""
from __future__ import annotations

import os
import sys
from typing import Any

_SENTINEL = object()  # marks "attribute/key did not exist" so undo deletes instead of restoring


def _pytest_notset_sentinels() -> tuple:
    """Whichever "key did not exist" sentinels the running pytest defines.

    pytest renamed its sentinel between releases — `_pytest.monkeypatch.notset` (a `Notset`) through
    8.x, `NOTSET` (a `NotSetType` enum from `_pytest.compat`) in 9.x — and compares it *by identity*.
    So this collects whatever actually exists and `undo()` compares by identity too. Importing a
    single name would reproduce, inside its own fix, the very version gap TID-44 is about.

    Only called when pytest-format records exist, and only pytest can have produced those, so a suite
    that never touches pytest still never imports it."""
    try:
        import _pytest.monkeypatch as pytest_monkeypatch
    except ImportError:
        return ()
    names = ("notset", "NOTSET")
    return tuple(v for v in (getattr(pytest_monkeypatch, n, None) for n in names) if v is not None)


class MonkeyPatch:
    """Record-and-undo mutations for the duration of one test.

    Mirrors the subset of pytest's `MonkeyPatch` API that the conformance corpus actually uses:
    `setattr`/`delattr`/`setitem`/`delitem`/`setenv`/`delenv`/`syspath_prepend`/`chdir`. Each call
    appends an undo thunk; `undo()` (called by the provider's teardown) runs them last-in-first-out."""

    def __init__(self) -> None:
        self._undo: list = []  # list[Callable[[], None]] — inverse ops, replayed in reverse
        # pytest's *private* setitem undo log, as `(mapping, key, old_value)` tuples (TID-44). Not part
        # of pytest's API, but real suites reach into it: flask's conftest appends a session's worth of
        # standard-environ records to `monkeypatch._setitem` so every test's teardown resets
        # `os.environ`. Without this attribute that autouse fixture errored on setup for 451 of flask's
        # 482 tests. Entries appended here are honoured by `undo()` exactly as pytest honours them.
        self._setitem: list = []

    # ---- attributes ----
    def setattr(self, target: Any, name: str, value: Any = _SENTINEL, raising: bool = True) -> None:
        """`setattr(obj, "attr", value)` or the string-target form `setattr("pkg.mod.attr", value)`.

        `raising=True` (the default) refuses to create an attribute that does not already exist —
        pytest's guard against a patch that silently does nothing because the name was misspelled or
        moved. `raising=False` allows it."""
        if value is _SENTINEL:
            target, name, value = self._resolve_target(target, name)
        old = getattr(target, name, _SENTINEL)
        if old is _SENTINEL and raising:
            raise AttributeError(f"{target!r} has no attribute {name!r}")
        self._undo.append(
            (lambda: setattr(target, name, old)) if old is not _SENTINEL
            else (lambda: delattr(target, name))
        )
        setattr(target, name, value)

    def delattr(self, target: Any, name: str = _SENTINEL, raising: bool = True) -> None:
        if name is _SENTINEL:
            target, name, _ = self._resolve_target(target, _SENTINEL)
        old = getattr(target, name, _SENTINEL)
        if old is _SENTINEL and raising:
            raise AttributeError(f"{target!r} has no attribute {name!r}")
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

    def delitem(self, mapping: Any, key: Any, raising: bool = True) -> None:
        if key not in mapping and raising:
            raise KeyError(key)
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
    def context(self) -> "_MonkeyPatchContext":
        """A nested patcher undone at the end of the `with` block, not at teardown.

        ```python
        with monkeypatch.context() as m:
            m.setenv("MODE", "test")
        # already undone here
        ```

        The inner patcher is independent: what it records is undone on exit, and this one's own
        records are untouched."""
        return _MonkeyPatchContext()

    def undo(self) -> None:
        """Replay every recorded inverse, newest first; idempotent (the queues empty).

        tiderace's own inverses run first, then any pytest-format `_setitem` records, newest first —
        the order pytest itself uses (attribute undos, then item undos). For flask that means the
        standard environ it appended is what survives, which is what it appended it for."""
        while self._undo:
            self._undo.pop()()
        if self._setitem:
            notset = _pytest_notset_sentinels()
            while self._setitem:
                mapping, key, old = self._setitem.pop()
                if old is _SENTINEL or any(old is sentinel for sentinel in notset):
                    try:
                        del mapping[key]
                    except KeyError:
                        pass  # already gone — exactly pytest's behaviour
                else:
                    mapping[key] = old

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


class _MonkeyPatchContext:
    """The object `MonkeyPatch.context()` yields — a fresh patcher scoped to the `with` block."""

    __slots__ = ("_mp",)

    def __init__(self) -> None:
        self._mp = MonkeyPatch()

    def __enter__(self) -> MonkeyPatch:
        return self._mp

    def __exit__(self, *exc: object) -> None:
        self._mp.undo()
