"""What discovery produced (TID-124, step 2): the registry, and everything the walk learned on the
way to it — the conftests and the directory each governs, the options and ini values they
declared, the skips the collection hooks decided, the directories a conftest skipped or broke —
as one [`Discovery`] the engine holds.

Eight module globals used to carry this (`_CLI_OPTIONS`, `_INI_DECLARED`, `_MARKER_SKIPS`,
`_ANCESTOR_CONFTESTS`, `_CONFTEST_SCOPES`, `_DIR_SKIPS`, `_DIR_ERRORS`, `_HOOK_MARKS`), two of
them declared after their first use, filled by `discover` and its helpers and read back by the
gate, `request.config` and the `pytest_generate_tests` driver.
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Any

from .safe import safe_getattr
import importlib.util
import inspect
import sys
import time
import traceback
import unittest

from .config import _env, _env_flag, NOTSET as _NOTSET, ProjectConfig, RunConfig
from .fixtures import (_fixture_def, _is_fixture, _is_native_provider, _native_fixture_def,
                       FixtureDef, Registry)
from .invoke import SKIP_EXCEPTIONS as _SKIP_EXCEPTIONS
from .log import warn as _warn
from .nodes import import_module as _import_module
from .pytest_compat import (_HookItem, _own_markers, normalise_all as _normalise_marks,
                            skip_reason as _mark_skip_reason)
from .safe import safe_getattr as _safe_getattr
from .selection import plugin_marks as _plugin_marks, registered_marks as _registered_marks


def dir_mark(marks: dict, rel_path: str) -> str | None:
    """The mark (a skip reason, a conftest's import failure) covering `rel_path` — a suite-relative
    file or directory — from the nearest ancestor directory that carries one; `""` covers all."""
    if not marks:
        return None
    if "" in marks:
        return marks[""]
    parts = rel_path.split("/")
    for depth in range(len(parts), 0, -1):
        mark = marks.get("/".join(parts[:depth]))
        if mark is not None:
            return mark
    return None


@dataclass
class Discovery:
    """What one discovery produced. `registry` is the fixture registry it built; the rest is what
    the walk recorded, keyed the way the gate asks for it."""

    registry: Any  # the `Registry` — every fixture and provider, by name and by type
    # The conftests above the run root, `(module, location)` shallowest first (TID-19). Executed
    # once: a conftest's whole job is side effects, and running it twice would apply them twice.
    # `serve()` imports them before `preimport`; `discover` finds them here.
    ancestors: list | None = None
    # Every conftest imported, with the directory it governs: `""`/`"."` for the run root, a
    # `..`-relative location for an ancestor (which governs everything), else a root-relative
    # directory (TID-85).
    conftest_scopes: list = field(default_factory=list)
    # Command-line options conftests declared via `pytest_addoption`, as `dest -> default` (TID-14).
    # Only defaults: tiderace has no way to *pass* a custom flag yet (TID-17), so a declared option
    # always reads as its default — which is what an opt-in guard like
    # `if not request.config.getoption("--real"): pytest.skip(...)` needs to resolve correctly.
    cli_options: dict = field(default_factory=dict)
    # `parser.addini(name, help, type, default)` declarations, as `name -> (type, default)` (TID-87).
    # A value the project's config sets wins over the declared default; `getini` of a name nobody
    # declared is `None`.
    ini_declared: dict = field(default_factory=dict)
    # Node ids a collection hook (or a direct `@pytest.mark.skip`) decided to skip, as
    # `node_id -> reason` (TID-20). Computed once, consulted per node by the gate.
    marker_skips: dict = field(default_factory=dict)
    # Suite-relative dir (`""` = everything) -> why its conftest skipped it (TID-48).
    dir_skips: dict = field(default_factory=dict)
    # Suite-relative dir -> the traceback of its conftest's failed import (TID-72). pytest stops
    # at collection with one error and runs nothing; every test under that conftest is reported
    # with the conftest's own traceback, which is the same verdict per test.
    dir_errors: dict = field(default_factory=dict)
    # Node id -> the parametrize marks its `pytest_generate_tests` hooks produced (TID-85), filled
    # as nodes are planned: hooks are deterministic and `_cases` / `_indirect` both ask.
    hook_marks: dict = field(default_factory=dict)

    def dir_skip(self, rel_path: str) -> str | None:
        """The skip reason covering `rel_path`, if a conftest skipped it."""
        return dir_mark(self.dir_skips, rel_path)

    def dir_error(self, rel_path: str) -> str | None:
        """The conftest import failure covering `rel_path`, if one of its conftests did not import."""
        return dir_mark(self.dir_errors, rel_path)

    @property
    def generate_tests_hooks(self) -> bool:
        """Whether any conftest declares `pytest_generate_tests` — a suite without the hook pays a
        dictionary lookup per node, nothing more."""
        return any(safe_getattr(m, "pytest_generate_tests", None) is not None
                   for _, m in self.conftest_scopes)

    def conftests_governing(self, module_key: str) -> list:
        """The conftest modules whose directory holds `module_key`, deepest first — pytest's
        calling order for their hooks (a later-registered plugin is called first)."""
        module_dir = module_key.rsplit("/", 1)[0] if "/" in module_key else ""
        governing = []
        for location, module in self.conftest_scopes:
            loc = "" if location in (".", "") else location.replace(os.sep, "/")
            if loc.startswith(".."):
                depth = -1  # an ancestor: governs everything, called after every in-tree conftest
            elif loc == "" or module_dir == loc or module_dir.startswith(loc + "/"):
                depth = loc.count("/") + 1 if loc else 0
            else:
                continue
            governing.append((depth, module))
        governing.sort(key=lambda d: -d[0])
        return [m for _, m in governing]


class _PhaseTimer:
    """Start-up phase timings to stderr under `TIDERACE_TIMING=1`; silent otherwise. Counts the
    test modules discovery did not import — the ones `--modules` left out (TID-75)."""

    def __init__(self) -> None:
        self.on = _env_flag("TIDERACE_TIMING")
        self.last = time.perf_counter()
        self.unselected = 0

    def mark(self, label: str) -> None:
        if not self.on:
            return
        now = time.perf_counter()
        _warn(f"start-up: {label}: {now - self.last:.2f}s")
        self.last = now


def _package_basedir(directory: str) -> str:
    """The first ancestor of `directory` that is not itself a package.

    pytest's basedir rule, and the only directory that belongs on `sys.path`: a *package* directory
    placed there makes its own subpackages importable as bare top-level names."""
    cur = os.path.abspath(directory)
    while os.path.exists(os.path.join(cur, "__init__.py")):
        parent = os.path.dirname(cur)
        if parent == cur:  # filesystem root; nothing further to walk to
            break
        cur = parent
    return cur


def _insert_run_root(root: str) -> None:
    """Put the run root's **basedir** on `sys.path`, and warn if it still shadows the stdlib (TID-37).

    The run root goes on `sys.path[0]`, so any directory or module inside it named like a standard
    library module wins over the real one. A suite laid out as `tests/` containing `tests/types/` or
    `tests/statistics/` captures `types` / `statistics` for the whole run. Every module that touches
    one — directly or transitively — breaks, and it surfaces as ordinary test errors with nothing
    pointing at the cause. On one real suite that was 73 of them.

    It bites unevenly, which is what makes it so hard to see: a stdlib module already in
    `sys.modules` when the path is poisoned (`types`, `json`, `os`) keeps working, because nothing
    re-resolves it. Only names first imported *during* the run are captured, so the same defect
    looks like a batch-size or ordering effect rather than a naming one.

    Inserting the basedir instead of the root only works because `discover` now names test modules
    through `_module_name`, which walks to the same place. While the two disagreed, the run root had
    to stay importable and this could not be fixed.

    The warning stays for the case the walk cannot help with: a basedir that is *not* a package but
    still holds a directory named like a stdlib module. pytest has the same exposure there, and
    naming it beats letting it surface as unrelated errors later."""
    basedir = _package_basedir(root)
    # First, not merely present (TID-48). A monorepo venv's editable-install `.pth` files already put
    # every package directory on `sys.path` — each holding its own `tests` package — so the basedir
    # is usually there, just behind a sibling. Leaving it where it was meant `tests.unit` resolved
    # against pirn-agents' `tests` while running pirn-core: 4,855 of 4,855 tests errored.
    # `python -m pytest` hides this, because `-m` puts the cwd ahead of every `.pth` entry.
    while basedir in sys.path:
        sys.path.remove(basedir)
    sys.path.insert(0, basedir)
    try:
        entries = os.listdir(basedir)
    except OSError:
        return
    names = {e[:-3] if e.endswith(".py") else e for e in entries}
    shadowed = sorted(names & sys.stdlib_module_names)
    if shadowed:
        _warn(
            f"{basedir} is on sys.path and contains "
            f"{', '.join(shadowed)}, which shadow standard-library modules of the same name; "
            f"imports of those will resolve here, not to the stdlib"
        )


def _ini_value(project: ProjectConfig, ini_declared: dict, name: str):
    """`config.getini(name)`: the project's configured value if set, else the declared default
    (`ini_declared`, what the conftests' `addini` calls recorded), else `None`. Typed the way
    pytest types it — `bool` parses, list types split."""
    declared = ini_declared.get(name)
    ini_type = declared[0] if declared else None
    values = project.values(name)
    if values:
        if ini_type == "bool":
            text = str(values[0]).strip().lower()
            return text in ("1", "true", "yes", "on")
        if ini_type in ("linelist", "args", "paths", "pathlist"):
            return [str(v) for v in values]
        return values[0] if len(values) == 1 else values
    if declared is None:
        return None
    default = declared[1]
    if default is not _NOTSET:
        return default
    return {"bool": False, "linelist": [], "args": [], "paths": [], "pathlist": []}.get(ini_type, "")


class _OptionRecorder:
    """Stands in for pytest's argument parser while a conftest's `pytest_addoption` hook runs.

    The hook expects to be handed a parser and to call `addoption` on it (or on a group). Rather
    than model argparse, record just what `getoption` needs: the destination name and the default
    the option would have carried."""

    def __init__(self, options: dict, ini: dict):
        self._options = options
        self._ini = ini

    def addoption(self, *names, **kw) -> None:
        dest = kw.get("dest")
        if dest is None:
            flag = next((n for n in names if n.startswith("--")), names[0] if names else None)
            if flag is None:
                return
            dest = flag.lstrip("-").replace("-", "_")
        if "default" in kw:
            default = kw["default"]
        else:  # mirror argparse's implicit defaults for the actions conftests actually use
            action = kw.get("action")
            default = {"store_true": False, "store_false": True, "count": 0, "append": []}.get(action)
        self._options[dest] = default

    _addoption = addoption  # the private spelling plugins use on a group (xdist)

    def getgroup(self, *_a, **_kw):
        return self  # groups expose the same `addoption`, so the recorder can be its own group

    def addini(self, name, help=None, type=None, default=_NOTSET, **_kw) -> None:  # noqa: A002
        self._ini[name] = (type, default)  # read back through `config.getini` (TID-87)


def _collect_addoption(module, disc: Discovery) -> None:
    """Run a conftest's `pytest_addoption` hook against the recorder, if it has one."""
    hook = getattr(module, "pytest_addoption", None)
    if hook is None:
        return
    try:
        hook(_OptionRecorder(disc.cli_options, disc.ini_declared))
    except Exception as exc:  # noqa: BLE001 — a hook we can't model must not abort discovery
        _warn(f"pytest_addoption in {getattr(module, '__file__', '?')} "
              f"could not be recorded: {exc!r}")


class _Config:
    """The slice of pytest's `config` that tests reach for through `request.config` — one per run,
    built by the engine (and by discovery, for the collection hooks) from the run's `RunConfig`
    and what discovery recorded: the options and ini values the conftests declared."""

    __slots__ = ("run", "discovery")

    def __init__(self, run: RunConfig, discovery: Discovery) -> None:
        self.run = run
        self.discovery = discovery

    def getoption(self, name: str, default=_NOTSET, skip: bool = False):
        key = name.lstrip("-").replace("-", "_")
        if key in self.discovery.cli_options:
            value = self.discovery.cli_options[key]
        elif default is not _NOTSET:
            value = default
        else:
            # pytest raises for an option nobody declared; matching that beats inventing a value,
            # which would silently flip an opt-in guard the wrong way.
            raise ValueError(f"no option named {name!r}")
        if skip and value is None:
            raise _SKIP_EXCEPTIONS[0](f"no value for option {name!r}")
        return value

    def getini(self, name: str):
        return _ini_value(self.run.project, self.discovery.ini_declared, name)


def _enumerate_items(test_modules: list) -> list:
    """The collected items, for the collection hooks to inspect (TID-20).

    Mirrors `RegexCollector`'s rules by **introspection** rather than by re-scanning source: module
    functions named `test*`, and methods named `test*` on `unittest.TestCase` subclasses (any name)
    or `Test*` classes.

    The Rust collector remains authoritative for what actually *runs*; this list only feeds the
    hooks. So a divergence can cost a skip that should have been applied, never a test that should
    not have run."""
    items = []
    for module, rel in test_modules:
        module_marks = _own_markers(module)
        for name, obj in vars(module).items():
            if name.startswith("test") and inspect.isfunction(obj):
                items.append(_HookItem(f"{rel}::{name}", name, module_marks + _own_markers(obj)))
            elif inspect.isclass(obj) and (
                name.startswith("Test") or issubclass(obj, unittest.TestCase)
            ):
                class_marks = module_marks + _own_markers(obj)
                for mname, meth in vars(obj).items():
                    if mname.startswith("test") and callable(meth):
                        items.append(
                            _HookItem(
                                f"{rel}::{name}::{mname}",
                                mname,
                                class_marks + _own_markers(meth),
                            )
                        )
    return items


def _run_collection_hooks(conftests: list, test_modules: list, config: _Config) -> dict:
    """Run every conftest's `pytest_collection_modifyitems`, then return the skips it produced —
    `node_id -> reason` (TID-20), computed once and consulted per node by the gate.

    Suites gate optional backends here — `needs_postgres`, `needs_kuzu` — so without it those tests
    run anyway and die on a missing import. pytest reports them as skips; tiderace reported a red run
    for a dependency the suite deliberately made optional.

    Marks applied *directly* (`@pytest.mark.skip`) are folded into the same pass, so there is one
    place that decides a marker-driven skip rather than two that can disagree."""
    hooks = [
        (m, getattr(m, "pytest_collection_modifyitems", None))
        for m in conftests
    ]
    hooks = [(m, h) for m, h in hooks if h is not None]

    items = _enumerate_items(test_modules)
    for module, hook in hooks:
        try:
            hook(config=config, items=items)
        except TypeError:
            # Hooks may declare any subset of (session, config, items) — pytest matches by name.
            try:
                hook(config, items)
            except Exception as exc:  # noqa: BLE001
                _warn_hook_failed(module, exc)
        except Exception as exc:  # noqa: BLE001 — a hook we can't run must not abort discovery
            _warn_hook_failed(module, exc)

    skips: dict = {}
    for item in items:
        reason = _mark_skip_reason(_normalise_marks(item.iter_markers()))
        if reason is not None:
            skips[item.nodeid] = reason
    return skips


def _warn_hook_failed(module, exc: BaseException) -> None:
    _warn(f"pytest_collection_modifyitems in "
          f"{getattr(module, '__file__', '?')} failed: {exc!r} — its skips will not be applied")


# Files that mark a project root, in pytest's rootdir sense. The nearest ancestor holding one bounds
# how far up `conftest.py` collection reaches (pytest's confcutdir defaults to rootdir).
# `pytest.ini` is FIRST because it is first in pytest's own precedence — an explicit pytest config
# is the strongest statement about where a project's root is. Omitting it meant a suite laid out the
# conventional way, with `pytest.ini` and a suite-wide `conftest.py` above the test directory, found
# no rootdir at all: the ancestor walk stopped immediately and every session fixture in that conftest
# silently did not exist. That is how the repo's own `fx_corpus` became unrunnable (TID-34).
_ROOTDIR_MARKERS = ("pytest.ini", "pyproject.toml", "setup.cfg", "tox.ini", "setup.py")


def _rootdir(root: str) -> str | None:
    """The nearest ancestor of `root` holding a project marker, or None if there is none.

    This is the ceiling for ancestor-conftest collection. Returning None when nothing is found keeps
    a rootless tree behaving exactly as it did before ancestor collection existed, rather than
    walking to `/` and importing whatever happens to be up there."""
    cur = os.path.abspath(root)
    while True:
        parent = os.path.dirname(cur)
        if parent == cur:  # hit the filesystem root without finding a marker
            return None
        if any(os.path.exists(os.path.join(parent, m)) for m in _ROOTDIR_MARKERS):
            return parent
        cur = parent


def _load_ancestor_conftests(root: str, disc: Discovery) -> list:
    """Import every `conftest.py` between rootdir and the run root, shallowest first (TID-19).

    `os.walk(root)` only ever sees the tree at or below the run root, so a `conftest.py` beside
    `pyproject.toml` — the conventional home for suite-wide setup — was silently skipped. pytest
    collects conftests from rootdir down, and suites rely on it: env defaults, warning filters,
    `sys.path` surgery, plugin registration. Skipping it does not degrade gracefully; it surfaces
    later as a failure whose stated cause points nowhere near conftest discovery.

    Returns `[(module, location)]` where location is a `..`-relative dir (see `_location_depth`),
    and records it on `disc.ancestors`: they must be *executed once* — a conftest's whole job is
    side effects (env defaults, warning filters, sys.path surgery), and running it twice would
    apply them twice. `serve()` imports them before `preimport`; `discover` finds them there."""
    key = os.path.abspath(root)
    out: list = []
    ceiling = _rootdir(root)
    if ceiling is not None:
        # rootdir → run root, shallowest first, so a nearer conftest's side effects win by running last.
        chain, cur = [], key
        while cur != ceiling and os.path.dirname(cur) != cur:
            cur = os.path.dirname(cur)
            chain.append(cur)
            if cur == ceiling:
                break
        for directory in reversed(chain):
            path = os.path.join(directory, "conftest.py")
            if not os.path.exists(path):
                continue
            location = os.path.relpath(directory, key).replace(os.sep, "/")
            module = _import_conftest(path, location, disc)
            if module is not None:
                _collect_addoption(module, disc)
                out.append((module, location))

    disc.ancestors = out
    return out


# Directories never descended into during discovery. Mirrors `RegexCollector::SKIP_DIRS` — the two
# walks must agree, or the shim reports fixtures for files collection never saw (and vice versa).
_SKIP_DIRS = frozenset({
    "__pycache__", ".git", ".venv", "venv", ".tox", ".nox", "site-packages",
    ".tiderace-spike-venv", ".tiderace-bench-venv", ".tiderace-fx-venv", ".tiderace-cache",
    ".pytest_cache", "node_modules", ".mypy_cache", ".ruff_cache",
})


def _walk_suite(root: str):
    """`os.walk` over the suite, skipping what is not part of it, in a deterministic order.

    Two things this gets right that the obvious spelling does not.

    **The prune has to happen during the walk.** `sorted(os.walk(root))` reads the *entire* tree
    before yielding anything, so assigning to `dirs` afterwards prunes nothing — the traversal has
    already been to every directory. Sorting `dirs` in place instead gives the same deterministic
    order and lets the prune actually take effect.

    **`.venv` is not part of the suite.** Without a skip list the walk finds and imports the
    *dependencies'* test suites: numpy ships its own, and `.venv/…/numpy/conftest.py` was imported on
    every run of any project with numpy installed. Slow, wrong, and a source of foreign collection
    state — a `pytestmark` from someone else's suite reaching our marker handling is how the scalar
    `pytestmark` crash above was found."""
    for current, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in _SKIP_DIRS)
        yield current, dirs, files


def discover(run: RunConfig, disc: Discovery | None = None, *,
              timer: _PhaseTimer | None = None) -> Discovery:
    """What the run's suite defines: the registry — every fixture and provider its conftests and
    selected test modules define, the builtins' and the plugins' — with the collection hooks run
    and the conftests, their options, and the per-directory skips recorded on the way, as one
    `Discovery`. `disc` is the one `serve()` already imported the ancestor conftests into; `timer`
    counts the test modules `--modules` left out."""
    disc = disc if disc is not None else Discovery(Registry())
    reg = disc.registry
    root, project = run.root, run.project
    native: list[tuple] = []  # (provider obj, location) — resolved in a second pass (see below)
    conftests: list = []  # every conftest module, for the collection hooks (TID-20)
    test_modules: list = []  # (module, rel path) — the items those hooks inspect
    # Ancestor conftests first: their fixtures are the widest in the tree, and `serve()` has already
    # executed them ahead of `preimport` so their side effects precede every test-module import.
    ancestors = disc.ancestors if disc.ancestors is not None else _load_ancestor_conftests(root, disc)
    for module, location in ancestors:
        conftests.append(module)
        disc.conftest_scopes.append((location, module))
        for attr, obj in list(vars(module).items()):
            if _is_native_provider(obj):
                native.append((obj, location))
            elif _is_fixture(obj):
                reg.add(_fixture_def(obj, location, attr_name=attr))
    for current, dirs, files in _walk_suite(root):
        rel_dir = os.path.relpath(current, root)
        rel_dir = "" if rel_dir == "." else rel_dir.replace(os.sep, "/")
        if disc.dir_skip(rel_dir) is not None:
            dirs[:] = []  # a skipped conftest's subtree is not collected at all, as in pytest
            continue
        if run.is_ignored(current):
            dirs[:] = []
            continue
        # The directory's conftest before its test modules: it may skip the directory, and `sorted`
        # alone would put `a_test.py` ahead of `conftest.py`.
        for name in sorted(files, key=lambda n: (n != "conftest.py", n)):
            if disc.dir_skip(rel_dir) is not None:
                dirs[:] = []
                break
            if not name.endswith(".py"):
                continue
            path = os.path.join(current, name)
            if name == "conftest.py":
                module, location = _import_conftest(path, rel_dir, disc), rel_dir
                if module is not None:
                    _collect_addoption(module, disc)
                    conftests.append(module)
                    disc.conftest_scopes.append((location, module))
            elif name.startswith("test_") or name.endswith("_test.py"):
                # Named through `_module_name`, exactly as execution names it (TID-37). The old
                # spelling was relative to the run *root*, which forced the run root itself onto
                # `sys.path` — and a run root that is a package is what shadowed the stdlib. It was
                # also a latent double-import: when the two spellings disagreed, the same file was
                # imported twice under two names, so a module-level fixture could register against
                # one copy while the test ran against the other.
                location = os.path.relpath(path, root).replace(os.sep, "/")
                if not run.module_selected(location):
                    if timer is not None:
                        timer.unselected += 1
                    continue  # this run will not execute it (TID-75)
                try:
                    module = _import_module(location, root)
                except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — surfaces per-test, not at discovery
                    continue
                test_modules.append((module, location))
            else:
                continue
            if module is None:
                continue
            for attr, obj in list(vars(module).items()):
                if _is_native_provider(obj):  # native-first (ADR-E012); pytest is compat fallback
                    native.append((obj, location))
                elif _is_fixture(obj):
                    reg.add(_fixture_def(obj, location, attr_name=attr))
                elif isinstance(obj, type):
                    # Fixtures defined inside a test class. pytest scopes these to the class, where
                    # they commonly *override* a conftest fixture of the same name for that class
                    # alone — flask's `TestRoutes.app` is exactly that — so they are registered with
                    # a `module.py::Class` location rather than merged into the module (TID-47).
                    _register_class_fixtures(reg, obj, location)

    # After every conftest is loaded and every test module imported — the hooks need both, and the
    # marks they inspect only exist once the decorators have run.
    disc.marker_skips = _run_collection_hooks(conftests, test_modules, _Config(run, disc))

    # `--strict-markers` (TID-59): ask pytest for the plugins' marks *now*, in the process every
    # worker is forked from. The answer was fetched lazily by the first strict node each worker
    # met, a 0.5s subprocess per worker per run — the largest single cost of a `-k` run on
    # pirn-core (TID-91). The selection itself is the engine's (`Selection.load`), read once every
    # conftest has imported: a native `tiderace.mark.register` in one of them counts as declared.
    if _registered_marks(project)[1]:
        _plugin_marks(root)

    # Native providers wire by type, so provider→provider deps need the FULL type set first: build the
    # type index, then build the defs (a two-pass the name-DI pytest path doesn't need).
    type_index: dict = {}
    for obj, _loc in native:
        spec = obj.__tiderace_provider__
        type_index.setdefault(spec.provides, []).append(spec.name)
    for obj, location in native:
        reg.add(_native_fixture_def(obj, location, type_index))
    _register_builtins(run, disc)
    # Last, so everything above — a conftest at any depth, the builtins, the native anyio_backend —
    # takes precedence over a plugin's fixture of the same name, as in pytest (TID-87).
    _register_plugin_fixtures(disc, project, conftests)
    return disc


# pytest's own plugins are what the shim replaces; their fixtures come from `tiderace.builtins`.
_PYTEST_OWN_PLUGINS = ("pytester", "_pytest", "pytest")


def _plugin_modules(project: ProjectConfig, conftests: list) -> list:
    """`(plugin name, module name)` for every pytest plugin the project would load (TID-87):
    the `pytest11` entry points of the installed distributions, `-p NAME` in `addopts`, and each
    conftest's `pytest_plugins` — minus `-p no:NAME`, minus pytest's own, and subject to
    `TIDERACE_PLUGINS` (`none`, or a comma-separated allow-list) or `[tool.tiderace] plugins`.
    `PYTEST_DISABLE_PLUGIN_AUTOLOAD` turns the entry points off, as it does for pytest; the
    explicit spellings still load."""
    allow = _env("TIDERACE_PLUGINS")
    if allow is None:
        configured = project.setting("plugins")
        if isinstance(configured, (list, tuple)):
            allow = ",".join(str(v) for v in configured) or "none"  # `plugins = []`: none at all
        elif isinstance(configured, str):
            allow = configured
    if allow is not None and allow.strip().lower() in ("none", ""):
        return []
    allowed = {n.strip() for n in allow.split(",") if n.strip()} if allow is not None else None
    explicit: list = []
    disabled: set = set()
    for value in project.opt_values("-p"):
        if value.startswith("no:"):
            disabled.add(value[3:])
        else:
            explicit.append(value)
    found: list = []
    seen: set = set()

    def take(name: str, module: str) -> None:
        if name in disabled or module in disabled or module in seen:
            return
        if any(module == own or module.startswith(own + ".") for own in _PYTEST_OWN_PLUGINS):
            return
        if allowed is not None and name not in allowed and module not in allowed:
            return
        seen.add(module)
        found.append((name, module))

    if not os.environ.get("PYTEST_DISABLE_PLUGIN_AUTOLOAD"):
        try:
            from importlib.metadata import entry_points
            eps = list(entry_points(group="pytest11"))
        except Exception:  # noqa: BLE001 — no metadata machinery: no entry points
            eps = []
        for ep in sorted(eps, key=lambda e: e.name):
            take(ep.name, ep.value.split(":", 1)[0].strip())
    for value in explicit:
        take(value, value)
    for module in conftests:
        declared = _safe_getattr(module, "pytest_plugins", None)
        if isinstance(declared, str):
            declared = [declared]
        for value in declared or ():
            if isinstance(value, str):
                take(value, value)
    return found


def _register_plugin_fixtures(disc: Discovery, project: ProjectConfig, conftests: list) -> None:
    """Import each plugin module and register the fixtures it defines at the root location, after
    everything else (TID-87): a suite's own fixture of the same name — a conftest at any depth, a
    test module's — already outranks it, and a name the shim itself provides (a builtin, the native
    `anyio_backend`) is left alone. Only fixtures are taken; the plugin's hooks are never called,
    except `pytest_addoption`, which is recorded exactly as a conftest's is (TID-14) so its
    options and ini defaults read back through `config`."""
    reg = disc.registry
    for name, module_name in _plugin_modules(project, conftests):
        try:
            module = importlib.import_module(module_name)
        except (Exception, *_SKIP_EXCEPTIONS) as exc:  # noqa: BLE001 — one plugin, not the run
            _warn(f"pytest plugin {name!r} ({module_name}) not loaded: {exc!r}")
            continue
        _collect_addoption(module, disc)
        for attr, obj in list(vars(module).items()):
            if not _is_fixture(obj):
                continue
            fdef = _fixture_def(obj, "", attr_name=attr)
            if any(d.location == "" for d in reg.by_name.get(fdef.name, ())):
                continue  # the shim's own, or a root conftest's: theirs wins
            reg.add(fdef)


def _register_class_fixtures(reg: Registry, cls: type, module_key: str) -> None:
    """Register every fixture a test class defines, including ones it inherits.

    pytest collects a class's fixtures from its whole MRO, so a base class holding shared fixtures
    works. Each def is filed under the class it is *looked up from*, which is what makes an override
    in a subclass beat its base."""
    names = _safe_getattr(cls, "__mro__", None) or ()
    for base in names:
        if base is object:
            continue
        for attr, obj in list(vars(base).items()):
            if not _is_fixture(obj):
                continue
            fdef = _fixture_def(obj, f"{module_key}::{cls.__name__}", owner=cls, attr_name=attr)
            # A subclass that redefines the name has already registered its own def for this class;
            # the base's copy would be an identical location and must not shadow it.
            if any(d.location == fdef.location for d in reg.by_name.get(fdef.name, ())):
                continue
            reg.add(fdef)


class _BuiltinsContext:
    """What the builtins see of the run — the `RunContext` the authoring package defines (TID-111):
    the root, the options conftests declared, the ini values."""

    __slots__ = ("run", "discovery")

    def __init__(self, run: RunConfig, discovery: Discovery) -> None:
        self.run = run
        self.discovery = discovery

    @property
    def rootdir(self) -> str:
        return self.run.root or os.getcwd()

    @property
    def options(self) -> dict:
        return dict(self.discovery.cli_options)

    def ini(self, name: str):
        return _ini_value(self.run.project, self.discovery.ini_declared, name)


def _register_builtins(run: RunConfig, disc: Discovery) -> None:
    """Register tiderace's always-available builtin resources (ROADMAP-v2 B1: monkeypatch/tmp_path/
    capsys/capfd/caplog) at the root location (""), so every test can request them — by type (the
    migrated form, `mp: MonkeyPatch`) or by name (the pytest form, `monkeypatch`), with no per-tree
    import.

    Staying best-effort is deliberate: a pure-pytest suite driven by a bare interpreter has no
    `tiderace` installed and must still run. But the failure is now **announced** (TID-21). Silence
    here meant every builtin was quietly missing while the suite stayed green, which is how the CI
    fixture venv went a long time with no builtin coverage at all and how `tmp_path` sat recorded as
    36 open errors months after it worked."""
    reg = disc.registry
    try:
        import tiderace.builtins as builtins_pkg
    except Exception as exc:  # noqa: BLE001 — tiderace not importable ⇒ no builtins
        _warn(f"builtin providers unavailable ({exc!r}) — monkeypatch/tmp_path/capsys/"
              f"capfd/caplog will not resolve. Install `tiderace` into this interpreter, or put "
              f"engine/py-tiderace on PYTHONPATH.")
        return
    # The builtins read the run root, the declared options and the ini values through one
    # accessor (TID-111); hand them the run. Per interpreter: a sub-interpreter registers its own.
    builtins_pkg.set_context(_BuiltinsContext(run, disc))
    for obj in builtins_pkg.providers():
        reg.add(_native_fixture_def(obj, "", {}))
    _register_anyio_backend(reg)


def _register_anyio_backend(reg: Registry) -> None:
    """Provide `anyio_backend` when anyio is installed, as anyio's own plugin would.

    The fixture a `@pytest.mark.anyio` test runs against is not declared by the suite — anyio's plugin
    declares it, parametrised over the backends that are actually installed:

        @pytest.fixture(scope="module", params=get_available_backends())
        def anyio_backend(request): return request.param

    Tiderace hosts no plugins, so nothing supplied it and every marked test ran **once**, on the
    default loop, where its author asked for one run per backend. The tests passed, which is what
    made it dangerous: half the intended coverage was missing and nothing said so (TID-54).

    Registered at the root location, so a suite that declares its own `anyio_backend` — anyio's test
    suite does, to add uvloop — still overrides this one by ordinary nearest-wins resolution."""
    try:
        import anyio  # noqa: F401 — presence is the question
    except Exception:  # noqa: BLE001 — no anyio, nothing to provide
        return
    backends: list = []
    try:
        from anyio.pytest_plugin import get_available_backends

        backends = list(get_available_backends())
    except Exception:  # noqa: BLE001 — older or restructured anyio: work it out directly
        backends = ["asyncio"]
        try:
            import trio  # noqa: F401

            backends.append("trio")
        except Exception:  # noqa: BLE001
            pass
    if not backends:
        return

    def anyio_backend(request):
        return request.param

    reg.add(FixtureDef(name="anyio_backend", scope="module", params=backends, autouse=False,
                       func=anyio_backend, location=""))


def _skip_reason(exc: BaseException) -> str:
    return str(getattr(exc, "msg", None) or exc) or type(exc).__name__


def _import_conftest(path: str, rel_dir: str, disc: Discovery):
    # Ancestor dirs (TID-19) arrive as `..`, `../..`, … — dotted, non-identifier, and indistinguishable
    # from each other once punctuation is stripped. Name them by how far up they sit instead.
    suffix = f"up{len(rel_dir.split('/'))}" if rel_dir.startswith("..") else rel_dir.replace("/", "_")
    mod_name = "_fx_conftest_" + (suffix or "root")
    try:
        spec = importlib.util.spec_from_file_location(mod_name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[mod_name] = module
        spec.loader.exec_module(module)
        return module
    except _SKIP_EXCEPTIONS as exc:
        # `pytest.importorskip("ray")` at the top of a conftest skips that directory — pytest collects
        # nothing below it (TID-48). `Skipped` is a BaseException, so it used to sail past the handler
        # below and kill the shim during discovery: under the shared-import pool that was the pool
        # parent, and the entire run failed before a single test started.
        disc.dir_skips["" if rel_dir.startswith("..") else rel_dir] = _skip_reason(exc)
        return None
    except Exception as exc:  # noqa: BLE001 — a broken conftest is every test under it, not the run
        # A conftest that fails to import takes its fixtures and its side effects with it. The tests
        # below it used to run anyway and mostly pass — 508 of 511 on fx_corpus with a conftest that
        # raised on import — while the few that needed a fixture failed naming the fixture rather than
        # the cause (TID-72). pytest stops at collection with the conftest's error and runs nothing;
        # the per-test equivalent is every test under that conftest erroring with that traceback,
        # which `run()` reports the way it reports a conftest-level skip (TID-48).
        _warn(f"could not import {path}: {exc!r}")
        disc.dir_errors["" if rel_dir.startswith("..") else rel_dir] = (
            f"conftest {path} failed to import:\n"
            + "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)))
        return None


def preimport(run: RunConfig) -> None:
    root = run.root
    for current, _dirs, files in _walk_suite(root):  # never warm a dependency's own suite
        for name in files:
            if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
                if run.is_ignored(os.path.join(current, name)):
                    continue
                rel = os.path.relpath(os.path.join(current, name), root).replace(os.sep, "/")
                if not run.module_selected(rel):
                    continue  # this run will not execute it (TID-75)
                try:
                    # Named as `discover` and execution name it (TID-37); a module-level
                    # `importorskip` is a skip, not a reason to take the pool parent down (TID-48).
                    _import_module(rel, root)
                except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001
                    pass
