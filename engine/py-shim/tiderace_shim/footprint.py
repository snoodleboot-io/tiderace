"""What a test touches (TID-124, step 5): the per-test executed-source capture (`_Coverage`, ADR-E006),
the static import closure a module depends on (TID-40) and the file-dependency memos behind it
(TID-76, TID-82), and the third-party packages a test module reaches (TID-46). `Caches` is the
process's memo for all of it.
"""
from __future__ import annotations

import ast
import dataclasses
import hashlib
import json
import os
import re
import sys

from .config import _env
from .log import warn as _warn


# The test frameworks themselves. A suite mutating pytest's internals is not the leak this is for,
# and pytest is large: including it made the scan below cost 7ms, more than the tests it wraps.
_UNWATCHED_PACKAGES = frozenset({"pytest", "_pytest", "py", "tiderace", "unittest", "hypothesis"})


@dataclasses.dataclass
class Caches:
    """The per-process memos (TID-124), built lazily and inherited by every forked child: the
    third-party packages each test module imports (TID-46); the registry targets found in them
    (TID-68); each module's static import closure, `module_key -> {rel_path, …}` (TID-40); per
    source file the in-tree files it imports (TID-76), and the same carried across runs (TID-82) —
    `path -> [mtime_ns, size, sys.path hash, deps]`, loaded by the pool parent before it forks and
    extended by every worker at teardown, an entry used only when the file is unchanged and the
    import roots are the ones it was resolved under; and the resolution of each dotted import."""

    watched_packages: dict = dataclasses.field(default_factory=dict)  # module key -> packages
    registry_targets: dict = dataclasses.field(default_factory=dict)  # module key -> (…, targets)
    import_closure: dict = dataclasses.field(default_factory=dict)  # module key -> frozenset
    file_deps: dict = dataclasses.field(default_factory=dict)  # source file -> in-tree imports
    file_deps_cache: dict = dataclasses.field(default_factory=dict)  # earlier runs' entries
    file_deps_new: dict = dataclasses.field(default_factory=dict)  # this process's, written at teardown
    file_deps_stats: dict = dataclasses.field(default_factory=lambda: {"hits": 0, "parsed": 0})
    resolved: dict = dataclasses.field(default_factory=dict)  # (dotted, level, importing dir) -> file


def _watched_packages(caches: Caches, root: str, module_key: str) -> tuple:
    """Top-level **non-stdlib** packages a test module imports, for registry watching (TID-46).

    Scoped deliberately. Watching every module in `sys.modules` would cost more than the tests do,
    and watching the standard library would demote constantly for no reason: `re._cache` is a
    module-level dict that grows the first time anything compiles a pattern, which is not a leak.
    A library the *test file itself* imports is where a registry mutation can plausibly come from —
    click's `_available_shells`, a codec or plugin table, a framework's app registry."""
    cached = caches.watched_packages.get(module_key)
    if cached is not None:
        return cached
    roots: set = set()
    path = os.path.join(root or ".", module_key)
    for name, level in _imported_names(path):
        if level or not name:
            continue  # a relative import is the suite's own code, covered by the module snapshot
        root = name.partition(".")[0]
        if root and root not in sys.stdlib_module_names and root not in _UNWATCHED_PACKAGES:
            roots.add(root)
    result = tuple(sorted(roots))
    caches.watched_packages[module_key] = result
    return result


# An import statement starts a line, or follows `;` or a compound statement's `:` on one. `yield from`
# and `from_x = ...` do not match. What this finds is parsed as a statement, so names are exact.
_IMPORT_STMT = re.compile(r"(?:^|[;:])[ \t]*(import|from)[ \t]")


def _imported_names(path: str) -> list[tuple[str, int]]:
    """Every module a file imports, as `(dotted_name, relative_level)`.

    AST rather than execution, because that is the whole point: a module's `import` lines run *once*,
    for whichever test happens to be first, so nothing that watches execution can see the imports of
    the nineteen tests that follow. Parsing sees all of them, in any order, every time.

    Parsing a whole file is 1.5ms; a closure walks ~100 of them and every worker walks the closures
    of every module it runs (TID-76). So this parses only the import *statements*: a scan finds the
    lines, each statement is parsed on its own, and the names are exactly what a full parse gives.
    The one thing a line scan cannot tell is whether a line sits inside a string, so a file whose
    candidate import lines fall inside a triple-quoted region takes the full parse instead — exact
    over fast, never a missed import."""
    try:
        with open(path, encoding="utf-8") as fh:
            src = fh.read()
    except (OSError, UnicodeDecodeError):
        return []  # unreadable ⇒ no closure; the runtime footprint still applies
    if "import" not in src:
        return []
    stmts = _scan_import_statements(src)
    if stmts is None:  # a candidate inside a string region: parse the whole file
        try:
            tree = ast.parse(src, filename=path)
        except SyntaxError:
            return []
        return _import_names_in(ast.walk(tree))
    out: list[tuple[str, int]] = []
    for stmt in stmts:
        try:
            out.extend(_import_names_in(ast.parse(stmt).body))
        except SyntaxError:
            continue  # not a statement after all (a comment, a fragment); contributes nothing
    return out


def _scan_import_statements(src: str) -> list[str] | None:
    """The import statements in `src`, each as its own parseable text, or None if any candidate
    lies inside a triple-quoted region (the caller then parses the whole file)."""
    lines = src.split("\n")
    stmts: list[str] = []
    in_string: str | None = None  # the delimiter of the triple-quoted region we are inside, if any
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        i += 1
        candidate = "import" in line and _IMPORT_STMT.search(line)
        if candidate and in_string:
            return None
        if candidate:
            m = candidate
            stmt = line[m.start(1):]
            depth = stmt.count("(") - stmt.count(")")
            while (depth > 0 or stmt.rstrip().endswith("\\")) and i < n:
                nxt = lines[i]
                i += 1
                stmt = stmt.rstrip().rstrip("\\") + "\n" + nxt
                depth += nxt.count("(") - nxt.count(")")
            stmts.append(stmt)
        # Track triple-quoted regions after the line's own statement is taken: a docstring that
        # opens and closes on this line leaves the state as it was.
        for tq in ('"""', "'''"):
            if line.count(tq) % 2 == 1:
                if in_string == tq:
                    in_string = None
                elif in_string is None:
                    in_string = tq
    return stmts


def _import_names_in(nodes) -> list[tuple[str, int]]:
    out: list[tuple[str, int]] = []
    for node in nodes:
        if isinstance(node, ast.Import):
            out.extend((a.name, 0) for a in node.names)
        elif isinstance(node, ast.ImportFrom):
            base = node.module or ""
            out.append((base, node.level))
            # `from pkg import mod` may name a submodule rather than an attribute; both resolve
            # harmlessly, and a miss just contributes nothing.
            out.extend((f"{base}.{a.name}" if base else a.name, node.level) for a in node.names)
    return out


def _resolve_module_file(caches: Caches, dotted: str, level: int, from_file: str, root: str) -> str | None:
    """The file a dotted import resolves to **inside the suite**, or None if it is external.

    Third-party and stdlib imports are deliberately dropped: a footprint exists to answer "did
    anything this test depends on change in this tree", and site-packages does not change between
    runs of the same checkout.

    Memoised on (name, level, importing directory): the same `import os` or `from pirn.x import y`
    appears in hundreds of files, and each resolution probes every `sys.path` entry (TID-76)."""
    key = (dotted, level, os.path.dirname(from_file) if level else "")
    if key in caches.resolved:
        return caches.resolved[key]
    resolved = caches.resolved[key] = _resolve_module_file_uncached(dotted, level, from_file, root)
    return resolved


def _resolve_module_file_uncached(dotted: str, level: int, from_file: str, root: str) -> str | None:
    if level:  # relative import: resolve against the importing file's package
        base_dir = os.path.dirname(os.path.abspath(from_file))
        for _ in range(level - 1):
            base_dir = os.path.dirname(base_dir)
        candidates = [os.path.join(base_dir, *dotted.split(".")) if dotted else base_dir]
    else:
        candidates = [os.path.join(p, *dotted.split(".")) for p in sys.path if p]
    for stem in candidates:
        for candidate in (stem + ".py", os.path.join(stem, "__init__.py")):
            if os.path.isfile(candidate):
                abs_path = os.path.abspath(candidate)
                # Only what lives under the run root; anything else is not ours to invalidate on.
                if abs_path.startswith(os.path.abspath(root) + os.sep):
                    return abs_path
                return None
    return None


def _file_deps(caches: Caches, path: str, root: str) -> tuple[str, ...]:
    """The in-tree files one source file imports, parsed and resolved once per process.

    The closures of different test modules overlap almost entirely — on pirn-core each one walks
    ~100 files, and nearly all of them are the same project files every time. Without this memo
    every module's closure re-parsed and re-resolved all of them: 230ms per module, 575 modules,
    once per worker, which was the whole of coverage's cost on a cold run (TID-76). `root` and
    `sys.path` are fixed for the life of a process, so the key is the file alone."""
    cached = caches.file_deps.get(path)
    if cached is None:
        cached = _file_deps_from_cache(caches, path)
        if cached is None:
            deps: dict[str, None] = {}
            for dotted, level in _imported_names(path):
                resolved = _resolve_module_file(caches, dotted, level, path, root)
                if resolved:
                    deps[resolved] = None
            cached = tuple(deps)
            caches.file_deps_stats["parsed"] += 1
            try:
                st = os.stat(path)
                caches.file_deps_new[path] = [st.st_mtime_ns, st.st_size, _sys_path_key(), list(cached)]
            except OSError:
                pass
        caches.file_deps[path] = cached
    return cached


def _sys_path_key() -> str:
    """The import roots a resolution ran under, as one short token: a cached dependency list is only
    right for the `sys.path` that produced it."""
    return hashlib.sha1("\n".join(p for p in sys.path if p).encode("utf-8", "replace")).hexdigest()[:16]


def _file_deps_from_cache(caches: Caches, path: str):
    entry = caches.file_deps_cache.get(path)
    if entry is None:
        return None
    try:
        st = os.stat(path)
    except OSError:
        return None
    mtime_ns, size, key, deps = entry
    if st.st_mtime_ns != mtime_ns or st.st_size != size or key != _sys_path_key():
        return None
    caches.file_deps_stats["hits"] += 1
    return tuple(deps)


def _file_deps_cache_dir(root: str) -> str:
    return os.path.join(os.path.abspath(root), ".tiderace-cache", "file-deps")


def _load_file_deps_cache(caches: Caches, root: str) -> None:
    """Read the index and every worker file left by earlier runs, fold them into one index, and
    drop the worker files. Called once per process that serves a run — in the pool that is the
    parent, and the workers inherit the result through the fork (TID-82). Any file that does not
    parse is ignored; a concurrent run can lose an entry, never hand us a corrupt one."""
    d = _file_deps_cache_dir(root)
    try:
        names = os.listdir(d)
    except OSError:
        return
    merged: dict[str, list] = {}
    worker_files = []
    for name in sorted(names):
        if not name.endswith(".json"):
            continue
        full = os.path.join(d, name)
        try:
            with open(full, encoding="utf-8") as fh:
                data = json.load(fh)
            if data.get("v") == 1 and isinstance(data.get("files"), dict):
                merged.update(data["files"])
        except (OSError, ValueError):
            pass
        if name != "index.json":
            worker_files.append(full)
    caches.file_deps_cache.update(merged)
    if worker_files:
        _write_file_deps_index(d, merged)
        for full in worker_files:
            try:
                os.unlink(full)
            except OSError:
                pass


def _write_file_deps_index(d: str, files: dict) -> None:
    tmp = os.path.join(d, f".index-{os.getpid()}.tmp")
    try:
        os.makedirs(d, exist_ok=True)
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump({"v": 1, "files": files}, fh)
        os.replace(tmp, os.path.join(d, "index.json"))
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def _save_file_deps_cache(caches: Caches, root: str) -> None:
    """What this process parsed, to its own file under the cache dir; the next run's parent folds it
    in. Nothing to write is nothing written."""
    if not caches.file_deps_new or not root:
        return
    d = _file_deps_cache_dir(root)
    try:
        os.makedirs(d, exist_ok=True)
        tmp = os.path.join(d, f".w-{os.getpid()}.tmp")
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump({"v": 1, "files": caches.file_deps_new}, fh)
        os.replace(tmp, os.path.join(d, f"w-{os.getpid()}.json"))
    except OSError:
        pass
    if _env("TIDERACE_TIMING"):
        _warn(f"closure cache: {caches.file_deps_stats['hits']} files from cache, "
              f"{caches.file_deps_stats['parsed']} parsed")


def _import_closure(caches: Caches, module_key: str, root: str) -> frozenset:
    """Every in-tree file a module transitively imports, plus the conftests above it.

    This is the half of a test's dependency footprint that runtime coverage cannot produce (TID-40).
    Coverage sees a module's imports execute exactly once — for whichever test in it ran first — so
    on a twenty-test module the source under test appeared in one footprint out of twenty, and
    impact selection served the other nineteen from cache after that source changed. It reported a
    green suite that a full run reported as twenty failures.

    Conftests are included because a change to one alters fixtures for everything beneath it, and
    nothing in the runtime footprint necessarily mentions the conftest at all."""
    cached = caches.import_closure.get(module_key)
    if cached is not None:
        return cached
    root_abs = os.path.abspath(root)
    start = os.path.join(root_abs, module_key.replace("/", os.sep))
    seen: set[str] = set()
    queue = [start]
    while queue:
        current = queue.pop()
        for resolved in _file_deps(caches, current, root):
            if resolved not in seen:
                seen.add(resolved)
                queue.append(resolved)
    # Every conftest from the run root down to this module's directory.
    directory = os.path.dirname(start)
    while directory.startswith(root_abs):
        conftest = os.path.join(directory, "conftest.py")
        if os.path.isfile(conftest):
            seen.add(os.path.abspath(conftest))
        if directory == root_abs:
            break
        directory = os.path.dirname(directory)
    closure = frozenset(os.path.relpath(p, root_abs).replace(os.sep, "/") for p in seen)
    caches.import_closure[module_key] = closure
    return closure


class _Coverage:
    """Per-test executed-source capture inside the fork child (ADR-E006, design 11). Uses PEP 669
    `sys.monitoring` on CPython 3.12+, falling back to `sys.settrace` on ≤3.11. Records
    `{rel_source_path: set(line)}` for `.py` files under `root` — the test's dependency footprint the
    impact selection and cache key consume. A no-op when disabled, so the default path is
    byte-identical to before.

    By default the footprint is **file-level**: one `PY_START` event per code object entered (module
    and class bodies are code objects too, so a dynamic import is seen), disabled after its first hit,
    and an empty line list per file — the convention the import closure already uses for "any change
    to this file counts". Nothing on a production path reads a line number (TID-76), so the default
    carries none; `lines=True` (`--coverage-lines`) keeps LINE capture for a consumer that wants it.
    (The cost of capture on a cold run was never the events or the lines — see `_file_deps`.)"""

    _TOOL_ID = 5  # sys.monitoring tool slot (0..5 available); 5 avoids coverage.py/profiler clashes

    def __init__(self, root: str | None, enabled: bool, lines: bool = False, caches: Caches | None = None):
        self.enabled = enabled and root is not None
        self.caches = caches if caches is not None else Caches()
        self.lines = lines
        self.root = os.path.abspath(root) if root else ""
        self.touched: dict[str, set] = {}
        self._mon = getattr(sys, "monitoring", None) if self.enabled else None
        self._prev_trace = None
        self._stopped = False  # makes stop() idempotent (called once for the report, once in finally)

    def _want(self, path: str | None) -> bool:
        return bool(path) and path.endswith(".py") and os.path.abspath(path).startswith(self.root)

    def start(self) -> None:
        if not self.enabled:
            return
        if self._mon is not None:
            mon, tid, events = self._mon, self._TOOL_ID, self._mon.events

            def on_line(code, line_no):
                fn = code.co_filename
                if self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set()).add(line_no)
                return mon.DISABLE  # per-location disable ⇒ each line fires at most once (cheap)

            def on_start(code, offset):
                fn = code.co_filename
                if self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set())
                return mon.DISABLE  # per-code-object disable ⇒ each function fires at most once

            # PY_RESUME as well: a generator or coroutine created by an earlier test (or a fixture)
            # and resumed inside this one never *starts* here, but its file is still one this test
            # ran code in. Both events carry (code, offset) and both are per-code-object.
            file_events = events.PY_START | events.PY_RESUME

            mon.use_tool_id(tid, "tiderace")
            # `DISABLE` is per location and outlives `free_tool_id`; only this clears it. Without it
            # the first test in the process to enter a function is the only one ever credited with
            # its file — every later test in the same worker sees nothing there (TID-76).
            mon.restart_events()
            if self.lines:
                mon.register_callback(tid, events.LINE, on_line)
                mon.set_events(tid, events.LINE)
            else:
                mon.register_callback(tid, events.PY_START, on_start)
                mon.register_callback(tid, events.PY_RESUME, on_start)
                mon.set_events(tid, file_events)
        else:  # ≤3.11 fallback
            want_lines = self.lines

            def tracer(frame, event, arg):
                fn = frame.f_code.co_filename
                if event == "call":
                    if not self._want(fn):
                        return None  # nothing to learn from this frame's lines
                    if not want_lines:
                        self.touched.setdefault(os.path.abspath(fn), set())
                        return None
                elif event == "line" and self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set()).add(frame.f_lineno)
                return tracer

            self._prev_trace = sys.gettrace()
            sys.settrace(tracer)

    def stop(self) -> dict:
        if not self.enabled or self._stopped:
            return self._report() if self.enabled else {}
        self._stopped = True
        if self._mon is not None:
            mon, tid = self._mon, self._TOOL_ID
            mon.set_events(tid, 0)
            for event in ((mon.events.LINE,) if self.lines
                          else (mon.events.PY_START, mon.events.PY_RESUME)):
                mon.register_callback(tid, event, None)
            mon.free_tool_id(tid)
        else:
            sys.settrace(self._prev_trace)
        return self._report()

    def _report(self) -> dict:
        # Forward slashes whatever the platform, as the import closure and the Rust side use: on
        # Windows the raw relpath put `src\thing.py` beside the closure's `src/thing.py`, so the
        # runtime half of a footprint never matched a changed file.
        return {os.path.relpath(p, self.root).replace(os.sep, "/"): sorted(lines)
                for p, lines in self.touched.items()}

    def report_with_imports(self, module_key: str) -> dict:
        """The runtime footprint plus the module's static import closure (TID-40).

        The two halves answer different questions and neither is sufficient alone. Coverage says
        what this test *executed*, which is the only way to know it reached a particular branch. The
        closure says what its module *depends on*, which is the only way to know about code that was
        imported before this test ran — i.e. everything, for every test after the first one in the
        module.

        Closure entries carry no line numbers. A footprint's line detail exists to narrow
        invalidation to the lines a test actually ran; for an import dependency there is nothing to
        narrow, and an empty list correctly means "any change to this file counts"."""
        report = self._report()
        if not self.enabled:
            return report
        for rel in _import_closure(self.caches, module_key, self.root):
            report.setdefault(rel, [])
        return report
