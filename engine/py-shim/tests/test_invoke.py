"""One invoke path for sync and async (TID-124, step 4): fixtures set up and torn down through one
handle, the body called through one function, the exception ladder in one place, and the sync
tier driving the async implementation without a loop."""
from __future__ import annotations

import asyncio
import os
import sys
import types
import unittest

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tiderace_shim import invoke  # noqa: E402
from tiderace_shim.invoke import FixtureHandle, Request, call_with_hooks, classify_exception, run_sync, setup_fixture  # noqa: E402


def fdef(func, *, is_yield=False, wants_request=False, owner=None):
    return types.SimpleNamespace(func=func, is_yield=is_yield, wants_request=wants_request, owner=owner)


def test_run_sync_drives_a_coroutine_that_never_suspends_and_refuses_one_that_does():
    async def quick():
        return 42

    assert run_sync(quick()) == 42

    async def slow():
        await asyncio.sleep(0)

    with pytest.raises(RuntimeError, match="async provider awaited outside"):
        run_sync(slow())


def test_a_sync_fixture_sets_up_and_tears_down_through_the_one_handle():
    events = []

    def fx(request):
        request.addfinalizer(lambda: events.append("finalizer"))
        events.append("setup")
        yield "value"
        events.append("teardown")

    value, handle = run_sync(setup_fixture(fdef(fx, is_yield=True, wants_request=True), {}, None, None))
    assert value == "value" and isinstance(handle, FixtureHandle) and handle.gen is not None
    handle.close()
    assert events == ["setup", "teardown", "finalizer"]  # the yield teardown first, then the finalizers (TID-44)
    plain, handle = run_sync(setup_fixture(fdef(lambda: 3), {}, None, None))
    assert plain == 3 and handle.gen is None and handle.agen is None and handle.finalizers == []
    handle.close()  # nothing to do, nothing raised


def test_an_async_fixture_runs_on_the_loop_and_a_sync_one_beside_it():
    events = []

    async def agen_fx():
        events.append("asetup")
        yield "a"
        events.append("ateardown")

    async def coro_fx():
        return "c"

    def sync_fx():
        yield "s"
        events.append("steardown")

    async def body():
        a, ha = await setup_fixture(fdef(agen_fx), {}, None, None)
        c, hc = await setup_fixture(fdef(coro_fx), {}, None, None)
        s, hs = await setup_fixture(fdef(sync_fx, is_yield=True), {}, None, None)
        assert (a, c, s) == ("a", "c", "s") and ha.agen is not None and hc.agen is None and hs.gen is not None
        for h in (hs, hc, ha):
            await h.aclose()

    asyncio.run(body())
    assert events == ["asetup", "steardown", "ateardown"]


def test_the_sync_tier_cannot_drive_an_async_fixture_but_a_fixture_param_and_node_arrive():
    async def coro_fx():
        await asyncio.sleep(0)

    with pytest.raises(RuntimeError):
        run_sync(setup_fixture(fdef(coro_fx), {}, None, None))
    seen = {}

    def fx(request):
        seen["param"], seen["node"] = request.param, request.node
        return 1

    run_sync(setup_fixture(fdef(fx, wants_request=True), {}, "p", "NODE"))
    assert seen == {"param": "p", "node": "NODE"}


def test_call_with_hooks_runs_setup_body_teardown_then_finalizers_and_settles_a_coroutine():
    events = []
    owner = types.SimpleNamespace(setup_function=lambda f: events.append("setup"),
                                  teardown_function=lambda f: events.append("teardown"))
    request = Request(None)
    request.addfinalizer(lambda: events.append("finalizer"))

    async def coro_test():
        await asyncio.sleep(0)
        events.append("body")

    hooks = ((owner, ("setup_function",), coro_test), (owner, ("teardown_function",), coro_test))
    run_sync(call_with_hooks(coro_test, {}, hooks, request, on_loop=False))  # the sync tier: a fresh loop
    assert events == ["setup", "body", "teardown", "finalizer"]
    events.clear()

    def failing():
        raise ValueError("boom")

    with pytest.raises(ValueError):
        run_sync(call_with_hooks(failing, {}, hooks, Request(None), on_loop=False))
    assert events == ["setup", "teardown"]  # the teardown hook still ran
    events.clear()

    async def on_loop_body():
        await call_with_hooks(coro_test, {}, hooks, Request(None))  # the async tier: awaited in place

    asyncio.run(on_loop_body())
    assert events == ["setup", "body", "teardown"]


def test_classify_exception_is_pytests_taxonomy():
    assert classify_exception(ValueError("x"))[0] == "failed"
    assert classify_exception(unittest.SkipTest("no backend")) == ("skipped", "no backend")
    outcome, detail = classify_exception(AssertionError("1 != 2"), introspect=lambda exc: "RICH\n")
    assert outcome == "failed" and detail.startswith("RICH\n") and "AssertionError: 1 != 2" in detail
    assert classify_exception(AssertionError("x"), introspect=lambda exc: None)[1] == "AssertionError: x\n"
    assert unittest.SkipTest in invoke.SKIP_EXCEPTIONS
