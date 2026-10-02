"""What the shim does when launched (TID-124, step 5): `serve` — the worker loop, alone or as a pool
forked from one imported image — `probe` (sub-interpreter safety, ADR-E015) and `subinterp` (the
sub-interpreter pool), dispatched by `main` on argv. The top of the package: everything else is
below it, nothing imports it.
"""
from __future__ import annotations

import os
import socket
import sys
import threading

from .config import _env_flag, option as _argv_option, RunConfig
from .discovery import (discover, _insert_run_root, _load_ancestor_conftests, _PhaseTimer,
                        preimport, Discovery)
from .engine import _start_clean_room, Engine
from .fixtures import Registry
from .footprint import _load_file_deps_cache, Caches
from .log import warn as _warn
from .nodes import module_name as _module_name
from .protocol import reap, spawn, Transport
from .results import errored
from .tiers import EngineOptions


def _probe_module_safe(root: str, module_key: str, paths: list) -> dict:
    """Sub-interpreter safety probe (ADR-E015, TID-9). Import the module (and thus its transitive
    closure) in a **fresh isolated sub-interpreter** (`concurrent.interpreters`, PEP 734 / per-interpreter
    GIL); if it loads there the module is *safe* to run on the sub-interpreter tier, otherwise not (e.g.
    a single-phase-init C-extension like numpy: `... does not support loading in subinterpreters`).
    Reports `safe=None` when the API is unavailable (< CPython 3.14) so the caller falls back."""
    module_name = _module_name(module_key, root)
    try:
        from concurrent import interpreters
    except Exception:  # noqa: BLE001 — no sub-interpreter API ⇒ undeterminable, caller falls back to fork
        return {"module": module_key, "safe": None, "reason": "concurrent.interpreters unavailable (CPython < 3.14)"}
    # Process-global state is shared across sub-interpreters: the working directory, the
    # environment `putenv` reaches, signal handlers, the umask. A module whose tests move any of
    # it is unsafe there whatever it imports — click's `monkeypatch.chdir` into a temp directory
    # that another interpreter's teardown then removed left every other interpreter with no
    # working directory at all, 306 errors and a hung pool (TID-104). Found by text, since the
    # import probe cannot see what a test body will do.
    global_touch = _touches_process_globals(root, module_key)
    if global_touch:
        return {"module": module_key, "safe": False,
                "reason": f"touches process-global state ({global_touch}), shared across sub-interpreters"}
    interp = interpreters.create()
    try:
        interp.exec("import sys\nsys.path[:0] = %r\nimport %s\n" % (paths, module_name))
        return {"module": module_key, "safe": True}
    except Exception as exc:  # noqa: BLE001 — an import failure in the sub-interp ⇒ unsafe (the point)
        text = str(exc).strip()
        reason = text.splitlines()[-1][:200] if text else type(exc).__name__
        return {"module": module_key, "safe": False, "reason": reason}
    finally:
        try:
            interp.close()
        except Exception:  # noqa: BLE001
            pass


_PROCESS_GLOBAL_CALLS = ("chdir(", "isolated_filesystem(", "putenv(", "unsetenv(", "setenv(",
                         "delenv(", "os.environ[", "signal.signal(", "umask(")


def _touches_process_globals(root: str, module_key: str) -> str:
    """The first process-global call named in a test module's source, or in the `conftest.py`
    beside it, or `""` — the text check behind the sub-interpreter probe (TID-104)."""
    root = os.path.abspath(root or ".")
    candidates = [os.path.join(root, module_key),
                  os.path.join(root, os.path.dirname(module_key), "conftest.py")]
    for path in candidates:
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError:
            continue
        for call in _PROCESS_GLOBAL_CALLS:
            if call in text:
                return f"{call[:-1] if call.endswith('(') else call} in {os.path.basename(path)}"
    return ""


def probe() -> int:
    """`--probe` mode: classify each requested module as sub-interpreter-safe (ADR-E015 detection).
    Same framed pipe as `serve`: reads `{"module": "<rel/path.py>"}` frames, replies
    `{"module", "safe": true|false|null, "reason"?}`. No tests run — this only decides eligibility."""
    root = sys.argv[1]
    _insert_run_root(root)
    paths = list(sys.path)  # the sub-interpreter inherits the same import roots (root + site-packages + …)
    transport = Transport.stdio(redirect_stdout=False)
    transport.ready()
    transport.serve(lambda req: _probe_module_safe(root, req["module"], paths))
    return 0


# Runs INSIDE each pool sub-interpreter (ADR-E015 Phase 2). Builds its own warm Engine — `restore=True`
# gives per-test isolation *within* the interpreter, and the sub-interpreter boundary isolates it from
# the other workers. Pulls tasks off the shared queue, runs them in-process, pushes results back.
_SUBINTERP_WORKER_LOOP = """
import sys
sys.path[:0] = list(_paths)
from tiderace_shim import config as _config, discovery as _discovery, engine as _engine, results as _results
_run = _config.RunConfig.load(_root)
_eng = _engine.Engine(_discovery.discover(_run), _run, no_fork=True, restore=True)
try:
    while True:
        _task = _in_q.get()
        if _task is None:
            break
        try:
            _r = _eng.run(_task["node_id"], _task["style"], _task.get("deadline_ms", 5000),
                          force_no_fork=True)
            # The whole response — expansion, variants, skips, keywords — so the engine reads it
            # as it reads every other transport's (TID-104): an empty expansion is a deselected
            # node, not a pass, and a parametrized node is its cases.
            _r.setdefault("node_id", _task["node_id"])
            _out_q.put(_r)
        except BaseException as _exc:  # noqa: BLE001 — never drop a task's response
            _out_q.put(_results.errored(_task["node_id"], repr(_exc)))
finally:
    _eng.teardown_all()
"""


def subinterp() -> int:
    """`--subinterp` mode (ADR-E015 Phase 2): run a batch of *safe* tests across a pool of isolated
    sub-interpreters, parallel via per-interpreter GILs (PEP 684). Batch protocol: read one
    `{"batch": [{node_id, style, deadline_ms}, …]}` frame, reply one `{"results": [{node_id, outcome,
    detail}, …]}` frame (input order). The caller only routes sub-interpreter-safe modules here."""

    from concurrent import interpreters  # 3.14+; the caller probes first, so this is expected present

    root = sys.argv[1]
    # As in `serve()` (TID-103): every sub-interpreter's `sys.stdout` is fd 1, so a test that
    # printed put its bytes into the engine's result stream — read as a frame length, waited on
    # forever; click's suite left the engine waiting on an idle pool (TID-104).
    transport = Transport.stdio()
    _insert_run_root(root)
    paths = list(sys.path)
    workers = max(1, int(_argv_option(sys.argv[2:], "--pool-size") or os.cpu_count() or 4))

    in_q = interpreters.create_queue()
    out_q = interpreters.create_queue()
    pool, threads = [], []
    for _ in range(workers):
        it = interpreters.create()
        it.prepare_main(_paths=tuple(paths), _root=root, _in_q=in_q, _out_q=out_q)
        t = threading.Thread(target=it.exec, args=(_SUBINTERP_WORKER_LOOP,), daemon=True)
        t.start()
        pool.append(it)
        threads.append(t)

    def handle(req: dict) -> dict:
        batch = req.get("batch", [])
        for task in batch:
            in_q.put(task)
        collected = {}
        # Each result is waited for at most the deadline plus the margin the engine allows a
        # silent worker (TID-104). A test blocked in a sub-interpreter cannot be interrupted
        # — no signal lands there, and a watchdog thread cannot be a daemon — so a result
        # that does not come is reported for every task still outstanding, naming them, and
        # this process exits: the engine launches a fresh pool for the next batch.
        budget = max((t.get("deadline_ms", 5000) for t in batch), default=5000) / 1000 + 10
        for _ in range(len(batch)):
            try:
                r = out_q.get(timeout=budget)
            except Exception:  # noqa: BLE001 — QueueEmpty on timeout, whatever its spelling
                pending = [t["node_id"] for t in batch if t["node_id"] not in collected]
                detail = (f"no result within {budget:g}s — a test in this batch blocked in a "
                          f"sub-interpreter, where nothing can interrupt it (TID-104); "
                          f"outstanding: {', '.join(pending)}")
                for node in pending:
                    collected[node] = errored(node, detail)
                transport.send({"results": [collected[t["node_id"]] for t in batch]})
                os._exit(1)  # the blocked interpreter cannot be joined; the pool is done
            collected[r["node_id"]] = r
        return {"results": [collected[t["node_id"]] for t in batch]}

    transport.ready()
    try:
        transport.serve(handle)
        return 0
    finally:
        for _ in pool:
            in_q.put(None)  # stop each worker
        for t in threads:
            t.join(timeout=5)


def _serve_pool(transport: Transport, size: int, socket_path: str, engine_args: dict) -> int:
    """Import once in this process, then fork `size` workers that each serve their own connection.

    The pool exists because the per-worker *import* was being paid N times (TID-4). Every wellspring
    in the old pool was an independent `python shim.py`, so an 8-worker run imported the project
    eight times — on a large-import corpus that is ~2.6s of CPU each, ~21s of the ~34s that eight
    workers add. Wall clock hid it, because the imports overlap; a CI runner billed for CPU does not.

    The fix is the same primitive the engine already runs on. `preimport`/`discover` happen once,
    *here*, and then `fork()` hands every worker a copy-on-write view of the result for free. Each
    child runs the ordinary `serve` loop unchanged — the only difference is which fd it talks over.

    Workers connect *back* to a listening socket rather than being handed inherited fds. That keeps
    the whole thing dependency-free on both sides: no `SCM_RIGHTS`, no `dup2`, and nothing for the
    Rust side to do beyond accepting `size` connections.
    """
    if size == 0:
        return _serve_pool_persistent(transport, engine_args)
    children = _fork_pool_workers(size, socket_path, engine_args)
    # Parent: nothing to serve. Hold the imported image alive — the children are COW views of it —
    # and reap them so no worker is orphaned if the run is cut short.
    status = 0
    for pid in children:
        st = reap(pid)
        if os.WIFEXITED(st) and os.WEXITSTATUS(st) != 0:
            status = os.WEXITSTATUS(st)
    return status


def _serve_pool_persistent(transport: Transport, engine_args: dict) -> int:
    """The warm image (TID-84): import once, then serve the Rust side's requests over stdin/stdout
    for as long as it stays connected — `{"spawn": n, "connect": path}` forks `n` workers that
    connect to `path` and serve one run each; `{"ping": true}` answers `{"pong": true}`; EOF ends
    the process. Every run forks fresh workers from the one imported image, so the second run pays
    no import at all. Finished workers are reaped before each spawn; the rest at exit."""
    live: list = []

    def handle(req: dict) -> dict | None:
        live[:] = [pid for pid in live if os.waitpid(pid, os.WNOHANG)[0] == 0]
        if req.get("ping"):
            return {"pong": True, "pid": os.getpid(), "workers": len(live)}
        n = int(req.get("spawn", 0))
        if n:
            live.extend(_fork_pool_workers(n, req["connect"], engine_args, req.get("selection")))
            return {"spawned": n}
        return None

    transport.ready()
    try:
        transport.serve(handle)
    finally:
        for pid in live:
            try:
                reap(pid)
            except ChildProcessError:
                pass
    return 0


def _fork_pool_workers(size: int, socket_path: str, engine_args: dict,
                       selection: dict | None = None) -> list:
    """Fork `size` workers off this (imported) process, each connecting to `socket_path` and
    serving the ordinary single-worker loop until its connection closes. Returns their pids.
    `selection` is this run's `-k` / `-m` / `--strict-markers`, applied in each child (TID-90)."""

    children = []
    for _ in range(size):
        def worker() -> int:
            # Child: take a connection of our own and become an ordinary single worker. Anything the
            # parent is holding is irrelevant to us and closing it keeps the parent's exit clean.
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(socket_path)
            transport = Transport.over(sock)
            engine = Engine(**engine_args)
            engine.apply_selection(selection)
            _start_clean_room(engine)  # before a single test runs: the image is pristine now (TID-50)
            transport.ready()
            try:
                transport.serve(_run_handler(engine))
            finally:
                engine.teardown_all()
            return 0

        children.append(spawn(worker))
    return children


def serve() -> int:
    root = sys.argv[1]
    transport = Transport.stdio()  # fd 1 goes to stderr; the frames have a private fd (TID-103)
    no_fork = "--no-fork" in sys.argv[2:]
    coverage = "--coverage" in sys.argv[2:] or _env_flag("TIDERACE_COVERAGE")
    coverage_lines = ("--coverage-lines" in sys.argv[2:]
                      or _env_flag("TIDERACE_COVERAGE_LINES"))
    purity = "--purity" in sys.argv[2:] or _env_flag("TIDERACE_PURITY")
    restore = "--restore" in sys.argv[2:] or _env_flag("TIDERACE_RESTORE")
    _insert_run_root(root)
    caches = Caches()
    _load_file_deps_cache(caches, root)  # earlier runs' closures, before anything computes one (TID-82)
    # Ancestor conftests before `preimport` (TID-19): a root conftest exists to set things up that
    # must already be true when test modules import — env defaults, warning filters, `sys.path`. pytest
    # loads conftests first for the same reason. `discover` reads the memoised result back.
    # The run's configuration — the project's own, and the modules `--modules` names — before
    # anything is imported (TID-75).
    run = RunConfig.load(root, modules_file=_argv_option(sys.argv[2:], "--modules"))
    # `TIDERACE_TIMING=1` prints how long each start-up phase took, to stderr. The start-up is a
    # fixed cost every run pays before a worker exists; knowing which phase is the cost is what
    # decides what to do about it (TID-75).
    _phase = _PhaseTimer()
    if _phase.on and run.modules is not None:
        _warn(f"start-up: {len(run.modules)} modules selected")
    disc = Discovery(Registry())
    _load_ancestor_conftests(root, disc)
    _phase.mark("ancestor conftests")
    preimport(run)
    _phase.mark("pre-import test modules")
    discover(run, disc, timer=_phase)
    _phase.mark("discover (conftests, fixtures, hooks, marks)")
    if _phase.on and _phase.unselected:
        _warn(f"start-up: {_phase.unselected} test modules not imported (unselected)")
    engine_args = dict(discovery=disc, config=run, caches=caches, options=EngineOptions(
        no_fork=no_fork, restore=restore, purity_guard=purity, coverage=coverage,
        coverage_lines=coverage_lines))
    # Pool mode (TID-4): fork the workers from this one imported image instead of importing per
    # worker. Every worker below is created *after* the fork, so its fixture state is its own and
    # the semantics match N separate wellsprings exactly.
    pool = _argv_option(sys.argv[2:], "--pool")
    conn = _argv_option(sys.argv[2:], "--connect")
    if pool and conn:
        return _serve_pool(transport, int(pool), conn, engine_args)
    engine = Engine(**engine_args)
    _start_clean_room(engine)  # before a single test runs: the image is pristine now (TID-50)
    transport.ready()
    try:
        transport.serve(_run_handler(engine))
        return 0
    finally:
        engine.teardown_all()


def _run_handler(engine: "Engine"):
    """The worker's request: one node, run with the engine's knobs — what `serve` and every pool
    worker answer."""
    return lambda req: engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                                  req.get("force_no_fork", False), req.get("trusted_pure", False),
                                  req.get("must_fork", False))


def main() -> int:
    """The shim's modes, dispatched on argv: `<root> --probe`, `<root> --subinterp`, else serve.
    Run through the entry file (`py-shim/shim.py`, staged as `tiderace/_shim/shim.py` in the
    wheel) or as `python -m tiderace_shim`."""
    if "--probe" in sys.argv[2:]:
        return probe()
    if "--subinterp" in sys.argv[2:]:
        return subinterp()
    return serve()
