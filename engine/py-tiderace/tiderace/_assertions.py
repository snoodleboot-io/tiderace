"""Assertion helpers with no pytest counterpart in the decorator/fixture surface (TID-38).

`migrate` rewrites `import pytest` to `import tiderace`, but until now it only remapped *decorators*
and *fixtures*. A module using `pytest.raises` or `pytest.approx` came out naming `pytest` with
nothing bound to it, and every test in it raised `NameError` at call time — while the migration
report showed the file as fully mapped. On one real suite that was 86 of 542 modules.

These are the two helpers that actually carry their weight: `raises` appeared 236 times and `approx`
17 in that suite. Providing them is what makes "drop the pytest dependency" true rather than
aspirational, so the migrator can rewrite those call sites instead of leaving a landmine.

Deliberately small. This is not a project to reimplement pytest's assertion surface — anything not
here is reported as a can't-map finding and keeps its `import pytest`, which is honest and lets the
suite run.
"""

from __future__ import annotations

import math
import re as _re
from types import TracebackType
from typing import Any, Generic, TypeVar

__all__ = ["raises", "approx", "RaisesContext", "Approx"]

E = TypeVar("E", bound=BaseException)


class RaisesContext(Generic[E]):
    """The context manager `raises` returns. `.value` holds the caught exception after the block.

    Matches the shape of pytest's own so migrated code needs no edit: `with raises(ValueError) as
    excinfo: ...` then `excinfo.value`.
    """

    def __init__(self, expected: type[E] | tuple[type[E], ...], match: str | None = None):
        self.expected = expected
        self.match = match
        self.value: E | None = None

    def __enter__(self) -> RaisesContext[E]:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> bool:
        if exc_type is None:
            expected = getattr(self.expected, "__name__", None) or str(self.expected)
            raise AssertionError(f"DID NOT RAISE {expected}")
        if not issubclass(exc_type, self.expected):  # let anything unexpected propagate untouched
            return False
        if self.match is not None and not _re.search(self.match, str(exc)):
            raise AssertionError(
                f"the raised {exc_type.__name__} did not match {self.match!r}: {str(exc)!r}"
            )
        self.value = exc  # type: ignore[assignment]
        return True


def raises(
    expected: type[E] | tuple[type[E], ...],
    *args: Any,
    match: str | None = None,
    **kwargs: Any,
) -> RaisesContext[E] | E:
    """Assert that a block — or a call — raises `expected`.

    Both spellings pytest supports:

        with raises(ValueError):                 ...
        with raises(ValueError, match="bad"):    ...
        raises(ValueError, fn, arg)              # call form

    An exception that is *not* `expected` propagates rather than being swallowed, so a test that
    breaks for an unrelated reason still says so.
    """
    if args:
        func, rest = args[0], args[1:]
        with RaisesContext(expected, match) as ctx:
            func(*rest, **kwargs)
        return ctx.value  # type: ignore[return-value]
    return RaisesContext(expected, match)


class Approx:
    """Inexact numeric comparison. Compares equal to a number, or elementwise to a sequence/mapping.

    Same default tolerances as pytest — relative 1e-6, absolute 1e-12 — so a migrated assertion keeps
    the answer it had. The relative term is taken against the larger magnitude, which is what makes
    the comparison symmetric.
    """

    def __init__(self, expected: Any, rel: float | None = None, abs: float | None = None):
        self.expected = expected
        self.rel = 1e-6 if rel is None else rel
        self.abs = 1e-12 if abs is None else abs

    def _close(self, actual: Any, expected: Any) -> bool:
        if isinstance(expected, bool) or isinstance(actual, bool):
            return actual == expected
        try:
            a, e = float(actual), float(expected)
        except (TypeError, ValueError):
            return bool(actual == expected)
        if math.isnan(a) or math.isnan(e):
            return math.isnan(a) and math.isnan(e)
        if math.isinf(a) or math.isinf(e):
            return a == e
        return abs(a - e) <= max(self.abs, self.rel * max(abs(a), abs(e)))

    def __eq__(self, actual: Any) -> bool:
        expected = self.expected
        if isinstance(expected, dict):
            if not isinstance(actual, dict) or set(actual) != set(expected):
                return False
            return all(self._close(actual[k], expected[k]) for k in expected)
        if isinstance(expected, (list, tuple)):
            if not isinstance(actual, (list, tuple)) or len(actual) != len(expected):
                return False
            return all(self._close(a, e) for a, e in zip(actual, expected))
        return self._close(actual, expected)

    def __ne__(self, actual: Any) -> bool:
        return not self.__eq__(actual)

    def __hash__(self) -> int:  # __eq__ is defined, so this must be too
        return id(self)

    def __repr__(self) -> str:
        return f"approx({self.expected!r}, rel={self.rel}, abs={self.abs})"


def approx(expected: Any, rel: float | None = None, abs: float | None = None) -> Approx:
    """Wrap `expected` so `==` compares within a tolerance: `assert total == approx(0.3)`."""
    return Approx(expected, rel=rel, abs=abs)
