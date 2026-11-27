"""Estuary Trace Format output (TID-128): `tiderace run --etf <path>`.

estuary reuses a test's result on any snapshot where everything the test read is unchanged, so
it needs, per test, the complete set of `(key, value)` reads — a *trace*. This module turns a
finished run into one ETF file: a record per test node, each pointing at one root fragment whose
flattening is the test's trace.

Two steps, kept apart on purpose:

1. **`build`** names what each test read and factors the names into shared fragments. It needs
   nothing but this package, so it is tested everywhere. A harness reports key *names* (estuary
   design 06, D-602); this is that half.
2. **`write`** computes a value for every key from the tree on disk and writes the file through
   the `estuary_etf` binding — the one codec, so harness and store cannot drift. Under
   `estuary-runner` the runner computes the values instead; standalone, nobody else can.

What a trace is made of is a list of **sources** (`SOURCES`). Each gives a test the shared
fragments it reads through; whatever the test's own footprint holds beyond them is its residue,
the root fragment's own pairs:

    root(test) ── toolchain          tool:python · tool:pyenv · tool:tiderace
               ├─ config             one key per config section the run reads
               ├─ conftests(dir)     the conftest chain, repository root → the test's directory
               ├─ imports(module)    the test module's import closure, a DAG that mirrors the
               │                     import graph, so two modules importing one package share it
               └─ own pairs          files the test executed code in that nothing above names

Keys are file-granular (`file:<repo-relative path>`). Not here yet, each one more source or one
more kind in a test's `reads`: negative import probes, environment reads and data-file reads
(TID-129), unit keys (TID-130), and the carry-over chain for tests that ran in one process
(estuary design 03, D-306).
"""
from __future__ import annotations

import dataclasses
import json
import os
import sys
import time

from .config import CONFIG_FILES, _read as _read_config, load_project_config
from .footprint import Caches, _file_deps
from .log import warn as _warn

MANIFEST_VERSION = 1
# What `--etf` prints, and exits 3 with, when the binding is not installed.
MISSING_BINDING = (
    "--etf writes its output through the `estuary_etf` module, which is not installed in this "
    "interpreter ({python}). Install the optional dependency — `pip install 'tiderace[estuary]'`, "
    "or the estuary-etf wheel itself — or run without --etf.")
EXIT_NO_BINDING = 3

# tiderace's outcomes in ETF's four. An expected failure that failed, and an unexpected pass under
# the non-strict default, are both runs that went the way the suite says is fine: `pass`, as the
# exit code already treats them.
OUTCOMES = {"passed": "pass", "failed": "fail", "error": "error", "skipped": "skip",
            "xfail": "pass", "xpass": "pass"}


class EtfUnavailable(RuntimeError):
    """The `estuary_etf` binding cannot be imported."""


def binding():
    """The `estuary_etf` module, imported on first use: a run without `--etf` never looks for it."""
    try:
        import estuary_etf
    except ImportError as exc:
        raise EtfUnavailable(MISSING_BINDING.format(python=sys.executable)) from exc
    return estuary_etf


# ------------------------------------------------------------------------------ the plan
@dataclasses.dataclass(frozen=True)
class Fragment:
    """A set of key names and the fragments it includes, by index into `Plan.fragments` —
    children always come first."""

    keys: frozenset  # of (kind, name)
    children: frozenset  # of int


@dataclasses.dataclass(frozen=True)
class Record:
    check: str
    outcome: str  # an ETF outcome
    root: int
    duration_us: int


@dataclasses.dataclass
class Plan:
    """What `build` hands `write`: fragments of key names, the records over them, the toolchain
    fragment the header names, and — for the kinds whose value is not a file's content — the
    bytes each key's value is the hash of."""

    fragments: list = dataclasses.field(default_factory=list)
    records: list = dataclasses.field(default_factory=list)
    inputs: dict = dataclasses.field(default_factory=dict)  # (kind, name) -> bytes
    toolchain: int = -1
    memo: dict = dataclasses.field(default_factory=dict)  # each source's own, by its name
    _index: dict = dataclasses.field(default_factory=dict)
    _flat: dict = dataclasses.field(default_factory=dict)

    def fragment(self, keys=(), children=()) -> int:
        """The index of the fragment with exactly these keys and children, added if new: equal
        content is one fragment, which is what makes it shared."""
        frag = Fragment(frozenset(keys), frozenset(children))
        index = self._index.get(frag)
        if index is None:
            index = self._index[frag] = len(self.fragments)
            self.fragments.append(frag)
        return index

    def flatten(self, index: int) -> frozenset:
        """Every key a fragment holds, its children's included. Memoised for the fragments asked
        about — the handful each record points at — not for every node of the import graph."""
        flat = self._flat.get(index)
        if flat is None:
            keys: set = set()
            seen, stack = {index}, [index]
            while stack:
                frag = self.fragments[stack.pop()]
                keys.update(frag.keys)
                for child in frag.children:
                    if child not in seen:
                        seen.add(child)
                        stack.append(child)
            flat = self._flat[index] = frozenset(keys)
        return flat

    def trace(self, record: Record) -> frozenset:
        """A record's whole trace, as key names."""
        return self.flatten(record.root)


@dataclasses.dataclass(frozen=True)
class Test:
    """One finished node, as the engine reported it. `module` is its file, repository-relative;
    `reads` its own footprint by key kind (`{"file": [...]}` today)."""

    check: str
    module: str
    outcome: str
    duration_ms: int
    reads: dict


@dataclasses.dataclass
class Run:
    """What the sources read from: the run root, the tree the keys are relative to, and the
    import-graph memo shared with the footprint code."""

    run_root: str
    trace_root: str
    harness_version: str
    caches: Caches = dataclasses.field(default_factory=Caches)
    # Directories inside the tree that belong to the toolchain, not the repository: a virtualenv
    # kept in the checkout. `tool:pyenv` covers what is installed there.
    toolchain_dirs: tuple = ()

    def rel(self, path: str) -> str | None:
        """`path` as a key name — relative to the trace root, `/`-separated — or None when it is
        outside the tree or inside the toolchain."""
        path = os.path.abspath(path)
        if path != self.trace_root and not path.startswith(self.trace_root + os.sep):
            return None
        if any(path.startswith(d + os.sep) for d in self.toolchain_dirs):
            return None
        return os.path.relpath(path, self.trace_root).replace(os.sep, "/")

    def is_key_path(self, name: str) -> bool:
        """Whether a footprint entry names a repository file: canonical, and not the toolchain's."""
        if not name or name.startswith("/") or any(part in ("", ".", "..") for part in name.split("/")):
            return False
        return self.rel(os.path.join(self.trace_root, name.replace("/", os.sep))) is not None


def _installed_under(trace_root: str) -> tuple:
    """This interpreter's prefix and site directories, where they sit inside the tree."""
    import site

    candidates = {sys.prefix, sys.exec_prefix, *site.getsitepackages()}
    try:
        candidates.add(site.getusersitepackages())
    except Exception:  # noqa: BLE001 — no user site on this build
        pass
    dirs = {os.path.abspath(c) for c in candidates if c}
    return tuple(sorted(d for d in dirs if d.startswith(trace_root + os.sep)))


# ------------------------------------------------------------------------------ the sources
def toolchain(run: Run, plan: Plan, test: Test) -> int:
    """What every test of the run ran on: the interpreter, the installed distributions, the
    harness. Site-packages is not keyed file by file — `tool:pyenv` covers it, as the sandbox
    image does under estuary — so an upgraded dependency invalidates through this fragment."""
    if plan.toolchain < 0:
        plan.inputs.update(_toolchain_descriptors(run.harness_version))
        plan.toolchain = plan.fragment(
            [("tool", "python"), ("tool", "pyenv"), ("tool", "tiderace")])
    return plan.toolchain


def _toolchain_descriptors(harness_version: str) -> dict:
    import importlib.metadata
    import platform

    dists = set()
    for dist in importlib.metadata.distributions():
        try:
            dists.add(f"{dist.metadata['Name'].lower().replace('_', '-')}=={dist.version}")
        except Exception:  # noqa: BLE001 — a distribution with no readable metadata names nothing
            pass
    python = " ".join((sys.implementation.name, platform.python_version(),
                       sys.implementation.cache_tag or "", platform.system(), platform.machine()))
    return {
        ("tool", "python"): python.encode(),
        ("tool", "pyenv"): "\n".join(sorted(dists)).encode(),
        ("tool", "tiderace"): harness_version.encode(),
    }


def config(run: Run, plan: Plan, test: Test) -> int | None:
    """The configuration the run reads (TID-132), section by section: an edit to `[project]` in
    `pyproject.toml` is not an edit to `[tool.pytest.ini_options]`, and only the second can change
    an outcome. Each section the shim's loader reads is one `derived:cfg/<file>#<section>` key
    whose value is the hash of the section's canonical JSON; a config file that could be there
    and is not is a `file:` key with nothing behind it, so adding one invalidates."""
    if "config" in plan.memo:
        return plan.memo["config"]
    keys: list = []
    rootdir = load_project_config(run.run_root).dir
    for name in CONFIG_FILES:
        path = os.path.join(rootdir, name)
        rel = run.rel(path)
        if rel is None:  # the config is above the tree the trace covers: nothing to name
            continue
        sections = _read_config(path, name) if os.path.isfile(path) else None
        if sections is None:  # absent, or unreadable: the file itself, whatever it holds
            keys.append(("file", rel))
            continue
        for label, section in zip(_section_names(name), sections[1]):
            key = ("derived", f"cfg/{rel}#{label}")
            plan.inputs[key] = json.dumps(section, sort_keys=True, separators=(",", ":"),
                                          default=str).encode()
            keys.append(key)
    plan.memo["config"] = index = plan.fragment(keys) if keys else None
    return index


def _section_names(name: str) -> tuple:
    """The sections `config._read` returns for a file, in its order."""
    if name == "pyproject.toml":
        return ("tool.pytest.ini_options", "tool.pytest", "tool.tiderace")
    return ("tool:pytest",) if name == "setup.cfg" else ("pytest",)


def conftests(run: Run, plan: Plan, test: Test) -> int:
    """The conftest chain (TID-132): every `conftest.py` from the repository root down to the
    test's directory, each with what it imports. A directory with none still contributes the key —
    with no value behind it — so that adding one invalidates the tests beneath it."""
    return _conftest_chain(run, plan, os.path.dirname(test.module))


def _conftest_chain(run: Run, plan: Plan, directory: str) -> int:
    memo = plan.memo.setdefault("conftests", {})
    if directory in memo:
        return memo[directory]
    children = [] if not directory else [_conftest_chain(run, plan, os.path.dirname(directory))]
    rel = f"{directory}/conftest.py" if directory else "conftest.py"
    path = os.path.join(run.trace_root, rel.replace("/", os.sep))
    keys = []
    if os.path.isfile(path):
        children.append(_import_fragment(run, plan, path))
    else:
        keys.append(("file", rel))
    memo[directory] = index = plan.fragment(keys, children)
    return index


def imports(run: Run, plan: Plan, test: Test) -> int:
    """The test module's import closure (TID-40, TID-133): the module and every file in the tree
    it imports, transitively. A module's imports execute once per process, for whichever test runs
    first, so no test after it *executes* them — yet all of them depend on what was imported."""
    return _import_fragment(run, plan, os.path.join(run.trace_root, test.module.replace("/", os.sep)))


def _import_fragment(run: Run, plan: Plan, path: str) -> int:
    """The fragment for `path` and everything it imports. One fragment per strongly connected
    component of the import graph — a file, or the files of an import cycle — with the components
    it imports as children: fragments form a DAG, the import graph need not."""
    memo = plan.memo.setdefault("imports", {})
    path = os.path.abspath(path)
    if path not in memo:
        _condense(run, plan, path, memo)
    return memo[path]


def _condense(run: Run, plan: Plan, start: str, memo: dict) -> None:
    """Tarjan's algorithm from `start`, iteratively (an import chain can be deeper than the
    recursion limit), over the files not condensed yet. Components complete callee-first, so a
    fragment's children exist before it does."""
    index: dict = {}
    low: dict = {}
    stack: list = []
    on_stack: set = set()
    work = [(start, iter(_deps(run, start)))]
    index[start] = low[start] = 0
    stack.append(start)
    on_stack.add(start)
    while work:
        node, deps = work[-1]
        advanced = False
        for dep in deps:
            if dep in memo:
                continue
            if dep not in index:
                index[dep] = low[dep] = len(index)
                stack.append(dep)
                on_stack.add(dep)
                work.append((dep, iter(_deps(run, dep))))
                advanced = True
                break
            if dep in on_stack:
                low[node] = min(low[node], index[dep])
        if advanced:
            continue
        work.pop()
        if work:
            parent = work[-1][0]
            low[parent] = min(low[parent], low[node])
        if low[node] == index[node]:
            component = []
            while True:
                member = stack.pop()
                on_stack.discard(member)
                component.append(member)
                if member == node:
                    break
            members = set(component)
            children = {memo[dep] for member in component for dep in _deps(run, member)
                        if dep not in members}
            keys = [("file", run.rel(member)) for member in component]
            fragment = plan.fragment(keys, children)
            for member in component:
                memo[member] = fragment


def _deps(run: Run, path: str) -> tuple:
    """The files in the tree `path` imports — the footprint code's own answer, so the trace and
    the impact analysis read one import graph."""
    return tuple(dep for dep in _file_deps(run.caches, path, run.trace_root) if run.rel(dep) is not None)


# One entry per thing a trace is made of. A new kind of read is one more entry here, or one more
# kind in a test's `reads`; `build` does not change.
SOURCES = (toolchain, config, conftests, imports)


# ------------------------------------------------------------------------------ build
def build(run: Run, tests: list) -> Plan:
    """Every test's trace as fragments of key names: the shared fragments each source gives it,
    and a root holding whatever its own footprint adds to them."""
    plan = Plan()
    for test in sorted(tests, key=lambda t: t.check):
        children = [f for f in (source(run, plan, test) for source in SOURCES) if f is not None]
        shared: set = set()
        for child in children:
            shared |= plan.flatten(child)
        own = {(kind, name) for kind, names in test.reads.items() for name in names} - shared
        root = plan.fragment(own, children)
        plan.records.append(Record(test.check, OUTCOMES.get(test.outcome, "error"), root,
                                   max(0, int(test.duration_ms)) * 1000))
    return plan


def tests_from(manifest: dict, run: Run) -> list:
    """The manifest's tests with their ids and modules made repository-relative. The engine's node
    ids are relative to the run root, so `tiderace run tests/` and `tiderace run .` would name one
    test two ways; a check id has to be the same wherever the run was started."""
    prefix = run.rel(run.run_root) or ""
    prefix = "" if prefix == "." else prefix + "/"
    by_check: dict = {}
    for entry in manifest.get("tests", []):
        node = entry["id"]
        check = prefix + node
        reads = {kind: [n for n in names if kind != "file" or run.is_key_path(n)]
                 for kind, names in (entry.get("reads") or {}).items()}
        by_check[check] = Test(check, prefix + node.split("::", 1)[0], entry.get("outcome", "error"),
                               entry.get("duration_ms", 0), reads)
    return list(by_check.values())


# ------------------------------------------------------------------------------ write
class Values:
    """A value for every key, from the tree on disk, each computed once: a file's is the hash of
    its bytes or nothing when it is not there; any other kind's is the hash of `Plan.inputs`."""

    def __init__(self, etf, trace_root: str, inputs: dict):
        self.etf, self.trace_root, self.inputs = etf, trace_root, inputs
        self._memo: dict = {}

    def of(self, key: tuple):
        if key not in self._memo:
            self._memo[key] = self._compute(*key)
        return self._memo[key]

    def _compute(self, kind: str, name: str):
        if kind == "file":
            try:
                with open(os.path.join(self.trace_root, name.replace("/", os.sep)), "rb") as fh:
                    return self.etf.value_hash("file", fh.read())
            except OSError:
                return None
        data = self.inputs.get((kind, name))
        return None if data is None else self.etf.value_hash(kind, data)


def write(plan: Plan, run: Run, *, created_at_ms: int, tree: bytes | None = None, etf=None) -> bytes:
    """The plan as ETF bytes, through the binding. Fragments are added children-first, as the plan
    holds them, and records in check-id order, so one run writes one sequence of bytes."""
    etf = etf or binding()
    values = Values(etf, run.trace_root, plan.inputs)

    def pairs(fragment: Fragment) -> list:
        return [(kind, name.encode("utf-8", "surrogateescape"), values.of((kind, name)))
                for kind, name in sorted(fragment.keys)]

    toolchain_id = etf.fragment_id(pairs(plan.fragments[plan.toolchain])) if plan.toolchain >= 0 \
        else etf.fragment_id([])
    writer = etf.Writer(harness="tiderace", harness_version=run.harness_version, u_version="",
                        toolchain_fragment=toolchain_id, tree=tree or bytes(32),
                        created_at_ms=created_at_ms, granularity="file")
    ids: list = []
    for fragment in plan.fragments:
        ids.append(writer.add_fragment(pairs(fragment), children=[ids[c] for c in sorted(fragment.children)]))
    for record in plan.records:
        writer.add_record(record.check, record.outcome, ids[record.root], duration_us=record.duration_us)
    return writer.finish()


# ------------------------------------------------------------------------------ the mode
def harness_version(fallback: str) -> str:
    """The installed distribution's version — the one a user can name — else the engine's own."""
    try:
        import importlib.metadata

        return importlib.metadata.version("tiderace")
    except Exception:  # noqa: BLE001 — a checkout has no distribution
        return fallback


def emit(manifest: dict) -> str:
    """Write the ETF file a manifest describes; returns the one-line summary."""
    if manifest.get("v") != MANIFEST_VERSION:
        raise ValueError(f"unknown manifest version {manifest.get('v')!r}")
    etf = binding()
    roots = manifest.get("sys_path_file")
    if roots and os.path.isfile(roots):
        # Resolve imports against the path the workers had, not this bare interpreter's.
        with open(roots, encoding="utf-8") as fh:
            sys.path[:] = json.load(fh)["sys_path"]
    trace_root = os.path.abspath(manifest["trace_root"])
    run = Run(os.path.abspath(manifest["run_root"]), trace_root,
              harness_version(manifest.get("engine_version", "0.0.0")),
              toolchain_dirs=_installed_under(trace_root))
    plan = build(run, tests_from(manifest, run))
    tree = bytes.fromhex(manifest["tree"]) if manifest.get("tree") else None
    data = write(plan, run, created_at_ms=int(manifest.get("created_at_ms") or time.time() * 1000),
                 tree=tree, etf=etf)
    out = manifest["out"]
    tmp = f"{out}.{os.getpid()}.tmp"
    with open(tmp, "wb") as fh:
        fh.write(data)
    os.replace(tmp, out)
    return f"{len(plan.records)} records, {len(plan.fragments)} fragments, {len(data)} bytes"


def main(argv: list) -> int:
    """`--etf-check`: is the binding there. `--etf-emit <manifest>`: write the file."""
    try:
        if "--etf-check" in argv:
            binding()
            return 0
        with open(argv[argv.index("--etf-emit") + 1], encoding="utf-8") as fh:
            manifest = json.load(fh)
        print(emit(manifest))
        return 0
    except EtfUnavailable as exc:
        _warn(str(exc))
        return EXIT_NO_BINDING
    except Exception as exc:  # noqa: BLE001 — one line for the engine to relay, not a traceback
        _warn(f"--etf: {type(exc).__name__}: {exc}")
        return 1
