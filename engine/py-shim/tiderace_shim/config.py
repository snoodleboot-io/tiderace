"""The project's own pytest configuration, loaded once (TID-121), and the run's (TID-124).

pytest reads one config file — the nearest at or above the run root that *carries* a pytest
section, in its precedence order — and that file's directory is the rootdir. The shim used to
open the same four files twice (once for `addopts`, once for every other setting), split
`addopts` with `shlex` in three places and scan it by string containment in a fourth. This is the
one read: a [`ProjectConfig`] with the sections, the `addopts` argv, and the lookups the shim
needs — a setting's values, a flag's presence, an option's value in the spellings pytest accepts.

A [`RunConfig`] is what one run of the shim is configured with — the root, the project, what its
`addopts` ignores, the modules `--modules` names — loaded once by the mode that starts the shim
and handed to discovery and the engine, where module globals used to carry each piece.
"""
from __future__ import annotations

import configparser
import fnmatch
import os
import shlex
import tomllib
from dataclasses import dataclass, field
from typing import Any

from .selection import path_names

NOTSET = object()

# pytest's own precedence order.
CONFIG_FILES = ("pytest.ini", "pyproject.toml", "tox.ini", "setup.cfg")


@dataclass(frozen=True)
class ProjectConfig:
    """What the project's config says. `dir` is pytest's rootdir — the config file's directory,
    or the run root when there is none (`source` is then `None`); `sections` every table a setting
    may live in, in the order pytest reads the files (for `pyproject.toml`:
    `[tool.pytest.ini_options]`, pytest 9's native `[tool.pytest]`, then `[tool.tiderace]`)."""

    dir: str
    source: str | None
    sections: tuple[dict, ...]
    addopts: str
    argv: tuple[str, ...] = field(init=False)

    def __post_init__(self) -> None:
        try:
            argv = tuple(shlex.split(self.addopts)) if self.addopts else ()
        except ValueError:  # unbalanced quotes: no options rather than a guess
            argv = ()
        object.__setattr__(self, "argv", argv)

    def values(self, key: str) -> list:
        """`key` from every section, as a flat list: a multi-line ini value is its lines, a TOML
        list its items."""
        out: list = []
        for section in self.sections:
            value = section.get(key)
            if value is None:
                continue
            if isinstance(value, str):
                out.extend(v for v in value.splitlines() if v.strip())
            elif isinstance(value, (list, tuple)):
                out.extend(value)
            else:
                out.append(value)
        return out

    def setting(self, key: str) -> Any:
        """`key` as the project spells it — the raw value of the first section that sets it — or
        `NOTSET`. For a setting whose *emptiness* means something (`plugins = []`)."""
        for section in self.sections:
            value = section.get(key)
            if value is not None:
                return value
        return NOTSET

    def flag(self, name: str) -> bool:
        """Whether `addopts` carries the bare flag."""
        return name in self.argv

    def opt_values(self, flag: str) -> list[str]:
        """Every value given for `flag` in `addopts`: `--flag value`, `--flag=value`, and for a
        short flag the joined spelling too (`-mEXPR`, `-pNAME`)."""
        return option_values(self.argv, flag)

    def opt(self, flag: str) -> str | None:
        """The first value given for `flag` in `addopts`, or `None`."""
        values = self.opt_values(flag)
        return values[0] if values else None


def option_values(argv: tuple[str, ...] | list[str], flag: str) -> list[str]:
    """`flag`'s values in an argv: `flag value`, `flag=value`, and — for a short flag — `flagVALUE`."""
    short = not flag.startswith("--")
    out: list[str] = []
    for i, arg in enumerate(argv):
        if arg == flag:
            if i + 1 < len(argv):
                out.append(argv[i + 1])
        elif arg.startswith(flag + "="):
            out.append(arg[len(flag) + 1:])
        elif short and arg.startswith(flag) and len(arg) > len(flag):
            out.append(arg[len(flag):])
    return out


def option(argv: tuple[str, ...] | list[str], flag: str) -> str | None:
    """The first value of `flag` in an argv, or `None` — the shim's own command line too."""
    values = option_values(argv, flag)
    return values[0] if values else None


def _read(path: str, name: str) -> tuple[bool, list[dict]] | None:
    """One config file: whether it carries a pytest section, and its sections in order. `None`
    when it cannot be read — an unreadable config must not stop the run."""
    try:
        if name == "pyproject.toml":
            with open(path, "rb") as fh:
                tool = tomllib.load(fh).get("tool", {})
            pytest_tool = tool.get("pytest") if isinstance(tool.get("pytest"), dict) else None
            table = pytest_tool or {}
            return pytest_tool is not None, [
                table.get("ini_options") or {},
                {k: v for k, v in table.items() if k != "ini_options"},
                tool.get("tiderace", {}),
            ]
        parser = configparser.ConfigParser()
        parser.read(path)
        header = "tool:pytest" if name == "setup.cfg" else "pytest"
        found = parser.has_section(header)
        return found, [dict(parser[header]) if found else {}]
    except Exception:  # noqa: BLE001
        return None


def _sections_in(directory: str) -> tuple[dict, ...]:
    """Every section a setting may live in, from every config file in `directory`."""
    out: list[dict] = []
    for name in CONFIG_FILES:
        path = os.path.join(directory, name)
        if not os.path.exists(path):
            continue
        read = _read(path, name)
        if read is not None:
            out.extend(read[1])
    return tuple(out)


def _addopts_in(sections: list[dict]) -> str:
    """`addopts` from a file's pytest sections: `[tool.pytest.ini_options]` when it is there,
    else the native table — joined, since the native TOML table takes a list."""
    for section in sections[:2]:
        if not section:
            continue
        addopts = section.get("addopts", "") or ""
        if isinstance(addopts, (list, tuple)):
            addopts = " ".join(str(a) for a in addopts)
        return str(addopts)
    return ""


def load_project_config(start: str) -> ProjectConfig:
    """The nearest config at or above `start` that carries a pytest section — stopping at the
    first file that *carries* one rather than the first that exists: a `pyproject.toml` with no
    `[tool.pytest.ini_options]` does not mean the project has no pytest config. Its directory is
    the rootdir, and `--ignore` paths resolve against it. An empty `[pytest]` in a `pytest.ini`
    counts (TID-100). With none found, the rootdir is `start` and its own files are still read
    for `[tool.tiderace]`."""
    directory = os.path.abspath(start)
    while True:
        for name in CONFIG_FILES:
            path = os.path.join(directory, name)
            if not os.path.exists(path):
                continue
            read = _read(path, name)
            if read is None or not read[0]:
                continue
            return ProjectConfig(directory, path, _sections_in(directory), _addopts_in(read[1]))
        parent = os.path.dirname(directory)
        if parent == directory:
            root = os.path.abspath(start)
            return ProjectConfig(root, None, _sections_in(root), "")
        directory = parent


# ------------------------------------------------------------------------------ the run
def ignores(project: ProjectConfig) -> tuple:
    """`--ignore` / `--ignore-glob` paths out of the project's `addopts`, resolved to absolute paths
    against the config's own directory, as pytest resolves them.

    A project that excludes a directory from its default run means it: pirn-core's `--ignore=tests/perf`
    holds benchmarks that need the `pytest-benchmark` plugin, and collecting them anyway reported 23
    failures for tests pytest never runs. Ignored here rather than in the Rust collector because this
    is where the project's own config is already being read."""
    return tuple((os.path.abspath(os.path.join(project.dir, value)), glob)
                 for flag, glob in (("--ignore", False), ("--ignore-glob", True))
                 for value in project.opt_values(flag) if value)


def _force_asyncio(project: ProjectConfig) -> bool:
    """`asyncio_mode = "auto"` means pytest-asyncio claims *every* async test, including ones carrying
    `@pytest.mark.anyio`. In that configuration pytest runs even a `[trio]`-labelled variant on an
    asyncio loop — the id says trio and the loop never is. Emulating the suite's configured
    toolchain is the job here, so the same thing happens: the expansion still produces one variant
    per backend, as pytest's ids do, and they all run where pytest runs them (TID-54)."""
    if not any(str(v).strip().strip('"\'') == "auto" for v in project.values("asyncio_mode")):
        return False
    try:
        import pytest_asyncio  # noqa: F401 — only its presence matters
    except Exception:  # noqa: BLE001 — declared but not installed: nothing claims the tests
        return False
    return True


@dataclass(frozen=True)
class RunConfig:
    """What one run of the shim is configured with (TID-124). `root` is the run root as the engine
    gave it (argv[1]); `project` the project's own config; `ignored` the absolute paths its
    `addopts` excludes; `force_asyncio` whether pytest-asyncio's auto mode drives every async test;
    `modules` the test modules this run executes, suite-relative (`tests/x/test_y.py`), or None
    for all of them (TID-75) — set from `--modules <file>` before anything is imported, so a run
    that executes one test does not pay the import of every test module in the suite (4s on
    pirn-agents, the whole of a one-test run after an edit)."""

    root: str
    project: ProjectConfig
    ignored: tuple = ()
    force_asyncio: bool = False
    modules: frozenset | None = None

    @classmethod
    def load(cls, root: str, *, modules_file: str | None = None) -> RunConfig:
        project = load_project_config(root)
        modules = None
        if modules_file:
            with open(modules_file, encoding="utf-8") as fh:
                modules = frozenset(line.strip() for line in fh if line.strip())
        return cls(root, project, ignores(project), _force_asyncio(project), modules)

    @property
    def abs_root(self) -> str:
        return os.path.abspath(self.root or ".")

    def is_ignored(self, path: str) -> bool:
        """Is `path` (absolute) excluded by the project's own `--ignore` / `--ignore-glob`?"""
        if not self.ignored:
            return False
        # Absolute on both sides: the run root arrives as `.` as often as not, and a relative path
        # never matches a target resolved against the config's directory.
        path = os.path.abspath(path)
        for target, glob in self.ignored:
            if glob:
                if fnmatch.fnmatch(path, target):
                    return True
            elif path == target or path.startswith(target + os.sep):
                return True
        return False

    def module_ignored(self, module_key: str) -> bool:
        return self.is_ignored(os.path.join(self.root or ".", module_key))

    def module_selected(self, rel: str) -> bool:
        """Whether this run executes tests from `rel`. Only test *modules* are ever skipped: every
        conftest in the tree is still imported, exactly as pytest imports every conftest at
        collection whatever it later deselects — a conftest can carry a side effect the rest of the
        suite relies on, and pruning the directories without selected modules cost 50 tests their
        isolation on pirn-agents before this was understood."""
        return self.modules is None or rel in self.modules

    def keyword_names(self, node_id: str, marks) -> list:
        """What pytest's `-k` matches against: the names its path gives the node (TID-100), every
        `::` segment — class, function, the function with its parametrize id — and the node's mark
        names."""
        parts = node_id.split("::")
        return [*path_names(parts[0], self.abs_root, self.project.dir), *parts[1:], *sorted(marks)]
