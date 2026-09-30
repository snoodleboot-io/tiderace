"""`CapLog` — captured log records for the duration of one test, injected **by type** (`log: CapLog`).

pytest's `caplog` attaches a handler to the root logger and hands the test the records it saw. This
is the same idea with no pytest: a `logging.Handler` installed on the root for the test's lifetime,
removed at teardown, plus the level plumbing tests actually reach for (`set_level`, `at_level`).

Three details are load-bearing and easy to get wrong:

* **`record.message` must be populated.** `logging` only sets it when a `Formatter` formats the
  record, so `assert any("x" in rec.message for rec in log.records)` — the common shape — raises
  `AttributeError` if the handler merely appends. The handler formats each record for that reason.
* **Level changes must be undone.** Raising a logger's level to capture DEBUG and leaving it there
  would leak into every later test sharing the interpreter (the no-fork tiers do share one).
* **The root logger's level is pytest's to leave alone (TID-97).** pytest's `catching_logs` does not
  touch it unless `log_level` is configured, so with an unconfigured root (WARNING) a DEBUG record
  is dropped before any handler sees it. An earlier version of this fixture lowered the root to
  NOTSET "to capture everything and let the assertions filter" — and every `assert not
  caplog.messages` was then one library DEBUG line away from failing: asyncio logs `Using
  selector: EpollSelector` at DEBUG when a loop is created, which failed two anyio tests under
  tiderace only. A test that reads INFO/DEBUG without `set_level` / `at_level` / `log_level` fails
  under pytest too; parity is the contract.
"""
from __future__ import annotations

import logging
import shlex

from ._config import _declared_ini

# pytest's `log_format` / `log_date_format` defaults, so `caplog.text` reads the same.
_DEFAULT_FORMAT = "%(levelname)-8s %(name)s:%(filename)s:%(lineno)d %(message)s"
_DEFAULT_DATE_FORMAT = "%H:%M:%S"


def _ini_text(name: str) -> str | None:
    value = _declared_ini(name)
    if isinstance(value, (list, tuple)):
        value = " ".join(str(v) for v in value)
    if isinstance(value, str) and value.strip():
        return value
    return None


def _configured_level() -> int | None:
    """pytest's `get_log_level_for_setting(config, "log_level")`: `--log-level` in the project's
    `addopts`, else the `log_level` ini value, as a numeric level; `None` when neither is set."""
    raw: object = None
    addopts = _ini_text("addopts")
    if addopts:
        try:
            argv = shlex.split(addopts)
        except ValueError:
            argv = []
        for i, arg in enumerate(argv):
            if arg == "--log-level" and i + 1 < len(argv):
                raw = argv[i + 1]
            elif arg.startswith("--log-level="):
                raw = arg.split("=", 1)[1]
    if not raw:
        raw = _declared_ini("log_level")
    if not raw:
        return None
    if isinstance(raw, str):
        text = raw.strip().upper()
        try:
            return int(text)
        except ValueError:
            level = logging.getLevelName(text)
            return level if isinstance(level, int) else None
    try:
        return int(raw)  # type: ignore[call-overload]
    except (TypeError, ValueError):
        return None


class _Recorder(logging.Handler):
    """Appends every record it is given, formatting each so `record.message` is populated."""

    def __init__(self, records: list[logging.LogRecord]) -> None:
        super().__init__()
        self.records = records

    def emit(self, record: logging.LogRecord) -> None:
        record.message = record.getMessage()
        self.records.append(record)

    def reset(self) -> None:
        """pytest's `LogCaptureHandler.reset`: drop what was captured so far."""
        self.records.clear()


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
        self.handler = _Recorder(self.records)
        self.handler.setFormatter(logging.Formatter(_ini_text("log_format") or _DEFAULT_FORMAT,
                                                    _ini_text("log_date_format")
                                                    or _DEFAULT_DATE_FORMAT))
        self._touched: list[tuple[logging.Logger, int]] = []
        self._attached: list[logging.Logger] = []
        self._root_prior = logging.getLogger().level

    @property
    def _handler(self) -> _Recorder:  # the pre-TID-97 spelling, kept for anything that reached in
        return self.handler

    # ---- lifecycle, driven by the provider ----
    def _start(self) -> None:
        root = logging.getLogger()
        self._root_prior = root.level
        # pytest's `catching_logs`: the handler sits at the configured `log_level` and the root is
        # lowered to it if it sat higher; with no `log_level` configured neither level moves, and
        # the root's own level (WARNING, unless the project set it) decides what reaches us.
        level = _configured_level()
        if level is not None:
            self.handler.setLevel(level)
            root.setLevel(min(root.level, level))
        root.addHandler(self.handler)
        self._attached = [root]
        # A non-propagating logger's records never reach the root, so pytest attaches the handler
        # to each one that exists when capture starts. Same here.
        for logger in list(root.manager.loggerDict.values()):
            if isinstance(logger, logging.Logger) and not logger.propagate and logger is not root:
                logger.addHandler(self.handler)
                self._attached.append(logger)

    def _stop(self) -> None:
        for logger, level in reversed(self._touched):
            logger.setLevel(level)
        self._touched.clear()
        for logger in self._attached:
            logger.removeHandler(self.handler)
        self._attached = []
        logging.getLogger().setLevel(self._root_prior)

    # ---- the surface tests use ----
    @property
    def text(self) -> str:
        """Every captured record, formatted, one per line — pytest's `caplog.text`."""
        return "\n".join(self.handler.format(r) for r in self.records)

    @property
    def messages(self) -> list[str]:
        """Just the interpolated messages, in order."""
        return [r.getMessage() for r in self.records]

    @property
    def record_tuples(self) -> list[tuple[str, int, str]]:
        """`(logger_name, levelno, message)` per record — pytest's `caplog.record_tuples`."""
        return [(r.name, r.levelno, r.getMessage()) for r in self.records]

    def get_records(self, when: str) -> list[logging.LogRecord]:
        """pytest's `caplog.get_records(when)`. tiderace does not split a test into setup / call /
        teardown phases, so everything captured is the call's; the other phases read as empty."""
        return list(self.records) if when == "call" else []

    def set_level(self, level: int | str, logger: str | None = None) -> None:
        """Set a logger's level for the rest of the test; restored at teardown."""
        target = logging.getLogger(logger)
        self._touched.append((target, target.level))
        target.setLevel(level)
        self.handler.setLevel(level)

    def at_level(self, level: int | str, logger: str | None = None):
        """Context manager: raise/lower a logger's level for the block, then restore it."""
        return _AtLevel(logging.getLogger(logger), logging.getLevelName(level)
                        if isinstance(level, str) else level, self.handler)

    def clear(self) -> None:
        """Drop the records captured so far, keeping the handler installed."""
        self.records.clear()
