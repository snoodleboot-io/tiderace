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
import ast
import dataclasses
import difflib
import linecache
import sys
from typing import TYPE_CHECKING

from .nodes import class_method as _class_method, resolve_target
from .pytest_compat import _Node, _runtime_outcome
from .safe import safe_getattr as _safe_getattr
if TYPE_CHECKING:
    from .discovery import _Config


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


@dataclasses.dataclass
class XunitState:
    """The xunit setups this process has run (TID-60, TID-64): `done` holds `(kind, qualified
    name)` of every module / class setup, `classes` the class objects whose teardown runs at
    worker end, `failed` the `(outcome, detail)` of a class whose own setup did not complete — a
    `setUpClass` that skips or raises decides the outcome of EVERY method in the class, not only
    the one that happened to trigger it, which is what once-per-class means when the first attempt
    fails. A forked child inherits all of it, so a class the parent set up is not set up again."""

    done: set = dataclasses.field(default_factory=set)
    classes: dict = dataclasses.field(default_factory=dict)
    failed: dict = dataclasses.field(default_factory=dict)


@dataclasses.dataclass
class ProcessState:
    """What this worker process has done so far and holds (TID-124): the node it is running right
    now — fixtures and the test share one object, so a marker a *fixture* attaches and one the
    *test* attaches land in the same place, and the executor reads both when folding runtime
    markers into the outcome; how many nodes it has run — whether a test here has neighbours
    (TID-70); the xunit setups it has run; and the socket to its clean room, the pristine helper
    that re-runs demoted tests (TID-50), `None` until started and in every process forked before."""

    current_node: Any = None
    nodes_run: int = 0
    xunit: XunitState = dataclasses.field(default_factory=XunitState)
    clean_room: Any = None

    def node_for(self, node_id: str, func=None, instance=None):
        """The node object for `node_id`, reused for the whole test."""
        node = self.current_node
        if node is None or node.nodeid != node_id:
            node = self.current_node = _Node(node_id, func, instance)
        elif func is not None and node.function is None:
            node.function = func
        return node


class _TestRequest:
    """The `request` a TEST function sees — pytest's `FixtureRequest`, minus the fixture plumbing.

    Distinct from `Request` (what a *parametrized fixture* sees, which is only `.param`). A test
    asking for `request` overwhelmingly wants `request.config.getoption(...)` to decide whether to
    run, so `config` is the part that has to be real; the identity attributes are cheap and come
    along for free."""

    __slots__ = ("config", "node", "function", "cls", "instance", "param", "fixturenames", "_finalizers")

    def __init__(self, node_id: str, func, config: _Config, state: ProcessState, instance=None):
        self.config = config
        self.node = state.node_for(node_id, func, instance)
        self.function = func
        self.instance = instance
        self.cls = type(instance) if instance is not None else None
        self.param = None  # only a parametrized *fixture* has one; a test's request never does
        self.fixturenames = [p for p in inspect.signature(func).parameters
                             if p not in ("self", "cls")]
        self._finalizers: list = []

    def addfinalizer(self, fn) -> None:
        """Run `fn` after this test's body, before its function-scoped fixtures tear down (TID-44)."""
        self._finalizers.append(fn)


def _with_request(func, args: dict, node_id: str, config: _Config, state: ProcessState,
                  instance=None) -> tuple:
    """Add a `request` argument when the test asks for one (TID-14).

    `_bind_by_type` deliberately skips the name `request`, so it never resolves as a provider and
    the test was simply called without it — a `TypeError` about a missing positional argument. It
    is injected here instead of registered as a provider because it needs the node context that
    only the call site has."""
    if "request" in args or "request" not in inspect.signature(func).parameters:
        return args, None
    request = _TestRequest(node_id, func, config, state, instance)
    return {**args, "request": request}, request


def _child_fault_detail(exc: BaseException) -> str:
    """The diagnostic a fork child sends when it fails OUTSIDE the test body — fixture setup or
    teardown, the coverage probe, the purity snapshot. `_invoke` already formats body failures; this
    is the path that used to vanish into `no result from child`, so it names the stage explicitly and
    carries the full traceback (the child is about to `_exit`, so this is the only chance to say it)."""
    label = "the test body was never reached" if isinstance(exc, Exception) else type(exc).__name__
    try:
        trace = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
    except BaseException:  # noqa: BLE001 — a __repr__ that raises must not cost us the whole frame
        trace = "".join(traceback.format_exception_only(type(exc), exc))
    return f"fixture setup/teardown raised ({label}):\n{trace}"


def _on_loop(make_coro, backend=None, *, auto_mode: bool = False):
    """Run an async body to completion on the backend the run asked for.

    `anyio_backend` carries either a name (`"trio"`) or a name and its options
    (`("asyncio", {"debug": True})`), which is what the suite's own fixture yields. anyio's public
    `run()` is used rather than a reimplementation: it is the same entry point the plugin uses, and it
    is only reached when the suite already depends on anyio.

    Everything else — the overwhelming majority — keeps the plain asyncio path it always had."""
    if backend is None or auto_mode:
        return asyncio.run(make_coro())
    name, options = (backend, None)
    if isinstance(backend, (tuple, list)) and backend:
        name = backend[0]
        options = backend[1] if len(backend) > 1 else None
    try:
        import anyio
    except ImportError:  # the fixture named a backend but anyio is gone: asyncio is the best guess
        return asyncio.run(make_coro())
    if not isinstance(name, str):
        return asyncio.run(make_coro())
    try:
        return anyio.run(make_coro, backend=name, backend_options=options or {})
    except TypeError:
        # An option this anyio does not accept (`loop_factory` came late) must not fail the test for
        # a reason the test has nothing to do with; the backend itself is what matters.
        return anyio.run(make_coro, backend=name)


def _awaited_test(node_id: str, style: str, root: str) -> bool:
    """Whether the test body is `async def` **and** tiderace is the thing that must await it.

    A `unittest` class drives its own coroutines — `IsolatedAsyncioTestCase.run()` builds the loop and
    calls `asyncSetUp` around the body — so those are never async-driven from here, however the
    collector labelled them."""
    return resolve_target(node_id, style, root, lenient=True).is_async


async def _invoke(node_id: str, style: str, args: dict, config: _Config, state: ProcessState,
                  *, on_loop: bool) -> tuple[str, str]:
    """Call the test and say how it went, then fold in any marker it or its fixtures attached
    while running (TID-51). Written once for both tiers (TID-124): on the async tier (`on_loop`)
    the body is awaited if it is a coroutine; on the sync tier nothing ever suspends. A `unittest` method runs through its
    own result handling; a pytest-style one through its class's `setup_class` (once per class per
    process, TID-60) and its per-test xunit hooks."""
    node = resolve_target(node_id, style, config.run.root, lenient=style == "unittest_method")
    try:
        if node.is_unittest:
            outcome, detail = _invoke_unittest(state.xunit, node.module, node_id)
        else:
            if style == "class_method":
                _xunit_class_setup(state.xunit, node.cls)
                instance = node.cls()
                target = getattr(instance, node.name)
            else:
                instance, target = None, node.func
            call_args, request = _with_request(target, args, node_id, config, state, instance)
            hooks = _xunit_test_hooks(node.module, style, node_id, target)
            await call_with_hooks(target, call_args, hooks, request, on_loop=on_loop)
            outcome, detail = "passed", ""
    except (Exception, *SKIP_EXCEPTIONS) as exc:  # noqa: BLE001 — the body raised: pytest's taxonomy
        outcome, detail = classify_exception(exc, _introspect_assertion)
    return _runtime_outcome(state.current_node, outcome, detail)


class _SkipAwareResult(unittest.TestResult):
    """A `TestResult` that recognises pytest's `Skipped` as a skip rather than an error (TID-16).

    `unittest`'s executor special-cases exactly one skip type, `unittest.SkipTest`. pytest's `skip()`
    and `importorskip()` raise `_pytest.outcomes.Skipped`, which derives from `BaseException` and so
    falls through to the executor's bare `except:` and is recorded via `addError`. The result: on a
    `TestCase` (including `IsolatedAsyncioTestCase`), `pytest.importorskip("optional_dep")` — the
    standard way to skip when an extra is absent — reported as an ERROR, turning a clean run red for
    something that is not a defect.

    `addError` receives the live `(type, value, tb)`, so the verdict is made on the exception object
    itself; the alternative, reading it back out of `result.errors`, only ever sees a formatted
    string. Covers skips raised from `setUp` and `tearDown` too, which route here just the same."""

    def addError(self, test, err):  # noqa: N802 — unittest's own casing
        if isinstance(err[1], SKIP_EXCEPTIONS):
            self.addSkip(test, str(err[1]))
            return
        super().addError(test, err)


def _xunit_class_key(cls) -> tuple:
    return ("class", f"{_safe_getattr(cls, '__module__', '')}.{_safe_getattr(cls, '__name__', '')}")


def _xunit_module_setup(xunit: XunitState, module) -> None:
    """`setUpModule` / `setup_module`, once per module per process.

    Once per *process*, not per test: a forked run re-enters it in each child, which is the right
    reading since every child is its own interpreter, but repeating it for every in-process test
    would run a non-idempotent hook many times. Its teardown runs when the worker finishes with the
    module, via `teardown_all` (TID-60)."""
    key = ("module", _safe_getattr(module, "__name__", ""))
    if key in xunit.done:
        return
    xunit.done.add(key)
    call_hook(module, ("setUpModule", "setup_module"), module)


def _xunit_class_teardown(xunit: XunitState) -> None:
    """`tearDownClass` / `teardown_class` for every class this process set up, once each (TID-64).

    Runs before the module teardowns, since a class's teardown may still need its module. Only
    classes whose setup *completed* are torn down: one that skipped or raised never acquired
    whatever its teardown releases."""
    for key, cls in list(xunit.classes.items()):
        if key not in xunit.failed:
            try:
                call_hook(cls, ("tearDownClass", "teardown_class"), cls)
            except Exception:  # noqa: BLE001 — a teardown fault must not mask the run's results
                pass
        xunit.classes.pop(key, None)
        xunit.done.discard(key)


def _xunit_module_teardown(xunit: XunitState) -> None:
    """`tearDownModule` / `teardown_module` for every module this process set up."""
    for kind, name in list(xunit.done):
        if kind != "module":
            continue
        module = sys.modules.get(name)
        if module is not None:
            try:
                call_hook(module, ("tearDownModule", "teardown_module"), module)
            except Exception:  # noqa: BLE001 — a teardown fault must not mask the run's results
                pass
        xunit.done.discard((kind, name))


def _xunit_test_hooks(module, style: str, node_id: str, target) -> tuple:
    """`(setup, teardown)` call specs for the per-test xunit hooks, or `(None, None)`.

    A method's hooks live on its class and receive the method; a module-level test's live on the
    module and receive the function. unittest's own `setUp`/`tearDown` are deliberately absent:
    `TestCase.run()` calls those itself, and calling them here would double every one."""
    if style == "unittest_method":
        return (None, None)
    if style == "class_method":
        cls = _safe_getattr(module, _class_method(node_id)[0], None)
        if cls is None:
            return (None, None)
        owner = target.__self__ if hasattr(target, "__self__") else cls
        return ((owner, ("setup_method",), target), (owner, ("teardown_method",), target))
    return ((module, ("setup_function",), target), (module, ("teardown_function",), target))


def _xunit_class_setup(xunit: XunitState, cls) -> None:
    """pytest's `setup_class`, once per class per process. unittest's `setUpClass` is run by
    `_invoke_unittest`, which needs it inside its own result handling."""
    key = _xunit_class_key(cls)
    if key in xunit.done:
        return
    xunit.done.add(key)
    xunit.classes[key] = cls  # so `teardown_class` runs at worker end (TID-64) — it never did before
    call_hook(cls, ("setup_class",), cls)


def _invoke_unittest(xunit: XunitState, module, node_id: str) -> tuple[str, str]:
    """Run one `unittest.TestCase` method with fuller fidelity (Phase 4): honor `setUpClass`/
    `tearDownClass` (which `TestCase.run()` alone does NOT call), and map `@expectedFailure` /
    unexpected-success / `subTest` to the right node outcome.

    `setUpClass` runs once per class per *process* and `tearDownClass` once at worker end, the
    contract unittest's own runner keeps (TID-64). They used to run around every method — right when
    every test forked, since each child was its own process, and wrong under the in-process ladder,
    where a class's methods share one process: N× the setup cost, and a `setUpClass` that opens a
    database or counts its own calls behaved differently from `python -m unittest`. A forked child
    inherits the xunit state, so a class the parent set up is not set up again there either."""
    cls_name, method = _class_method(node_id)
    cls = module.__dict__[cls_name]
    key = _xunit_class_key(cls)
    if key in xunit.failed:
        return xunit.failed[key]
    if key not in xunit.done:
        xunit.done.add(key)
        xunit.classes[key] = cls
        try:
            cls.setUpClass()
        except SKIP_EXCEPTIONS as exc:  # setUpClass may skip the whole class
            xunit.failed[key] = ("skipped", str(exc))
            return xunit.failed[key]
        except Exception as exc:  # noqa: BLE001 — unittest errors every method of the class
            xunit.failed[key] = (
                "error", "".join(traceback.format_exception_only(type(exc), exc)))
            return xunit.failed[key]
    result = _SkipAwareResult()
    try:
        cls(method).run(result)
    except SKIP_EXCEPTIONS as exc:
        return "skipped", str(exc)

    if result.errors:
        # unittest files body, setUp and tearDown exceptions all under `errors`, and pytest reports
        # every one of those as FAILED — checked against pytest rather than assumed (TID-30). Only a
        # fixture fault stays an error, and that path never reaches here.
        return "failed", result.errors[0][1]
    if result.failures:  # includes subTest failures (each recorded with its sub-description)
        return "failed", result.failures[0][1]
    if getattr(result, "unexpectedSuccesses", None):
        return "failed", "unexpected success: a test marked @expectedFailure passed"
    if getattr(result, "expectedFailures", None):
        return "xfail", result.expectedFailures[0][1]
    if result.skipped:
        return "skipped", result.skipped[0][1]
    return "passed", ""


_CMP_OPS = {
    ast.Eq: "==", ast.NotEq: "!=", ast.Lt: "<", ast.LtE: "<=", ast.Gt: ">", ast.GtE: ">=",
    ast.In: "in", ast.NotIn: "not in", ast.Is: "is", ast.IsNot: "is not",
}


def _introspect_assertion(exc: AssertionError) -> str | None:
    """Rich diff for a failed bare `assert`, built by RE-EVALUATING the failing expression once with
    the live frame's locals/globals (ADR-E009 — lazy: passes cost nothing). Returns a formatted block
    (operand source + values + an element/line diff), or `None` to fall back to the plain message when
    it is unsafe/unsupported (re-eval raises → side-effecting or non-reproducing; not a single compare).
    """
    tb = exc.__traceback__
    if tb is None:
        return None
    while tb.tb_next is not None:  # deepest frame = where the assert raised
        tb = tb.tb_next
    frame, lineno, filename = tb.tb_frame, tb.tb_lineno, tb.tb_frame.f_code.co_filename

    node = _find_assert(filename, lineno)
    if node is None or not isinstance(node.test, ast.Compare) or len(node.test.ops) != 1:
        return None  # only single comparisons are introspected in this pass
    cmp = node.test
    op = _CMP_OPS.get(type(cmp.ops[0]))
    if op is None:
        return None
    try:
        left = _eval_stable(cmp.left, frame, filename)
        right = _eval_stable(cmp.comparators[0], frame, filename)
    except Exception:  # noqa: BLE001 — re-eval failed/unstable (impure/non-reproducing) → fall back
        return None

    lines = [
        "assertion failed (tiderace rich diff):",
        f"    {ast.unparse(cmp.left)} {op} {ast.unparse(cmp.comparators[0])}",
        f"    left  = {_short_repr(left)}",
        f"    right = {_short_repr(right)}",
    ]
    diff = _value_diff(left, right)
    if diff:
        lines.append("    diff:")
        lines.extend(f"      {d}" for d in diff)
    return "\n".join(lines) + "\n"


def _find_assert(filename: str, lineno: int):
    """The `ast.Assert` node at (or spanning) `lineno` in `filename`, or None."""
    src = "".join(linecache.getlines(filename))
    if not src:
        return None
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return None
    for node in ast.walk(tree):
        if isinstance(node, ast.Assert):
            end = getattr(node, "end_lineno", node.lineno)
            if node.lineno <= lineno <= (end or node.lineno):
                return node
    return None


class _NonReproducing(Exception):
    """Raised when an operand yields a different value on re-eval (side-effecting / nondeterministic),
    so the introspector falls back to the plain message instead of reporting a misleading diff."""


def _eval_stable(node, frame, filename):
    """Evaluate one operand in the failing frame's scope, **twice**, and only trust it if both evals
    agree — the ADR-E009 purity guard. A differing value (e.g. a counter/RNG/clock call) means the
    expression doesn't reproduce, so we refuse to build a diff that would misreport what failed."""
    code = compile(ast.Expression(body=node), filename, "eval")
    first = eval(code, frame.f_globals, frame.f_locals)  # noqa: S307 — our own re-eval, same scope
    second = eval(code, frame.f_globals, frame.f_locals)  # noqa: S307
    if not _reproduces(first, second):
        raise _NonReproducing()
    return first


def _reproduces(a, b) -> bool:
    """Whether two re-evals are equal. Conservative: any `==` that raises ⇒ treat as non-reproducing."""
    try:
        return bool(a == b)
    except Exception:  # noqa: BLE001
        return False


def _short_repr(value, limit: int = 300) -> str:
    try:
        r = repr(value)
    except Exception:  # noqa: BLE001
        r = f"<unreprable {type(value).__name__}>"
    return r if len(r) <= limit else r[:limit] + f"… (+{len(r) - limit} chars)"


def _value_diff(left, right) -> list[str]:
    """A small per-element / per-line diff for the common container/string cases (empty otherwise)."""
    if isinstance(left, str) and isinstance(right, str):
        d = list(difflib.unified_diff(left.splitlines(), right.splitlines(), "left", "right", lineterm=""))
        return d[:40]
    if isinstance(left, (list, tuple)) and isinstance(right, (list, tuple)):
        out = []
        if len(left) != len(right):
            out.append(f"length {len(left)} != {len(right)}")
        for i, (a, b) in enumerate(zip(left, right)):
            if a != b:
                out.append(f"[{i}] {_short_repr(a, 80)} != {_short_repr(b, 80)}")
            if len(out) >= 20:
                break
        return out
    if isinstance(left, dict) and isinstance(right, dict):
        out = []
        for k in sorted(set(left) | set(right), key=repr):
            if left.get(k) != right.get(k):
                out.append(f"[{_short_repr(k, 40)}] {_short_repr(left.get(k), 60)} != {_short_repr(right.get(k), 60)}")
            if len(out) >= 20:
                break
        return out
    return []
