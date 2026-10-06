"""The engine (TID-124, step 5): one `Engine` per worker process, holding what the run is configured
with, what discovery produced, the process's state and memos — `run()` is gate → plan → route →
execute → assemble — plus the module child for an opaque module (TID-80) and the clean room that
re-runs a demoted test from a pristine image (TID-50).
"""
from __future__ import annotations

import dataclasses
import itertools
import json
import os
import socket
import time
import traceback
import unittest

from .config import RunConfig
from .discovery import _Config, _skip_reason, Discovery
from .fixtures import _Active, _async_provider, _closure, _instance_key, _is_fixture, FixtureDef
from .footprint import _Coverage, _save_file_deps_cache, _watched_packages, Caches
from .invoke import (_awaited_test, _child_fault_detail, _invoke, _on_loop, _xunit_class_teardown,
                     _xunit_module_setup, _xunit_module_teardown, ProcessState, run_sync,
                     setup_fixture, SKIP_EXCEPTIONS as _SKIP_EXCEPTIONS)
from .isolation import _restorable, Isolation
from .log import warn as _warn
from .nodes import (import_module as _import_module, module_key as _module_key, resolve_target,
                    Target)
from .plan import (_aggregate, _disambiguate, _fixture_param_id, _generate_tests_marks,
                   _GenerateTestsError, _indirect_names, _param_value, _parametrize_cases,
                   _variant_parts, Plan)
from .protocol import (end_child, exit_text as _exit_text, EXIT_UNREPORTABLE as _EXIT_UNREPORTABLE,
                       read_frame as _read_frame, read_frame_by as _read_frame_by, reap, run_child,
                       spawn, Transport, write_frame as _write_frame)
from .pytest_compat import (_own_markers, _pytest_markers, normalise_all as _normalise_marks,
                            skip_reason as _mark_skip_reason)
from .results import (_note_import_history, empty_expansion, errored, expansion, Outcome,
                      purity_from, skipped, UNKNOWN_PURITY as _UNKNOWN_PURITY, variant, with_purity)
from .safe import safe_getattr as _safe_getattr
from .selection import keyword_names as _keyword_names, Selection
from .tiers import (_in_process_deadline, _InProcessTimeout, assemble, EngineOptions, route,
                    Routing, Tier, VariantResult)


class _ModuleChild:
    """The forked process running one opaque module's tests (TID-80)."""

    __slots__ = ("module_key", "pid", "req_w", "resp_r")

    def __init__(self, module_key: str, pid: int, req_w: int, resp_r: int):
        self.module_key, self.pid, self.req_w, self.resp_r = module_key, pid, req_w, resp_r


# Windows has no `fork()`. The isolation ladder's bottom rung (fork an opaque module) therefore doesn't
# exist there, so the shim must decide what to do instead rather than call `os.fork` and raise.
_FORK_AVAILABLE = hasattr(os, "fork")


class Engine:
    """Parent-side scope state: wider-than-function fixtures live here, inherited by forked children."""

    def __init__(self, discovery: Discovery, config: RunConfig, *, options: EngineOptions | None = None,
                 selection: Selection | None = None, state: ProcessState | None = None,
                 caches: Caches | None = None, no_fork: bool = False, coverage: bool = False,
                 purity_guard: bool = False, restore: bool = False, coverage_lines: bool = False):
        """`discovery` is what discovery produced — the registry, the conftests, their options, the
        collection hooks' skips; `config` the run — root, project, what it ignores, the modules it executes (TID-124);
        `options` the engine's knobs (`EngineOptions`), and the keyword booleans the same five for
        the callers that spell them out (the proofs); `selection` this run's `-k` / `-m` /
        `--strict-markers` — by default the project's own and the environment's, read now, after
        discovery, so a mark a conftest registered natively counts as declared; `state` what this
        process has done so far (`ProcessState`) and `caches` its memos (`Caches`), fresh unless the
        pool parent hands its own down."""
        self.discovery = discovery
        self.state = state if state is not None else ProcessState()
        self.caches = caches if caches is not None else Caches()
        self.reg = discovery.registry
        self.config = config
        self.selection = selection if selection is not None else Selection.load(config.project)
        self._pytest_config = _Config(config, discovery)  # `request.config`: one per run
        self.options = options or EngineOptions(no_fork=no_fork, restore=restore, purity_guard=purity_guard,
                                                coverage=coverage, coverage_lines=coverage_lines)
        self._leaked = None          # this test's unmodelled state drift, if any (TID-33)
        self._state_disturbed = False  # …and whether the node should be forked from now on
        self._disturbance = None  # what moved, kept for the verdict the clean-room handoff reports
        self._timed_out = False  # the in-process deadline ended the case (TID-93): no re-run
        self._module_child = None  # the live child running an opaque module's tests, if any (TID-80)
        self._guard = None  # the in-process module's entry snapshot, restored when we leave it (TID-81)
        self._in_module_child = False  # set in that child: run everything in-process, never fork
        self.active: list[_Active] = []  # in setup order (widest → narrowest)

    def apply_selection(self, patch: dict | None) -> None:
        """This run's `-k` / `-m` / `--strict-markers`, in a worker forked off a warm image (TID-90).

        The image read its selection at start-up — the project's own `addopts`, since a persistent
        parent is launched with none of this run's — and the gate consults it per node, so
        replacing it after the fork is the whole job (`Selection.override`)."""
        self.selection = self.selection.override(patch)

    def _value(self, name: str, module_key: str):
        # The most-recently set-up active instance of `name` is the one in scope for this test.
        for a in reversed(self.active):
            if a.fdef.name == name:
                return a.value
        raise KeyError(name)

    def _sync_wider(self, closure: list[FixtureDef], node_id: str) -> None:
        """Tear down active wider fixtures whose scope-instance no longer matches this test, then set
        up any missing wider fixtures the test needs (each exactly once per scope-instance)."""
        self.state.node_for(node_id)  # a wider-scope fixture is built for the test that first needed it
        self._teardown_stale(node_id)
        # pytest runs xunit `setup_module` / `setUpModule` as the first module-scoped autouse fixture,
        # so it precedes every module-scoped fixture of the file: a client a fixture builds sees what
        # the hook put in place — a started mock's credentials, a stub in `sys.modules`. It ran on the
        # test's own path here, after the wider fixtures were already live, and a moto mock started
        # in `setup_module` never reached the fixture-built client (TID-79). Before any wider fixture,
        # once per module per process; the later call on the test path is then a no-op.
        _xunit_module_setup(self.state.xunit, _import_module(_module_key(node_id), self.config.root))
        # Set up missing wider fixtures in topo order.
        live = {a.key for a in self.active}
        for d in closure:
            if d.rank == 0:
                continue
            key = _instance_key(d, node_id)
            if key in live:
                continue
            mk = _module_key(node_id)
            args = {param: self._value(prov, mk) for param, prov in d.bindings.items()}
            value, handle = run_sync(setup_fixture(d, args, None, self.state.current_node))
            self.active.append(_Active(d, key, value, handle))
            live.add(key)

    def _teardown_stale(self, node_id: str) -> None:
        """Tear down active wider fixtures whose scope-instance no longer matches this test, from the
        narrow end (active is ordered widest → narrowest). Then, if this test is the first of a new
        module, put back what the previous module changed (TID-81) — after its fixtures are gone,
        so a finalizer never runs against restored globals."""
        while self.active:
            top = self.active[-1]
            if top.key == _instance_key(top.fdef, node_id):
                break
            top.handle.close()
            self.active.pop()
        if self._guard is not None and self._guard.module_key != _module_key(node_id):
            self._leave_module()

    def _enter_module(self, module_key: str) -> None:
        """Snapshot the module the worker is entering, once, before its first in-process test: its
        globals, `os.environ`, `sys.modules`, the interpreter state the fingerprint watches, and the
        library containers this module's imports reach (TID-81). `_leave_module` restores all of it."""
        if self._guard is not None:
            if self._guard.module_key == module_key:
                return
            self._leave_module()
        try:
            mod = _import_module(module_key, self.config.root)
        except Exception:  # noqa: BLE001 — nothing to snapshot; nothing to put back either
            return
        watched = _watched_packages(self.caches, self.config.root, module_key)
        self._guard = Isolation.before(module_key, mod, watched, measure=True, full=True,
                                       registry_cache=self.caches.registry_targets)

    def _leave_module(self) -> None:
        """Restore the entered module's snapshot: the next module on this worker starts from the
        state this one found, whatever its tests did in between (TID-81)."""
        guard, self._guard = self._guard, None
        if guard is not None:
            guard.restore()

    def _gate(self, node_id: str, style: str, deadline_ms: int, force_no_fork: bool,
              trusted_pure: bool, recorded_must_fork: bool):
        """What ends a run before anything is built — a ready response — or the node's mark names
        and its node-level `-k` verdict, for `_plan`. In pytest's order: a directory a conftest
        skipped or broke, an inherited class (dispatched whole), a fixture the collector took for
        a test, the module's own import skip, `--strict-markers`, `-m`, `-k`."""
        module_key = _module_key(node_id)
        # Under a directory whose conftest skipped itself (TID-48): pytest never collects these, so
        # nothing about the node — its class, its marks, its module — may be touched.
        if self.config.module_ignored(module_key):
            return empty_expansion(node_id)
        # A conftest that did not import (TID-72), before the skip: a directory whose setup is broken
        # is broken for every test in it, and that is an error pytest would have stopped on.
        dir_error = self.discovery.dir_error(module_key)
        if dir_error is not None:
            return errored(node_id, dir_error)
        dir_skip = self.discovery.dir_skip(module_key)
        if dir_skip is not None:
            # `skip_origin` names the module that never imported, so the summary can report skips in
            # both dimensions (TID-55): a conftest's `importorskip` skips every test under it, and
            # "578 skipped" next to pytest's "94 skipped" reads as a defect until you can also say
            # how many *modules* those 578 came from. A per-test skip leaves this empty.
            return skipped(node_id, dir_skip, skip_origin=module_key)
        if style in ("inherited_methods", "unresolved_class"):
            return self._run_inherited(node_id, deadline_ms, force_no_fork, trusted_pure,
                                       own_too=style == "unresolved_class",
                                       recorded_must_fork=recorded_must_fork)
        # A `@pytest.fixture` whose name starts with `test` (anyio's `TestAsyncFile.testdata`) is
        # what the regex collector cannot tell from a test; pytest never collects it. Reported as
        # an empty expansion, like a deselected node: absent from the tally (TID-88).
        if self._is_fixture_node(node_id, style):
            return empty_expansion(node_id)
        # Deselected by the project's own `-m` filter (TID-32). Reported as an EMPTY expansion
        # rather than a skip: pytest deselects these, so they must not appear in the tally at all —
        # a skip would be a different, visible outcome.
        # A module that skips at import is skipped under any `-k` or `-m`: pytest's collection
        # skips it before either is consulted. The verdicts below used to come first, so a `-k`
        # that was a definite No at node level (`-k "not unit"` over `tests/unit/`) deselected
        # these where `-k nomatch` — undecided until the import — reported them; the daemon,
        # replaying the skip from its record (TID-102), reported them either way. The import is
        # the first thing now, as it is for pytest.
        try:
            _import_module(module_key, self.config.root)
        except _SKIP_EXCEPTIONS as exc:
            return skipped(node_id, _skip_reason(exc), skip_origin=module_key)
        except Exception:  # noqa: BLE001 — an unimportable module surfaces per node, below
            pass
        # Always, `-k` or not (TID-102): the names `-k` would match against are reported with the
        # result, so the daemon can take the verdict itself next time for a node nothing touched.
        names = _mark_names(node_id, style, self.config.root)
        # `--strict-markers`: a mark the project never declared is a typo far more often than an
        # intention, and pytest errors the item rather than running it. Silently ignoring the flag
        # meant `@pytest.mark.slwo` quietly ran a test its author had filtered out (TID-59).
        unknown = self.selection.unknown_marks(names, self.config.root)
        if unknown:
            return errored(node_id, f"{', '.join(unknown)} not found in `markers` configuration option")
        if not self.selection.marker_allows(names):
            return empty_expansion(node_id)
        # `-k` (TID-63), decided here when it can be: a No at node level is a No for every case the
        # node could produce, so it is deselected before a fixture is built or a skip mark is read —
        # pytest deselects at collection, and a deselected `@pytest.mark.skip` test is not a skip.
        # An "unknown" is settled per case once the case ids exist, below.
        keywords = _keyword_names(self.config, node_id, names)
        keyword_verdict = self.selection.keyword_verdict(keywords, final=False)
        if keyword_verdict is False:
            return empty_expansion(node_id, keywords=keywords)
        return names, keyword_verdict

    def _plan(self, node_id: str, style: str, names: set, keyword_verdict) -> "Plan | dict":
        """The node's plan — or the response that stands in for one: a collection failure, every
        case deselected by `-k`, or a whole-node skip (one skipped variant per selected case, as
        pytest collects a skip-marked parametrized test — TID-88)."""
        module_key = _module_key(node_id)
        try:
            node = resolve_target(node_id, style, self.config.root)
            requested = self._requested(node)
            marks = self._marks(node)
            # Inside the same guard as its siblings. It used to sit outside, so a failure expanding this
            # node's parametrize cases escaped `run()` and killed the whole worker — every other test on
            # it was lost and the run reported `shim closed mid-run` (TID-43). Whatever the next unsafe
            # probe turns out to be, it now costs this node an error rather than costing the worker.
            raw_cases = self._cases(node)
        except _GenerateTestsError as exc:
            return errored(node_id, str(exc))
        except _SKIP_EXCEPTIONS as exc:
            # A module-level `pytest.importorskip` / `pytest.skip(allow_module_level=True)`. Not an
            # `Exception`, so without this it escaped `run()` and took the worker with it (TID-48).
            # `skip_origin`: this module is the unit pytest would have reported one skip for (TID-55).
            return skipped(node_id, _skip_reason(exc), skip_origin=module_key)
        except Exception as exc:  # noqa: BLE001 — import/collection failure for this node
            return errored(node_id, "".join(traceback.format_exception_only(type(exc), exc)))

        # Native marks first, then anything a `@pytest.mark.skip` or a collection hook decided
        # (TID-20). Both short-circuit BEFORE any fixture setup — a test skipped for a missing
        # backend must not pay to build one.
        skip_reason = _mark_skip_reason(_normalise_marks(marks)) or self.discovery.marker_skips.get(node_id)
        # Applied once the case ids exist, below: pytest collects a skip-marked parametrized test
        # as one variant per case and skips each, so `test_lchmod[asyncio]`, `[trio]`, … are what
        # the tally holds — not one un-expanded `test_lchmod` (TID-88). Nothing is set up on the
        # way there: the ids come from the marks and the registry, never from a fixture. And while
        # `-k` is still undecided (TID-63) the skip waits for the same ids: pytest deselects at
        # collection, before it reads a skip mark, so a skip-marked test `-k` does not select is
        # absent from the tally rather than a skip in it.

        # Split requested params: fixtures (resolved by the graph) vs. bare params filled positionally
        # by @tiderace.cases. Without this, a parametrized test's params look like missing fixtures.
        #
        # A name the parametrize supplies is NOT a fixture request, even when a fixture of that name
        # exists: pytest's rule is that direct parametrization wins, and the value the author wrote
        # beside the test is the one that runs (TID-57). The collision is easy to hit — `history`,
        # `client`, `config` are ordinary words — and it only appears once the run root is wide enough
        # to have discovered the other module's fixture, so the same test passes on a narrow root and
        # errors on the whole package.
        parametrized = {name for case, *_ in raw_cases if isinstance(case, dict) for name in case}
        indirect = set(self._indirect(node))
        # A parametrized name that is not one of the function's parameters but names a fixture —
        # anyio's `@pytest.mark.parametrize("anyio_backend", ["asyncio"])` on a test that takes no
        # argument — sets that fixture's `request.param`: pytest routes it as an indirect
        # parametrize when the fixture is in the closure, and errors otherwise. Inferred here so
        # the closure is built with it, confirmed against the closure below (TID-88).
        inferred = {n for n in parametrized if n not in requested and self.reg.is_provider(n)}
        indirect |= inferred
        parametrized -= indirect  # indirect values go to the fixture, not the test
        fixture_requested = {
            p: t for p, t in requested.items()
            if p not in parametrized and self.reg.is_provider(t)
        }
        case_params = [p for p in requested if p not in fixture_requested]
        # `@tiderace.cases` yields positional variants; `@pytest.mark.parametrize`
        # yields name→value maps (argnames need not follow the signature order).
        case_kwargs_list = [
            c if isinstance(c, dict) else dict(zip(case_params, c.values))
            for c, *_ in raw_cases
        ] or [{}]
        # Author-supplied ids, aligned with `case_kwargs_list`; `None` ⇒ generate one. And each
        # value's position in its own parametrize axis, for the generated ids (TID-86).
        case_ids = [cid for _, cid, *_ in raw_cases] or [None]
        case_pos_maps = [(rest[0] if rest else None) for _, _, *rest in raw_cases] or [None]


        uses = self._uses(node)  # @tiderace.uses: set up by type, not injected (B2)
        # `@pytest.mark.usefixtures("a", "b")` — on the function, its class or its module — sets those
        # fixtures up around the test without passing them (TID-86). click's shell-completion tests
        # snapshot and restore a registry through exactly this, and without it the registry entry a
        # test adds is still there for the next.
        uses = list(uses) + [
            name for mark in _pytest_markers(node)
            if getattr(mark, "name", "") == "usefixtures"
            for name in getattr(mark, "args", ()) if isinstance(name, str) and name not in uses
        ]
        # A marker can imply a fixture request. `@pytest.mark.anyio` means "run me on the backends
        # `anyio_backend` describes" — the anyio plugin wires that up, and a test never names the
        # fixture itself. Adding it to the closure is enough to get the expansion: `anyio_backend` is
        # an ordinary parametrised fixture, so the combos below turn one test into one per backend,
        # with the suite's own ids. Without it each test ran once, silently covering one backend
        # where its author asked for three (TID-54).
        # Async tests only: the marker parametrises *how a coroutine is run*, so a synchronous test
        # in an anyio-marked module is one test, not one per backend — which is how pytest collects
        # it too.
        if (node.is_async and "anyio" in names
                and self.reg.is_provider("anyio_backend") and "anyio_backend" not in uses
                and "anyio_backend" not in requested and "anyio_backend" not in parametrized):
            uses = list(uses) + ["anyio_backend"]
        closure = _closure(self.reg, module_key, fixture_requested, uses, self._test_classes(node))
        if inferred:
            # Not in the closure after all: pytest reports "function uses no argument"; here the
            # value reaches the test as a keyword it never declared, which fails the same way.
            present = {d.name for d in closure}
            indirect -= {n for n in inferred if n not in present}
        # A fixture the test parametrizes *indirectly* takes the case's value as `request.param`;
        # its own `params` do not fan out as well — pytest yields `test[asyncio]` for an
        # `indirect=True` parametrize of `anyio_backend`, not one case per backend times one (TID-86).
        parametrized = [d for d in closure if d.params and d.name not in indirect]
        if parametrized:
            axes = [
                [(d.name, _param_value(p), _fixture_param_id(d, i, p), i)
                 for i, p in enumerate(d.params)]
                for d in parametrized
            ]
            product = list(itertools.product(*axes))
            combos = [{n: v for n, v, _, _ in c} for c in product]
            # Aligned with `combos`: the author's id per axis, or None where one must be generated,
            # and each value's position in its own axis — what pytest numbers an unprintable value
            # by (`bucket0-trio`, not the case's position across the product) (TID-86).
            combo_id_maps = [{n: i for n, _, i, _ in c} for c in product]
            combo_pos_maps = [{n: pos for n, _, _, pos in c} for c in product]
        else:
            combos = [{}]
            combo_id_maps = [{}]
            combo_pos_maps = [{}]

        parametrized_node = bool(combos != [{}] or case_kwargs_list != [{}])
        # Ids are computed for the WHOLE node up front: pytest indexes every member of a colliding
        # group, which cannot be decided while walking the variants one at a time.
        specs = [
            (combo, combo_ids, case_pos, case_kwargs, combo_pos)
            for combo, combo_ids, combo_pos in zip(combos, combo_id_maps, combo_pos_maps)
            for case_pos, case_kwargs in enumerate(case_kwargs_list)
        ]
        variant_ids = [
            # Brackets whenever the node IS parametrized, even when the id text is empty: a case
            # whose only value is `""` is `test_x[]` in pytest, which is not the same as an
            # unparametrized `test_x`.
            f"{node_id}[{text}]" if parametrized_node else node_id
            for text in _disambiguate([
                _variant_parts(combo, combo_ids, case_kwargs, i, case_ids[case_pos],
                               combo_pos, case_pos_maps[case_pos])
                for i, (combo, combo_ids, case_pos, case_kwargs, combo_pos) in enumerate(specs)
            ])
        ]
        # The cases `-k` keeps (TID-63): every one when the node was already a Yes, else each case
        # judged on its full id — `test_x[1-a]` is what `-k 1-a` was written to name.
        selected = set(range(len(variant_ids)))
        if keyword_verdict is None:
            selected = {i for i, vid in enumerate(variant_ids)
                        if self.selection.keyword_verdict(_keyword_names(self.config, vid, names), final=True)}
            if not selected:
                return empty_expansion(node_id, keywords=_keyword_names(self.config, node_id, names))
        if skip_reason is not None:  # the skip deferred above, one per selected variant (TID-88)
            if not parametrized_node:
                return skipped(node_id, skip_reason, keywords=_keyword_names(self.config, node_id, names))
            return skipped(node_id, skip_reason, keywords=_keyword_names(self.config, node_id, names),
                           variants=[variant(vid, Outcome.SKIPPED, skip_reason, 0,
                                             keywords=_keyword_names(self.config, vid, names))
                                     for i, vid in enumerate(variant_ids) if i in selected])
        return Plan(node, names, marks, requested, fixture_requested, closure, indirect,
                    case_kwargs_list, combos, combo_id_maps, parametrized_node, variant_ids, selected)

    def _route(self, module_key: str, force_no_fork: bool, trusted_pure: bool,
               recorded_must_fork: bool) -> Tier:
        """The node's tier (`tiers.route`), from this engine's configuration and state."""
        child = self._module_child
        return route(Routing(
            fork_available=_FORK_AVAILABLE, in_module_child=self._in_module_child,
            module_child_holds_module=child is not None and child.module_key == module_key,
            no_fork=self.options.no_fork, restore=self.options.restore, force_no_fork=force_no_fork,
            trusted_pure=trusted_pure, recorded_must_fork=recorded_must_fork,
        ), lambda: _restorable(_import_module(module_key, self.config.root)))

    def run(self, node_id: str, style: str, deadline_ms: int, force_no_fork: bool = False,
            trusted_pure: bool = False, recorded_must_fork: bool = False) -> dict:
        # `force_no_fork`: run THIS test in-process (no fork). On a trivial test that is ~90× cheaper than a
        # fork; on a real suite the win is smaller and depends on the parent's size (TID-18, TID-41).
        # The caller asserts it's pure (purity guard); the guard re-checks and flags any escapee.
        self.state.nodes_run += 1
        module_key = _module_key(node_id)
        gate = self._gate(node_id, style, deadline_ms, force_no_fork, trusted_pure, recorded_must_fork)
        if isinstance(gate, dict):
            return gate
        names, keyword_verdict = gate
        plan = self._plan(node_id, style, names, keyword_verdict)
        if isinstance(plan, dict):
            return plan
        # Only now — after `-k` has chosen and a whole-node skip has returned — does the node's
        # *route* get decided (TID-99): nothing above set anything up, so a node `-k` was about to
        # deselect never pays the restorability snapshot. `_route` is the one place the tier is
        # chosen (TID-123). An opaque module's tests run in ONE forked child, sequentially, for as
        # long as the batch stays on that module (TID-80): the child is the isolation boundary
        # between modules; inside it the file behaves as under pytest.
        tier = self._route(module_key, force_no_fork, trusted_pure, recorded_must_fork)
        if tier is Tier.MODULE_CHILD:
            return self._module_child_run(node_id, style, deadline_ms)
        results = self._execute(plan, tier, deadline_ms)
        if isinstance(results, dict):
            return results
        # The native marks first, then pytest's own `@pytest.mark.xfail` / `skip`, closest first
        # — both through one fold (TID-123). Without the second a test the author marked as
        # expected-to-fail was reported as a failure — one of click's two remaining divergences
        # (TID-63).
        resp = assemble(node_id, results, parametrized=plan.parametrized_node,
                        native_marks=_normalise_marks(plan.marks),
                        pytest_marks=_normalise_marks(reversed(_pytest_markers(plan.node))),
                        keywords=lambda nid: _keyword_names(self.config, nid, names))
        if any(r.disturbed for r in results):
            clean = self._clean_room_handoff(node_id, style, deadline_ms, results)
            if clean is not None:
                return clean
        return _note_import_history(resp, self.state.nodes_run)
        return _note_import_history(resp, self.state.nodes_run)

    def _execute(self, plan: Plan, tier: Tier, deadline_ms: int) -> "list[VariantResult] | dict":
        """Run every case `-k` kept on `tier`, each combo's wider fixtures synced first; a fixture
        that cannot be set up ends the node with an error (TID-34), else the variants' results."""
        node_id, style = plan.node.node_id, plan.node.style
        results: list[VariantResult] = []
        variant_index = 0
        per_combo = plan.per_combo
        for combo in plan.combos:
            if not any(i in plan.selected for i in range(variant_index, variant_index + per_combo)):
                variant_index += per_combo  # nothing here survives `-k`: build none of its fixtures
                continue
            try:
                self._sync_wider(plan.closure, node_id)
            except BaseException as exc:  # noqa: BLE001
                # A fixture that cannot be set up is an ordinary condition — pytest errors that test
                # and carries on. Letting it escape here killed the whole worker: every *other* test
                # on it was lost, and the run reported `shim closed mid-run`, naming the transport
                # rather than the fixture (TID-34). Same lesson as TID-15, one level up.
                return errored(node_id, "error setting up fixtures: "
                               + "".join(traceback.format_exception_only(type(exc), exc)))
            for case_kwargs in plan.case_kwargs_list:
                if variant_index not in plan.selected:
                    variant_index += 1  # deselected by `-k`: absent from the tally, as in pytest
                    continue
                started = time.perf_counter()
                self._state_disturbed = False
                self._disturbance = None
                self._timed_out = False
                # `indirect=` routes a case's value to the *fixture* of that name, as `request.param`,
                # and the test receives whatever the fixture returns (TID-58). The per-fixture param
                # map is what `combo` already is, so an indirect value simply joins it — and must be
                # kept out of the test's own kwargs, or the raw value would shadow the fixture's.
                case_combo, test_kwargs = combo, case_kwargs
                if plan.indirect and case_kwargs:
                    routed = {k: v for k, v in case_kwargs.items() if k in plan.indirect}
                    if routed:
                        case_combo = {**combo, **routed}
                        test_kwargs = {k: v for k, v in case_kwargs.items() if k not in plan.indirect}
                oc, detail, cov, purity = self._run_variant(
                    node_id, style, plan.fixture_requested, plan.closure, case_combo, deadline_ms,
                    test_kwargs, tier, plan.variant_ids[variant_index])
                # Per case, because only some cases of a parametrized node may trip (TID-33).
                results.append(VariantResult(plan.variant_ids[variant_index], oc, detail, cov, purity,
                                             self._state_disturbed,
                                             int((time.perf_counter() - started) * 1000)))
                variant_index += 1
        return results

    def _clean_room_handoff(self, node_id: str, style: str, deadline_ms: int,
                            attempt: "list[VariantResult] | None" = None) -> dict | None:
        """Re-run a node that disturbed interpreter state from the clean room's pristine image,
        and report THAT: its in-process result is not to be trusted, and this process is no longer
        a safe thing to fork (TID-50). `None` when there is nothing better to hand it to — no
        clean room, `--strategy subprocess` (in-process by configuration; the restore is the whole
        remedy), or the deadline is what ended it (TID-93: a re-run would block again, cost a
        second deadline, and replace the timeout's own message)."""
        if self.state.clean_room is None or self.options.no_fork or self._timed_out:
            return None
        # What the in-process attempt did is in the warning, since the clean run's result replaces
        # it: the disturbance, how long the attempt took, and how it ended — a deadline-long attempt
        # that "failed" is a hang the deadline ended, whatever the test turned the interrupt into.
        took = f"{sum(r.duration_ms for r in attempt)}ms" if attempt else "?"
        ended = ", ".join(f"{r.outcome}{': ' + r.detail.strip().splitlines()[-1][:100] if r.detail else ''}"
                          for r in attempt) if attempt else "?"
        _warn(f"re-running {node_id} from a clean image — it {self._disturbance or 'disturbed interpreter state'}; "
              f"the in-process attempt took {took} and ended {ended}")
        clean = _clean_room_run(self.state, node_id, style, deadline_ms)
        if clean is None:
            return None
        clean["must_fork"] = True
        # The clean run cannot observe what the first attempt did, and the verdict is about the
        # test, not about where it finally ran: it disturbed state, so it is impure and must not
        # take the in-process path again.
        clean["pure"] = False
        if self._disturbance:
            clean["impurity"] = f"disturbed interpreter state: {self._disturbance}"
        return _note_import_history(clean, self.state.nodes_run, pristine=True)

    def _run_inherited(self, node_id: str, deadline_ms: int, force_no_fork: bool,
                       trusted_pure: bool, own_too: bool = False,
                       recorded_must_fork: bool = False) -> dict:
        """Run the test methods a class INHERITS rather than defines (TID-26).

        Collection scans source text, so `class TestKuzuConformance(GraphStoreConformance)` looks
        like a class with no tests — on a real corpus that silently dropped 129 tests, every backend
        conformance suite among them, and the run stayed green. Only something holding the live class
        can see through to the base, so the shim resolves it here and reports one result per method.

        Methods defined in the class's OWN body are excluded by default: the source scan already
        collected those, and running them here too would double-count them. `own_too` inverts that
        for a class the scan did not recognise at all (`unresolved_class`), where it collected
        nothing and this is the only report of the class's tests."""
        module_key = _module_key(node_id)
        cls_name = node_id.partition("::")[2]
        try:
            module = _import_module(module_key, self.config.root)
            cls = getattr(module, cls_name)
        except Exception as exc:  # noqa: BLE001 — a class we can't resolve contributes nothing
            return expansion(node_id, Outcome.ERROR,
                             "".join(traceback.format_exception_only(type(exc), exc)), [])

        # pytest's rule: a `Test*` class, or any `unittest.TestCase` subclass whatever its name.
        # `PackOverridesBuiltinTests` is the second kind, which is why the name scan missed it.
        if own_too and not (
            cls.__name__.startswith("Test") or issubclass(cls, unittest.TestCase)
        ):
            return empty_expansion(node_id)

        own = set() if own_too else set(vars(cls))
        inherited = sorted(
            name for name in dir(cls)
            if name.startswith("test") and name not in own and callable(getattr(cls, name, None))
        )
        # `expanded` says "these variants are the whole answer", so an empty list means this class
        # contributes nothing — distinct from a node that simply isn't parametrized.
        if not inherited:
            return empty_expansion(node_id)

        style = "unittest_method" if issubclass(cls, unittest.TestCase) else "class_method"
        variants = []
        for name in inherited:
            child = f"{module_key}::{cls_name}::{name}"
            started = time.perf_counter()
            res = self.run(child, style, deadline_ms, force_no_fork, trusted_pure, recorded_must_fork)
            # A parametrized inherited method expands again; splice its cases in rather than nesting.
            if res.get("variants"):
                variants.extend(res["variants"])
                continue
            # A child that expanded to nothing was deselected — `-m` or `-k` said no, and `run()`
            # answered with the empty expansion pytest's absence-from-the-tally means. Building a
            # variant from that answer's placeholder outcome reported every deselected inherited
            # method as a pass that never ran: 101 of them on pirn-agents under `-k nomatch` (TID-74).
            if res.get("expanded"):
                continue
            child_result = variant(child, res["outcome"], res.get("detail", ""),
                                   int((time.perf_counter() - started) * 1000))
            if res.get("coverage"):
                child_result["coverage"] = res["coverage"]
            if "pure" in res:
                child_result["pure"] = res["pure"]
            if res.get("must_fork"):
                child_result["must_fork"] = True
            if res.get("keywords"):
                child_result["keywords"] = res["keywords"]
            variants.append(child_result)
        # Every child deselected ⇒ the class contributes nothing, exactly as an inherited-nothing
        # class does above. `_aggregate` of an empty list is `max()` of nothing, and that exception
        # escaping `run()` took the whole worker down — "shim closed mid-run" for a `-k` that matched
        # no inherited method (TID-74, the shape TID-43 was about).
        if not variants:
            return empty_expansion(node_id)
        worst_outcome, worst_detail = _aggregate([(v["outcome"], v.get("detail", "")) for v in variants])
        return expansion(node_id, worst_outcome, worst_detail, variants)

    # ------------------------------------------------------------------ module child (TID-80)
    def _module_child_run(self, node_id: str, style: str, deadline_ms: int) -> dict:
        """Run this node in the live child for its module, forking one if there is none (or the live
        one serves another module). Everything the child does not report is reported here: a death
        names its exit, a hang its timeout, and either drops the child so the next node gets a fresh
        one rather than a dead pipe."""
        module_key = _module_key(node_id)
        child = self._module_child
        if child is not None and child.module_key != module_key:
            self._module_child_close()
            child = None
        if child is None:
            # Stale wider fixtures go before the fork, in the process that owns them: the child must
            # never tear down what the parent will tear down again.
            self._teardown_stale(node_id)
            child = self._module_child_spawn(module_key)
        try:
            _write_frame(child.req_w, {"node_id": node_id, "style": style, "deadline_ms": deadline_ms})
        except OSError:
            status = self._module_child_reap()
            return errored(node_id, "the module's child process was gone before this test could be "
                           f"sent to it ({_exit_text(status)})")
        data, timed_out = _read_frame_by(child.resp_r, time.monotonic() + deadline_ms / 1000.0)
        if timed_out:
            self._module_child_kill()
            return errored(node_id, "timeout")
        if data is None:  # EOF without a frame: the child died on this test
            status = self._module_child_reap()
            return errored(node_id, f"the module's child process died running this test "
                           f"({_exit_text(status)}); the module's remaining tests run in a fresh one")
        try:
            return json.loads(data.decode())
        except (ValueError, UnicodeDecodeError) as exc:
            self._module_child_kill()
            return errored(node_id, f"child sent an unreadable result frame ({exc}); "
                           f"{len(data)} bytes received")

    def _module_child_spawn(self, module_key: str):
        req_r, req_w = os.pipe()
        resp_r, resp_w = os.pipe()

        def child() -> int:  # ---- CHILD: this module's tests, in-process, until the parent closes the pipe
            os.close(req_w)
            os.close(resp_r)
            self._in_module_child = True
            # pytest's semantics inside the file: nothing is undone between tests, nothing measured.
            self.options = dataclasses.replace(self.options, restore=False, purity_guard=False)
            self._module_child = None
            inherited = len(self.active)  # the parent's fixtures: its to tear down, not ours
            done_before = set(self.state.xunit.done)  # likewise the parent's xunit hooks
            def handle(req: dict) -> dict:
                try:
                    return self.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                                    force_no_fork=True)
                except BaseException as exc:  # noqa: BLE001 — report it; never die silently
                    return errored(req["node_id"], _child_fault_detail(exc)[:4000])

            code = 0
            try:
                Transport(req_r, resp_w).serve(handle)
            except BaseException:  # noqa: BLE001 — an unsendable frame or a closed parent
                code = _EXIT_UNREPORTABLE
            finally:
                try:
                    while len(self.active) > inherited:
                        self.active.pop().handle.close()
                    for key in done_before:
                        self.state.xunit.done.discard(key)
                    _xunit_class_teardown(self.state.xunit)
                    _xunit_module_teardown(self.state.xunit)
                except BaseException:  # noqa: BLE001 — a teardown fault must not mask the results
                    pass
            return code

        pid = spawn(child)
        os.close(req_r)
        os.close(resp_w)
        self._module_child = _ModuleChild(module_key, pid, req_w, resp_r)
        return self._module_child

    def _module_child_close(self) -> None:
        """End the live child gracefully: EOF on its request pipe, its teardown, its exit."""
        child = self._module_child
        if child is None:
            return
        try:
            os.close(child.req_w)
        except OSError:
            pass
        end_child(child.pid, 30.0)
        try:
            os.close(child.resp_r)
        except OSError:
            pass
        self._module_child = None

    def _module_child_kill(self) -> int:
        return self._module_child_reap(kill=True)

    def _module_child_reap(self, kill: bool = False) -> int:
        child = self._module_child
        if child is None:
            return 0
        status = reap(child.pid, kill=kill)
        for fd in (child.req_w, child.resp_r):
            try:
                os.close(fd)
            except OSError:
                pass
        self._module_child = None
        return status

    def _run_variant(self, node_id, style, requested, closure, combo, deadline_ms, case_kwargs,
                     tier: Tier, variant_id) -> tuple:
        """Run one (combo, case) variant on `tier`; returns `(outcome, detail, coverage, purity)`
        where purity is a reason string (impure), `None` (measured pure), or `_UNKNOWN_PURITY` (not
        measured). The in-process tiers run it in THIS process (the bare one without a snapshot);
        the fork tier in a pristine copy-on-write child."""
        case_kwargs = case_kwargs or {}

        # No fork on this platform (Windows) and the module needs one to be isolated. Refuse rather
        # than run it: in-process would leak un-restorable state into the next test on this module, and
        # a wrong green is worse than a reported error. Previously this fell through to `os.fork()` and
        # raised an uncaught AttributeError, killing the worker.
        if tier is Tier.REFUSED:
            return ("error",
                    f"cannot isolate {node_id}: its module has state that can't be snapshot-restored, "
                    f"so it requires fork() — unavailable on this platform. Make the module's globals "
                    f"deep-copyable, or mark the test pure if it doesn't mutate shared state.",
                    {}, _UNKNOWN_PURITY)

        if tier.in_process:
            # No-COW fallback: run the test in THIS process (no isolation, but the same fixture
            # engine → result-identical outcomes; §8 boundary 3). Function fixtures are set up and
            # torn down per test in-process; wider scopes still live once in the parent.
            self._leaked = None
            try:
                # The deadline holds here too (TID-93): a forked child is killed when it overruns,
                # but a test that blocks on this tier used to block the worker, and the run.
                with _in_process_deadline(deadline_ms) as deadline:
                    result = self._child_exec(node_id, style, requested, closure, combo, case_kwargs,
                                              variant_id=variant_id, tier=tier)
            except _InProcessTimeout as exc:
                # The test was interrupted mid-body: whatever it held is not torn down, so this
                # process is not to be trusted with the next in-process test — the node forks from
                # now on (TID-33's must-fork), where the deadline can kill instead of interrupt.
                # The watchdog delivers the bare class (TID-98): the message is the deadline's.
                detail = str(exc) if exc.args else deadline.message
                self._state_disturbed = True
                self._disturbance = detail
                self._timed_out = True
                return "error", detail, {}, _UNKNOWN_PURITY
            except BaseException as exc:  # noqa: BLE001 — any in-process test error → Outcome::Error
                return "error", "".join(traceback.format_exception_only(type(exc), exc)), {}, _UNKNOWN_PURITY
            # `_child_exec` sets this when the fingerprint moved in a way nothing undid (TID-33), so
            # the in-process result cannot be trusted and neither can this process. Re-run the test
            # in a fork — a pristine copy — and report THAT, which fixes the current run rather than
            # only teaching the next one. `_FORK_AVAILABLE` is false on Windows, where there is
            # nothing better to fall back to, so the in-process answer stands there. Neither is
            # `--strategy subprocess`, where running in-process is the configured strategy rather
            # than this run's optimistic guess: there the restore above is the whole remedy.
            drift, self._leaked = self._leaked, None
            if drift is not None:
                # Recorded even on the tiers that cannot act on it now (`--strategy subprocess`,
                # Windows): the fact is true about the test, and a later run under the ladder is
                # exactly who needs it. Note this is NOT the `must_fork` parameter above, which says
                # the test's *module* is unrestorable; this says the test disturbed the interpreter.
                self._state_disturbed = True
                self._disturbance = drift
            if drift is not None and self.state.clean_room is not None and not self.options.no_fork:
                # The clean room re-runs the whole node from a pristine image; `run()` above does the
                # handoff and reports THAT. Forking here would fork the process this test just
                # dirtied — if what it leaked was a thread, straight into a deadlock (TID-50).
                return result
            if drift is not None and _FORK_AVAILABLE and not self.options.no_fork:
                _warn(f"re-running {node_id} in a fork — it {drift}")
                oc, detail, cov, _ = self._run_variant(
                    node_id, style, requested, closure, combo, deadline_ms, case_kwargs,
                    Tier.FORK, variant_id)
                # Keep the impurity verdict: the point is that this node must not take the
                # in-process path again, and the forked run cannot observe what the first one did.
                return oc, detail, cov, f"disturbed interpreter state: {drift}"
            return result

        def body() -> dict:
            # ---- CHILD: pristine COW copy with all wider fixtures already warm ----
            try:
                outcome, detail, coverage, purity = self._child_exec(
                    node_id, style, requested, closure, combo, case_kwargs, variant_id=variant_id,
                    tier=Tier.FORK)
                payload = {"outcome": outcome, "detail": detail[:4000]}
                if coverage:
                    payload["coverage"] = coverage
                # Carry the purity tri-state across the pipe: pure=True/False when measured (guard on),
                # omitted when unknown (the default forked path measures nothing).
                return with_purity(payload, purity, reason_key="impurity")
            except BaseException as exc:  # noqa: BLE001 — report it; never die silently (TID-15)
                # `_invoke` guards the test BODY only, so anything raised by fixture setup/teardown,
                # the coverage probe, or the purity snapshot lands here. Swallowing it exited 0 with an
                # empty pipe, and the parent could say no more than "no result from child" — a defect
                # indistinguishable, from the outside, from a test that genuinely failed. Send the
                # traceback back instead so the failure names its own cause.
                return {"outcome": "error", "detail": _child_fault_detail(exc)[:4000]}

        # The deadline covers the WHOLE exchange, not just the first byte (TID-31): a child that
        # wrote part of its frame and then hung used to satisfy the first-byte wait and block the
        # parent in `read` forever — taking the worker, and every remaining test in its batch,
        # with it. A frame larger than the 64 KB pipe buffer (a long traceback, a rich diff, a wide
        # coverage map) is written across several `write` calls, so this is reachable.
        got = run_child(body, deadline_ms / 1000.0)
        if got.timed_out:
            if got.received:
                # Distinct from a silent timeout on purpose: a child that produced half a frame is a
                # different fault from one that produced nothing, and saying which is the whole
                # point of TID-15.
                return ("error",
                        f"timeout after writing {got.received} bytes of a partial result frame — the "
                        f"child began reporting and then stopped", {}, _UNKNOWN_PURITY)
            return "error", "timeout", {}, _UNKNOWN_PURITY
        if got.reply is None and got.error is None:
            if got.signaled:
                return "error", f"child killed by signal {os.WTERMSIG(got.status)}", {}, _UNKNOWN_PURITY
            code = got.exit_code
            if code == _EXIT_UNREPORTABLE:
                return ("error",
                        "child ran the test but could not serialise its result frame — the outcome is "
                        "lost. Most likely an unserialisable coverage map or a detail string that is "
                        "not valid JSON.", {}, _UNKNOWN_PURITY)
            if code:
                return "error", f"child exited {code}", {}, _UNKNOWN_PURITY
            # The child now reports its own faults (TID-15), so reaching here means it left without
            # running the handler at all — `os._exit`/`os.abort` from inside the test, or the runtime
            # dying between fork and the first frame.
            return ("error",
                    "child exited 0 without sending a result — the test process terminated itself "
                    "(os._exit/os.abort) or the interpreter died before the result frame was written.",
                    {}, _UNKNOWN_PURITY)
        if got.error is not None:
            # A truncated or corrupt frame (child killed mid-write) must stay a reported error — letting
            # it raise here would take the worker down with it and lose the whole batch, not one test.
            return ("error",
                    f"child sent an unreadable result frame ({got.error}); {got.received} bytes received",
                    {}, _UNKNOWN_PURITY)
        res = got.reply
        # Reconstruct the purity tri-state from the pipe (`purity_from`: omitted ⇒ unknown).
        return res["outcome"], res.get("detail", ""), res.get("coverage", {}), purity_from(res)

    def _child_exec(self, node_id, style, requested, closure, combo, case_kwargs=None, variant_id=None,
                    tier: Tier = Tier.FORK) -> tuple:
        """Set up the function-scope fixtures (incl. parametrized + reinit-after-fork resources,
        which thus get a FRESH handle per child), run the body, tear down in reverse — in the
        forked child, or in this process on the in-process tiers. `case_kwargs` are the
        @tiderace.cases values bound to the test's bare params. Returns `(outcome, detail,
        coverage, purity)` where coverage is `{rel_path: [lines]}` (empty unless enabled).

        One path for sync and async (TID-124): `_run_case` is written once, as a coroutine. An
        async test body, or any function-scope async provider, runs it on ONE event loop — objects
        created on a loop must be awaited on the same loop (B5) — on the backend the run asked
        for; everything else drives it with `run_sync`, where nothing ever suspends. So an async
        test gets the same isolation measurement, the same module guard and the same
        `request.node` as a sync one."""
        module_key = _module_key(node_id)
        cov = _Coverage(self.config.root, self.options.coverage, self.options.coverage_lines, self.caches)
        cov.start()  # capture the per-test footprint: fixture setup + body, this test only (ADR-E006)
        # Named as pytest names it, parametrize id included: a fixture keying a resource off
        # `request.node.name` needs `test_x[case]`, not `test_x`, or every case collides (TID-51).
        self.state.node_for(variant_id or node_id)
        # `setUpModule` / `setup_module` before any fixture or test body: a suite uses it to put
        # something in place for the whole file — stubbing an optional SDK in `sys.modules`, say —
        # and without it every test in that file fails on the thing it was meant to provide (TID-60).
        _xunit_module_setup(self.state.xunit, _import_module(module_key, self.config.root))
        try:
            awaited = _awaited_test(node_id, style, self.config.root) or any(
                _async_provider(d.func) for d in closure if d.rank == 0)

            def case():
                return self._run_case(node_id, style, requested, closure, combo, case_kwargs, tier, cov,
                                      on_loop=awaited)

            if awaited:
                outcome, detail, purity = _on_loop(case, combo.get("anyio_backend"),
                                                   auto_mode=self.config.force_asyncio)
            else:
                outcome, detail, purity = run_sync(case())
            # Closure merged in here, not inside `_Coverage`, so the capture object stays purely
            # about what executed and the module attribution is visible at the call site.
            return outcome, detail, cov.report_with_imports(module_key), purity
        finally:
            cov.stop()  # idempotent — frees the monitoring tool id even if setup raised

    async def _run_case(self, node_id, style, requested, closure, combo, case_kwargs, tier: Tier,
                        cov: "_Coverage", *, on_loop: bool) -> tuple:
        """The case itself: function-scope fixtures up, the isolation snapshot the tier calls for,
        the body, the verdict — the footprint stops there, before the fixtures come down in
        reverse. Returns `(outcome, detail, purity)`."""
        module_key = _module_key(node_id)
        local: dict[str, object] = {}
        handles: list = []

        def value_of(name: str):
            if name in local:
                return local[name]
            return self._value(name, module_key)

        try:
            for d in closure:
                if d.rank != 0:
                    continue  # wider scopes are already live in inherited parent memory
                args = {param: value_of(prov) for param, prov in d.bindings.items()}
                val, handle = await setup_fixture(d, args, combo.get(d.name), self.state.current_node)
                local[d.name] = val
                handles.append(handle)
            test_args = {param: value_of(prov) for param, prov in requested.items()}
            if case_kwargs:
                test_args.update(case_kwargs)
            # Purity guard / restore: snapshot shared state right before the body, compare right after.
            # When running in-process (no fork) with `restore`, undo any mutation so the next test is
            # isolated WITHOUT a fork — the snapshot/restore fast path for impure tests too.
            # `trusted_pure` (TID-1): a recorded-pure, unchanged test skips the snapshot entirely and runs
            # BARE no-fork — no measurement, no restore, no isolation. Worth ~3.4× where it applies and
            # usually applies to few tests: anything recording into shared state is not pure (TID-41).
            # Otherwise snapshot to measure/restore.
            # The isolation level follows from the tier: an in-process test under restore enters
            # the module guard and takes the full set of snapshots (TID-81); the bare tier measures
            # nothing (TID-1); otherwise the purity guard decides whether to measure.
            full = self.options.restore and tier.in_process
            measure = tier is not Tier.BARE and (full or self.options.purity_guard)
            if full:
                self._enter_module(module_key)
            mod = _import_module(module_key, self.config.root) if measure else None
            watched = _watched_packages(self.caches, self.config.root, module_key) if full else ()
            isolation = Isolation.before(module_key, mod, watched, measure=mod is not None, full=full,
                                         registry_cache=self.caches.registry_targets)
            outcome, detail = await _invoke(node_id, style, test_args, self._pytest_config, self.state,
                                            on_loop=on_loop)
            # Everything below MEASURES; nothing here restores: the restore that stands in for a
            # fork happens when this worker leaves the module (`_leave_module`), against the
            # snapshot taken when it entered (TID-80, TID-81). The per-test verdict still says what
            # each test touched: it decides the bare tier (TID-1) and it is what a reader of
            # `--report` wants to know. What leaked — a thread — tells the caller to discard this
            # result and re-run it forked (TID-33, TID-50).
            purity, leaked = isolation.verdict()
            if leaked is not None:
                self._leaked = leaked
            cov.stop()
            return outcome, detail, purity
        finally:
            for handle in reversed(handles):
                await handle.aclose()

    def _is_fixture_node(self, node_id: str, style: str) -> bool:
        """Whether the object a node names is a fixture rather than a test (TID-88)."""
        if style == "unittest_method":
            return False
        try:
            obj = resolve_target(node_id, style, self.config.root).func
        except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — a module that skips itself at
            return False  # import raises a BaseException here; the run below reports it (TID-48)
        return _is_fixture(obj)

    def _requested(self, node: Target) -> dict:
        """The resources a test requests, as `param_name -> provider_name` bindings. Native params
        resolve by **type** (ADR-E012); untyped params fall back to name (the pytest path), so a
        pytest-authored test with `(db, cache)` args binds identically to before."""
        if node.style == "unittest_method":
            return {}  # unittest methods drive their own setUp/tearDown; no DI in Phase 3
        return self.reg.bind_params(node.func)

    def _marks(self, node: Target) -> list:
        """The native marks (`__tiderace_marks__`) on a test, read by attribute — the tiderace-owned
        analogue of pytest's marker read. unittest methods carry none."""
        if node.style == "unittest_method":
            return []
        return list(getattr(node.func, "__tiderace_marks__", ()))

    def _test_classes(self, node: Target) -> tuple:
        """The names in the test class's MRO, narrowest first — empty for a plain function.

        Fixtures defined inside a test class are visible to that class and its subclasses only, so the
        closure needs to know which class the node belongs to (TID-47)."""
        if node.cls is None:
            return ()
        mro = _safe_getattr(node.cls, "__mro__", None) or ()
        return tuple(c.__name__ for c in mro)

    def _indirect(self, node: Target) -> set:
        """Argnames this node's `parametrize` marks route through a fixture (`indirect=`)."""
        if node.style == "unittest_method":
            return set()
        hook_marks = self._hook_marks(node)
        if node.style == "class_method":
            return _indirect_names(node.func, node.cls, node.module, hook_marks=hook_marks)
        return _indirect_names(node.func, node.module, hook_marks=hook_marks)

    def _uses(self, node: Target) -> list:
        """Provider names a test depends on via `@tiderace.uses(Type, ...)` — resolved by type, set up
        in the closure but never passed as args (the native `usefixtures`). unittest carries none."""
        if node.style == "unittest_method":
            return []
        names = []
        for t in getattr(node.func, "__tiderace_uses__", ()):
            provs = self.reg.by_type.get(t, [])
            if len(provs) == 1:  # unambiguous; ambiguity is the author's to disambiguate
                names.append(provs[0])
        return names

    def _cases(self, node: Target) -> list:
        """The variants of a test: native `@tiderace.cases`, else `@pytest.mark.parametrize`.

        unittest has neither — pytest cannot parametrize a `TestCase` method
        either, so the early return matches the oracle.
        """
        if node.style == "unittest_method":
            return []
        native = list(getattr(node.func, "__tiderace_cases__", ()))
        if native:
            return [(c, None, None) for c in native]  # native cases carry no author-supplied id
        # The class and the module too: pytest applies their marks to every test they hold (TID-53).
        owner = node.cls if node.style == "class_method" else None
        return _parametrize_cases(node.func, *(o for o in (owner, node.module) if o is not None),
                                  hook_marks=self._hook_marks(node))

    def _hook_marks(self, node: Target) -> list:
        """The parametrize axes this node's `pytest_generate_tests` hooks declare (TID-85). Nothing
        to run ⇒ nothing computed: a suite without the hook pays a dictionary lookup."""
        node_id, module, func = node.node_id, node.module, node.func
        owner = node.cls if node.style == "class_method" else None
        cached = self.discovery.hook_marks.get(node_id)
        if cached is not None:
            return cached
        if (_safe_getattr(module, "pytest_generate_tests", None) is None
                and not self.discovery.generate_tests_hooks):
            return []
        requested = self._requested(node)  # param → provider (or the bare name)
        names = list(requested)
        try:
            providers = {p: t for p, t in requested.items() if self.reg.is_provider(t)}
            closure = _closure(self.reg, node.module_key, providers, [], self._test_classes(node))
            names = list(dict.fromkeys(names + [d.name for d in closure]))
        except Exception:  # noqa: BLE001 — an unresolvable request is the test's problem, later
            pass
        return _generate_tests_marks(node_id, func, module, owner, names,
                                     _own_markers(module, owner, func), self._pytest_config, self.discovery)

    def teardown_all(self) -> None:
        _save_file_deps_cache(self.caches, self.config.root)  # what this worker parsed (TID-82)
        self._module_child_close()  # its module's tests are done: its fixtures, hooks and exit (TID-80)
        while self.active:
            self.active.pop().handle.close()
        _xunit_class_teardown(self.state.xunit)  # tearDownClass / teardown_class, once per class (TID-64)
        _xunit_module_teardown(self.state.xunit)  # tearDownModule / teardown_module, once this worker is done


def _mark_names(node_id: str, style: str, root: str) -> set:
    """Every selectable mark name on a test — pytest's and tiderace's own.

    Selection has to mean the same thing in both dialects, so `-m "not slow"` deselects a
    `@pytest.mark.slow` test and a `@tiderace.mark.slow` one alike. pytest marks are read from the
    module, the class and the function; native tags are read from the function's
    `__tiderace_marks__` (TID-59)."""
    try:
        owners = resolve_target(node_id, style, root, lenient=True).owners
    except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — an unimportable module, or one that
        return set()  # skips at import (a BaseException), surfaces per node, not here (TID-102)
    names = {getattr(m, "name", "") for m in _own_markers(*owners)}
    for owner in owners:
        for m in _safe_getattr(owner, "__tiderace_marks__", None) or ():
            if getattr(m, "kind", "") == "tag" and getattr(m, "name", ""):
                names.add(m.name)
    return names


def _start_clean_room(engine: "Engine") -> None:
    """Fork a helper that keeps a pristine copy of this worker's image, for re-running demoted tests.

    A test that disturbs interpreter state is re-run in a fork so the *current* run reports the right
    answer (TID-33). The fork used to be taken from the worker that had just run it — a process now
    holding whatever the test leaked. When what leaked is a **thread**, that fork is the classic POSIX
    hazard: the child gets the one calling thread and inherits every object the others owned, so a
    re-run that waits on a background worker waits forever. Three dask tests in one real suite hung
    exactly there, each burning the full 60-second deadline: 54 of that run's 73 seconds were the
    engine waiting on tests whose work takes milliseconds.

    The helper is forked *before* this worker runs anything, so its image is clean, and it never
    executes test code itself — each request is run in a grandchild it forks on demand. That keeps it
    pristine for the life of the run no matter what the worker does to itself.

    Cheap by construction: one extra process per worker, copy-on-write, idle until something trips."""
    if not _FORK_AVAILABLE:
        return

    ours, theirs = socket.socketpair()

    def helper() -> int:  # ---- pristine, and it stays that way ----
        ours.close()
        _clean_room_serve(theirs, engine)
        return 0

    spawn(helper)
    theirs.close()
    engine.state.clean_room = ours


def _clean_room_serve(sock, engine: "Engine") -> None:
    """Serve node re-runs from a pristine image: one grandchild per request, nothing run in here.

    Running the node here instead would set its wider-scope fixtures up in *this* process, and a
    fixture that starts a thread would dirty the one image the run has left. Forking per request costs
    a fork — on the rare path this exists for — and keeps the guarantee absolute."""
    def handle(req: dict) -> dict:
        def body() -> dict:  # ---- grandchild: has the clean image, may dirty itself freely ----
            try:
                # `state.clean_room` is None in here (it is set in the worker only, after this helper was
                # forked), so a demotion inside this run takes the ordinary local fork and cannot
                # bounce back to us.
                return engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000))
            except BaseException as exc:  # noqa: BLE001 — report it; never die silently (TID-15)
                return errored(req["node_id"], _child_fault_detail(exc)[:4000])

        # Wait no longer than the node's own deadline plus slack: a re-run that hangs here must not
        # hang the worker waiting on it, which is the whole failure this exists to end.
        got = run_child(body, req.get("deadline_ms", 5000) / 1000.0 + 5.0)
        if got.reply is None:
            return errored(req["node_id"], "timeout (clean re-run produced no result)")
        return got.reply

    Transport.over(sock).serve(handle)


def _clean_room_run(state: ProcessState, node_id: str, style: str, deadline_ms: int) -> dict | None:
    """Ask the clean room to run a node from a pristine image. `None` if it cannot (caller falls back).

    A helper that has died takes the channel with it; the caller then forks locally, which is what it
    would have done anyway before this existed."""
    if state.clean_room is None:
        return None
    fd = state.clean_room.fileno()
    try:
        _write_frame(fd, {"node_id": node_id, "style": style, "deadline_ms": deadline_ms})
        resp = _read_frame(fd)
    except BaseException:  # noqa: BLE001 — a broken channel costs the clean re-run, not the run
        resp = None
    if resp is None:
        try:
            state.clean_room.close()
        except BaseException:  # noqa: BLE001
            pass
        state.clean_room = None
    return resp
