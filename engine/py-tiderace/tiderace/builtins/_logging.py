"""`CapLog` — captured log records for the duration of one test, injected **by type** (`log: CapLog`).

pytest's `caplog` attaches a handler to the root logger and hands the test the records it saw. This
is the same idea with no pytest: a `logging.Handler` installed on the root for the test's lifetime,
removed at teardown, plus the level plumbing tests actually reach for (`set_level`, `at_level`).

Two details are load-bearing and easy to get wrong:

* **`record.message` must be populated.** `logging` only sets it when a `Formatter` formats the
  record, so `assert any("x" in rec.message for rec in log.records)` — the common shape — raises
  `AttributeError` if the handler merely appends. The handler formats each record for that reason.
* **Level changes must be undone.** Raising a logger's level to capture DEBUG and leaving it there
  would leak into every later test sharing the interpreter (the no-fork tiers do share one).
"""
from __future__ import annotations

import logging


class _Recorder(logging.Handler):
    """Appends every record it is given, formatting each so `record.message` is populated."""

    def __init__(self, records: list[logging.LogRecord]) -> None:
        super().__init__()
        self._records = records

    def emit(self, record: logging.LogRecord) -> None:
        record.message = record.getMessage()
        self._records.append(record)


class _AtLevel:
    """Context manager returned by `CapLog.at_level` — restores the prior levels on exit."""

    __slots__ = ("_logger", "_level", "_handler", "_prior", "_prior_handler")

    def __init__(self, logger: logging.Logger, level: int, handler: logging.Handler) -> None:
        self._logger = logger
        self._level = level
        self._handler = handler
        self._prior = logger.level
        self._prior_handler = handler.level

    def __enter__(self) -> CapLog | None:
        self._prior = self._logger.level
        self._prior_handler = self._handler.level
        self._logger.setLevel(self._level)
        self._handler.setLevel(self._level)
        return None

    def __exit__(self, *exc) -> None:
        self._logger.setLevel(self._prior)
        self._handler.setLevel(self._prior_handler)


class CapLog:
    """Log records captured during one test. Distinct type ⇒ unambiguous tiderace type-DI."""

    def __init__(self) -> None:
        self.records: list[logging.LogRecord] = []
        self._handler = _Recorder(self.records)
        self._handler.setFormatter(logging.Formatter("%(levelname)s %(name)s:%(message)s"))
        self._touched: list[tuple[logging.Logger, int]] = []

    # ---- lifecycle, driven by the provider ----
    def _start(self) -> None:
        root = logging.getLogger()
        self._root_prior = root.level
        root.addHandler(self._handler)
        # Capture everything by default and let the assertions filter; a root still sitting at
        # WARNING would silently drop the INFO/DEBUG records most caplog tests are written against.
        root.setLevel(logging.NOTSET)

    def _stop(self) -> None:
        for logger, level in reversed(self._touched):
            logger.setLevel(level)
        self._touched.clear()
        root = logging.getLogger()
        root.removeHandler(self._handler)
        root.setLevel(self._root_prior)

    # ---- the surface tests use ----
    @property
    def text(self) -> str:
        """Every captured record, formatted, one per line — pytest's `caplog.text`."""
        return "\n".join(self._handler.format(r) for r in self.records)

    @property
    def messages(self) -> list[str]:
        """Just the interpolated messages, in order."""
        return [r.getMessage() for r in self.records]

    @property
    def record_tuples(self) -> list[tuple[str, int, str]]:
        """`(logger_name, levelno, message)` per record — pytest's `caplog.record_tuples`."""
        return [(r.name, r.levelno, r.getMessage()) for r in self.records]

    def set_level(self, level: int | str, logger: str | None = None) -> None:
        """Set a logger's level for the rest of the test; restored at teardown."""
        target = logging.getLogger(logger)
        self._touched.append((target, target.level))
        target.setLevel(level)
        self._handler.setLevel(level)

    def at_level(self, level: int | str, logger: str | None = None):
        """Context manager: raise/lower a logger's level for the block, then restore it."""
        return _AtLevel(logging.getLogger(logger), logging.getLevelName(level)
                        if isinstance(level, str) else level, self._handler)

    def clear(self) -> None:
        """Drop the records captured so far, keeping the handler installed."""
        self.records.clear()
