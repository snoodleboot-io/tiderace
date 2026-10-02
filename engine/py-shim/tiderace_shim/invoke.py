"""Calling a test, and setting up what it needs, written once for sync and async alike (TID-124).

The shim had two of everything here — `_setup_fixture` / `_setup_fixture_async`, `_teardown` /
`_teardown_async`, `_invoke_body` / `_invoke_async_body`, `_child_exec` / `_child_exec_async` —
whole bodies pasted with an `await` added, the `except` ladder byte-identical, and the async twin
skipping every isolation measurement: a feature gap born of the copy. Now there is one
implementation, written as the async one. The async tier runs it on the loop the run asked for;
the sync tier drives it with [`run_sync`], a trivial driver for a coroutine that never suspends.
Sync is the degenerate async.
"""
from __future__ import annotations

import asyncio
import inspect
import traceback
import unittest
from typing import Any, Callable

from .safe import safe_getattr


def skip_exceptions() -> tuple:
    """Every exception type that means "skip this test", not "this test broke".

    `unittest.SkipTest` is the obvious one. `pytest.skip()` and `pytest.importorskip()` raise
    `_pytest.outcomes.Skipped`, which derives from `BaseException` rather than `SkipTest` — so
    without it here a skip falls through to the catch-all and is reported as an error. A suite
    that skips a test because an optional backend is absent then shows up as broken."""
    try:
        from _pytest.outcomes import Skipped
    except Exception:  # noqa: BLE001 — pytest absent ⇒ unittest skips only
        return (unittest.SkipTest,)
    return (unittest.SkipTest, Skipped)


SKIP_EXCEPTIONS = skip_exceptions()


# ------------------------------------------------------------------------------ the sync driver
def run_sync(coro):
    """Drive a coroutine that never suspends — the sync tier's path through the one async
    implementation: a sync test, sync providers, no loop. It suspending means a provider or body
    awaited where nothing can run it; the only shape that does is a wider-scope async provider,
    which the shim has never driven (B5 drives function-scope ones on the test's own loop)."""
    try:
        coro.send(None)
    except StopIteration as stop:
        return stop.value
    coro.close()
    raise RuntimeError("an async provider awaited outside the test's event loop — a wider-scope "
                       "async provider is not supported")


async def settle(result, on_loop: bool) -> None:
    """What a test body returned, settled: a coroutine is awaited on the async tier (`on_loop` —
    whichever backend's loop the run put us on; probing for one would miss trio's) and run on a
    fresh asyncio loop on the sync tier, where nothing is running — the undetected `async def`,
    where a decorator hid the coroutine function from `inspect`; pytest fails such a test, the
    shim keeps running it as it always has. Anything else is left alone."""
    if not inspect.iscoroutine(result):
        return
    if on_loop:
        await result
    else:
        asyncio.run(result)


# ------------------------------------------------------------------------------ fixtures
class Request:
    """The minimal `request` object a fixture sees: `.param` and `.node`, plus `addfinalizer`."""

    __slots__ = ("param", "node", "_finalizers")

    def __init__(self, param, node=None):
        self.param = param
        self.node = node
        self._finalizers: list = []

    def addfinalizer(self, fn) -> None:
        """Run `fn` when this fixture tears down (TID-44).

        Tied to the fixture's own teardown handle, so a session-scoped fixture's finalizer runs at
        session end rather than after the first test. flask's fixtures use this for app and
        request-context cleanup; without it they raised `AttributeError` during setup."""
        self._finalizers.append(fn)


def run_finalizers(finalizers: list) -> None:
    """Run `request.addfinalizer` callbacks newest-first, as pytest does (TID-44).

    Guarded individually, matching a yield teardown: one finalizer raising must not stop the rest
    from releasing what they hold."""
    while finalizers:
        fn = finalizers.pop()
        try:
            fn()
        except Exception:  # noqa: BLE001 — a failing finalizer must not abort the remaining ones
            pass


def test_finalizers(request) -> None:
    """A test request's finalizers — after the body has fully run, including an awaited one."""
    if request is not None:
        run_finalizers(request._finalizers)


class FixtureHandle:
    """What a fixture's setup left to tear down: its generator — sync, or async for an
    `async def ... yield` provider — and the finalizers its `request` registered. One shape for
    every fixture (an empty one for a plain return), so a teardown never asks what it holds.

    Wrapped whenever the fixture takes a `request`, not only when it registered a finalizer during
    its own body. A fixture that hands the test a callable — flask's `purge_module` is the
    canonical shape — registers nothing at setup time and everything later, from inside the test
    body. Deciding at setup whether to keep the list therefore dropped exactly those finalizers,
    and a module a test asked to have purged stayed in `sys.modules` for its neighbours (TID-56).
    The list is shared by reference, so later appends are seen; an empty one tears down as
    cheaply as before. pytest registers a yield fixture's own teardown *after* the body returns,
    which makes it the newest finalizer: the yield teardown runs first, then the body's
    `addfinalizer` callbacks, newest first (TID-44)."""

    __slots__ = ("gen", "agen", "finalizers")

    def __init__(self, gen=None, agen=None, finalizers: list | None = None):
        self.gen = gen
        self.agen = agen
        self.finalizers = finalizers if finalizers is not None else []

    async def aclose(self) -> None:
        """The yield teardown — a teardown error must not abort the remaining finalizers — then
        the finalizers, newest first."""
        try:
            if self.agen is not None:
                await self.agen.__anext__()
            elif self.gen is not None:
                next(self.gen)
        except (StopIteration, StopAsyncIteration):
            pass
        except Exception:  # noqa: BLE001 — a teardown error must not abort remaining finalizers
            pass
        run_finalizers(self.finalizers)

    def close(self) -> None:
        """`aclose`, driven synchronously: a sync handle never suspends."""
        run_sync(self.aclose())


def owner_args(fdef) -> tuple:
    """The positional `self` a class fixture is called with, or `()` for an ordinary one.

    A fixture defined inside a test class is a plain function until it is looked up on an instance, so
    it needs one. pytest binds it to the class's instance; a fresh one per setup matches what fixture
    bodies actually use it for — reaching the class's own helpers — without tying fixture setup to the
    instance the test body will later run on (TID-47)."""
    return (fdef.owner(),) if fdef.owner is not None else ()


async def setup_fixture(fdef, args: dict, param, node) -> tuple[Any, FixtureHandle]:
    """Run a fixture body up to its first (a)yield, or to completion — a sync provider, an
    `async def` one, an `async def ... yield` one (B5). Returns `(value, handle)`. `node` is the
    test's `request.node`, shared with the test (TID-51)."""
    call_args = dict(args)
    request = Request(param, node) if fdef.wants_request else None
    if request is not None:
        call_args["request"] = request
    func = fdef.func
    gen = agen = None
    if inspect.isasyncgenfunction(func):
        agen = func(*owner_args(fdef), **call_args)
        value = await agen.__anext__()
    elif inspect.iscoroutinefunction(func):
        value = await func(*owner_args(fdef), **call_args)
    elif fdef.is_yield:
        gen = func(*owner_args(fdef), **call_args)
        value = next(gen)
    else:
        value = func(*owner_args(fdef), **call_args)
    return value, FixtureHandle(gen, agen, request._finalizers if request is not None else None)


# ------------------------------------------------------------------------------ the call
def call_hook(owner, names: tuple, *args) -> bool:
    """Call the first hook of `names` that `owner` defines, passing `args` if it accepts them.

    Both dialects are looked for at every level, because a suite mid-migration has files in each:
    unittest spells it `setUpModule`, pytest's xunit style spells it `setup_module`."""
    for name in names:
        hook = safe_getattr(owner, name, None)
        if hook is None or not callable(hook):
            continue
        try:
            signature = inspect.signature(hook)
            accepted = len([p for p in signature.parameters.values()
                            if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)])
        except (TypeError, ValueError):  # a builtin or C-level callable
            accepted = len(args)
        hook(*args[:accepted])
        return True
    return False


async def call_with_hooks(target, args: dict, hooks: tuple, request, *, on_loop: bool = True) -> None:
    """Call the test: its per-test xunit setup hook, the body — awaited if it is a coroutine, on
    the async tier (`on_loop`) — its teardown hook, which must not mask the body's outcome, then
    the finalizers its `request` registered, after the await (a coroutine's finalizers used to run
    before its body)."""
    setup, teardown = hooks
    try:
        if setup:
            call_hook(*setup)
        await settle(target(**args), on_loop)
    finally:
        if teardown:
            try:
                call_hook(*teardown)
            except Exception:  # noqa: BLE001 — teardown must not mask the body's outcome
                pass
        test_finalizers(request)


def classify_exception(exc: BaseException, introspect: Callable | None = None) -> tuple[str, str]:
    """The outcome an exception the body raised means, pytest's way (TID-30, verified against
    pytest directly): an `AssertionError` is a failure, with the rich diff `introspect` builds —
    lazy, so only a failed assert pays for it (ADR-E009); a skip exception is a skip; anything
    else is a failure too. pytest reserves `error` for a test it could not attempt — a fixture
    that raised, a module that would not import — and calls anything the body raises a failure,
    assertion or not. tiderace split on exception type instead, so `raise RuntimeError` reported
    `error` where pytest reports `failed`. Both are red, but the taxonomy leaked into the
    reporters and made the two runners impossible to reconcile."""
    if isinstance(exc, AssertionError):
        plain = "".join(traceback.format_exception_only(type(exc), exc))
        rich = introspect(exc) if introspect is not None else None
        return "failed", (rich + plain) if rich else plain
    if isinstance(exc, SKIP_EXCEPTIONS):
        return "skipped", str(exc)
    return "failed", "".join(traceback.format_exception_only(type(exc), exc))
