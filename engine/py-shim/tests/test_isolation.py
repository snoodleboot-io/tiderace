"""Isolation as one object (TID-123, step 2): what a test disturbed is measured by `verdict` and put
back by `restore`, with the precedence the shim always applied."""
from __future__ import annotations

import os
import sys
import threading
import types

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import isolation, results  # noqa: E402


def module_with(**globals_):
    mod = types.ModuleType("test_iso_mod")
    vars(mod).update(globals_)
    return mod


def test_nothing_measured_is_unknown_purity():
    iso = isolation.Isolation.before("t.py", None, (), measure=True, full=False)
    assert iso.verdict() == (results.UNKNOWN_PURITY, None)
    iso = isolation.Isolation.before("t.py", module_with(x=1), (), measure=False, full=False)
    assert iso.verdict() == (results.UNKNOWN_PURITY, None)


def test_a_mutated_global_is_impure_and_restore_puts_it_back_in_place():
    registry = {"a": 1}
    mod = module_with(COUNT=0, REGISTRY=registry)
    iso = isolation.Isolation.before("t.py", mod, (), measure=True, full=False)
    assert iso.verdict() == (None, None)  # untouched: measured pure
    mod.COUNT = 5
    purity, leaked = iso.verdict()
    assert purity == "mutated module global `COUNT`" and leaked is None
    mod.REGISTRY["b"] = 2
    mod.ADDED = "new"
    iso.restore()
    assert mod.COUNT == 0 and mod.REGISTRY == {"a": 1} and not hasattr(mod, "ADDED")
    assert mod.REGISTRY is registry  # restored in place (TID-22): references keep seeing the same object


def test_environ_counts_and_is_restored(monkeypatch):
    mod = module_with()
    iso = isolation.Isolation.before("t.py", mod, (), measure=True, full=False)
    os.environ["TIDERACE_ISO_TEST"] = "1"
    try:
        assert iso.verdict() == ("mutated os.environ", None)
        iso.restore()
        assert "TIDERACE_ISO_TEST" not in os.environ
    finally:
        os.environ.pop("TIDERACE_ISO_TEST", None)


def test_full_isolation_sees_the_interpreter_state_and_restores_it():
    mod = module_with()
    iso = isolation.Isolation.before("t.py", mod, (), measure=True, full=True)
    sys.path.append("/tiderace/iso/test")
    try:
        purity, leaked = iso.verdict()
        assert purity == "changed interpreter state: changed sys.path" and leaked is None
        iso.restore()
        assert "/tiderace/iso/test" not in sys.path
    finally:
        if "/tiderace/iso/test" in sys.path:
            sys.path.remove("/tiderace/iso/test")


def test_a_replaced_module_is_impure_whatever_the_globals_say():
    mod = module_with()
    name = "tiderace_iso_victim"
    sys.modules[name] = original = types.ModuleType(name)
    try:
        iso = isolation.Isolation.before("t.py", mod, (), measure=True, full=True)
        sys.modules[name] = types.ModuleType(name)
        purity, _ = iso.verdict()
        assert purity == f"replaced modules in sys.modules: {name}"
        iso.restore()
        assert sys.modules[name] is original
    finally:
        sys.modules.pop(name, None)


def test_a_thread_left_running_leaks_and_the_verdict_says_so():
    mod = module_with()
    iso = isolation.Isolation.before("t.py", mod, (), measure=True, full=True)
    stop = threading.Event()
    t = threading.Thread(target=stop.wait, daemon=True)
    t.start()
    try:
        purity, leaked = iso.verdict()
        assert leaked == "left 1 thread(s) running (unrestorable: left 1 thread(s) running)"
        assert purity == f"disturbed interpreter state: {leaked}"
    finally:
        stop.set()
        t.join()
    iso.restore()


def test_a_library_pools_idle_worker_is_not_a_leak():
    """anyio / trio / concurrent.futures keep a worker alive after a test used it — a cache, like a
    lazy import (TID-125). A thread the test started itself still counts."""
    stop = threading.Event()
    pooled = lambda: stop.wait()  # noqa: E731
    pooled.__module__ = "trio._core._thread_cache"
    own = lambda: stop.wait()  # noqa: E731
    own.__module__ = "test_iso_mod"
    before = isolation._live_threads()
    threads = [threading.Thread(target=pooled, daemon=True), threading.Thread(target=own, daemon=True)]
    for t in threads:
        t.start()
    try:
        assert isolation._live_threads() == before + 1
        assert isolation._thread_home(threads[0]) == "trio" and isolation._thread_home(threads[1]) == "test_iso_mod"
    finally:
        stop.set()
        for t in threads:
            t.join()


def test_restorable_refuses_an_opaque_global():
    assert isolation._restorable(module_with(x=[1, 2], y={"k": 1}))
    assert not isolation._restorable(module_with(gen=(i for i in range(3))))
