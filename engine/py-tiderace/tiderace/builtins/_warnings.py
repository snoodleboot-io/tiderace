"""Warning capture — the native resource behind pytest's `recwarn`."""
from __future__ import annotations

import warnings
from typing import Any, Iterator


class Warnings:
    """Records the warnings raised during a test, and lets it assert on them.

    The native form is a typed resource — `w: Warnings` — and `recwarn` is the same object under
    pytest's name. It behaves as pytest's `WarningsRecorder` does where the two overlap: iterable,
    sized, indexable, with `pop()` for the first match and `clear()` to start again.
    """

    __slots__ = ("_catcher", "_records")

    def __init__(self) -> None:
        self._catcher: Any = None
        self._records: list = []

    # ---- lifecycle (driven by the provider) ----
    def _start(self) -> None:
        self._catcher = warnings.catch_warnings(record=True)
        self._records = self._catcher.__enter__()
        warnings.simplefilter("always")  # record everything, including duplicates

    def _stop(self) -> None:
        if self._catcher is not None:
            self._catcher.__exit__(None, None, None)
            self._catcher = None

    # ---- what a test uses ----
    @property
    def list(self) -> list:
        """Every warning recorded so far, in order."""
        return list(self._records)

    def __iter__(self) -> Iterator:
        return iter(self._records)

    def __len__(self) -> int:
        return len(self._records)

    def __getitem__(self, index: int):
        return self._records[index]

    def pop(self, cls: type = Warning):
        """The first recorded warning that is an instance of `cls`, removed from the record.

        Raises `AssertionError` when there is none — the same failure pytest gives, because a test
        that pops a warning it never raised is asserting something false."""
        for i, record in enumerate(self._records):
            if issubclass(record.category, cls):
                return self._records.pop(i)
        raise AssertionError(
            f"no warning of type {cls.__name__} was raised; got "
            f"{[r.category.__name__ for r in self._records] or 'none'}"
        )

    def clear(self) -> None:
        self._records.clear()
