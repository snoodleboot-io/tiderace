"""The isolation ladder's tiers, decided once (TID-123, step 4).

A node runs on one tier: bare in-process (a trusted-pure test, no snapshot), in-process with
snapshot/restore (the ladder's default), in-process without restore (`--no-fork` without it), a
fork per variant, or the module's own forked child (an opaque or disturbing module, TID-80). The
choice used to be eleven booleans threaded through `Engine.run`, `_fork_run` and `_child_exec`,
each re-deriving a piece of it. [`route`] is the one place the tier is chosen, from every input it
needs as a parameter; [`assemble`] turns the variants' results into the node's response.
"""
from __future__ import annotations

import enum
from dataclasses import dataclass
from typing import Any, Callable, Iterable

from .pytest_compat import Mark, fold
from .results import UNKNOWN_PURITY, Outcome, response, variant, with_purity


@dataclass(frozen=True)
class EngineOptions:
    """A run's configuration, read by the route and the executor — the six booleans the engine
    used to carry as attributes, in one place."""

    no_fork: bool = False  # in-process by configuration: `--no-fork`, the SubprocessWorker, Windows
    restore: bool = False  # snapshot/restore shared state around in-process tests (isolation w/o fork)
    purity_guard: bool = False  # measure shared-state mutation per test (→ pure-test batching)
    coverage: bool = False  # capture the per-test executed-source footprint (ADR-E006)
    coverage_lines: bool = False  # line numbers in the footprint (opt-in, TID-76)


class Tier(str, enum.Enum):
    BARE = "bare"  # in-process, no snapshot: recorded pure and unchanged (TID-1)
    RESTORE = "restore"  # in-process, snapshot/restore around the test (the ladder's default)
    IN_PROCESS = "in-process"  # in-process without restore (`--no-fork` without it)
    FORK = "fork"  # a pristine copy-on-write child per variant (ADR-E003)
    MODULE_CHILD = "module-child"  # the module's one forked child, for the rest of the file (TID-80)
    REFUSED = "refused"  # the module needs a fork and the platform has none: reported, not run

    @property
    def in_process(self) -> bool:
        return self in (Tier.BARE, Tier.RESTORE, Tier.IN_PROCESS)


@dataclass(frozen=True)
class Routing:
    """Everything the tier choice depends on."""

    fork_available: bool
    in_module_child: bool  # this process IS a module child: it never forks again
    module_child_holds_module: bool  # a child is already open for this node's module (TID-96)
    no_fork: bool  # the run is in-process by configuration (`--no-fork`, Windows)
    restore: bool  # the snapshot/restore ladder is on
    force_no_fork: bool  # the engine asked for this node in-process (the ladder's guess)
    trusted_pure: bool  # recorded pure and unchanged: the bare tier (TID-1)
    recorded_must_fork: bool  # recorded as disturbing interpreter state (TID-33)


def route(inputs: Routing, restorable: Callable[[], bool]) -> Tier:
    """The tier for a node, decided once.

    The soundness gate for BOTH in-process paths: a module whose shared state cannot be
    snapshot/restored (opaque globals — an open file, a generator, a live socket) must fork,
    because running it in-process leaks whatever the test mutated into the next test on the same
    module. That applies to `--no-fork` too, not just the ladder's per-test guess (it once checked
    only `force_no_fork`, so whole-run no-fork ran opaque modules in-process and a module-level
    generator stayed advanced across tests). A trusted-pure test skips the check: known pure ⇒ it
    will not mutate. A recorded disturber (TID-33) is denied the in-process tier the same way and
    takes the same route as an opaque module (TID-96); and a module child already open for this
    file takes the rest of the file, so its tests are never split across two processes.

    `restorable` is called only when the answer depends on it — it costs a deep copy of the
    module's shared state (TID-99)."""
    must_fork = False
    if inputs.restore and not inputs.trusted_pure and (inputs.force_no_fork or inputs.no_fork):
        try:
            must_fork = not restorable()
        except Exception:  # noqa: BLE001 — can't import/inspect ⇒ be safe, fork
            must_fork = True
    if inputs.recorded_must_fork and inputs.restore and not inputs.no_fork:
        must_fork = True
    if (inputs.module_child_holds_module and not inputs.in_module_child and inputs.fork_available
            and inputs.restore and not inputs.no_fork):
        must_fork = True
    if must_fork:
        if not inputs.fork_available:
            return Tier.REFUSED
        return Tier.FORK if inputs.in_module_child else Tier.MODULE_CHILD
    if inputs.no_fork or inputs.force_no_fork:
        if inputs.trusted_pure:
            return Tier.BARE
        return Tier.RESTORE if inputs.restore else Tier.IN_PROCESS
    return Tier.FORK


@dataclass(frozen=True)
class VariantResult:
    """One variant, as executed: its outcome, what it touched, what it did to the interpreter."""

    variant_id: str
    outcome: str
    detail: str
    coverage: dict  # `rel_path -> set[int]`, empty unless capture is on
    purity: Any  # a reason (impure), `None` (measured pure), or `UNKNOWN_PURITY`
    disturbed: bool  # it disturbed interpreter state: fork it from now on (TID-33)
    duration_ms: int


def assemble(node_id: str, results: Iterable[VariantResult], *, parametrized: bool,
             native_marks: Iterable[Mark], pytest_marks: Iterable[Mark],
             keywords: Callable[[str], list]) -> dict:
    """The node's response from its variants' results: the worst outcome, the marks folded
    (native first, then pytest's, closest first), the per-variant results of a parametrized node
    (TID-25), the union of what they touched, the node's purity verdict across variants (any
    measured impure ⇒ impure; all measured pure ⇒ pure; nothing measured ⇒ no verdict), and
    `must_fork` when any variant disturbed the interpreter."""
    results = list(results)
    outcome, detail = Outcome.worst([(r.outcome, r.detail) for r in results])
    outcome, detail = fold(native_marks, outcome, detail)
    outcome, detail = fold(pytest_marks, outcome, detail)
    resp = response(node_id, outcome, detail=detail, keywords=keywords(node_id))
    # Additive and omitted for an unparametrized node, so its frame stays byte-identical.
    if parametrized and results:
        variants = []
        for r in results:
            case = variant(r.variant_id, r.outcome, r.detail, r.duration_ms, keywords=keywords(r.variant_id))
            if r.coverage:
                case["coverage"] = {p: sorted(lines) for p, lines in r.coverage.items()}
            with_purity(case, r.purity)
            if r.disturbed:
                case["must_fork"] = True
            variants.append(case)
        resp["variants"] = variants
    coverage: dict[str, set] = {}
    for r in results:
        for path, lines in r.coverage.items():
            coverage.setdefault(path, set()).update(lines)
    if coverage:  # additive field (Phase-3 CONTRACT §6); omitted when capture is off/empty
        resp["coverage"] = {path: sorted(lines) for path, lines in coverage.items()}
    node_pure = None  # tri-state across variants: None (unmeasured), True (all pure), False
    impurity = None  # the first impurity reason across variants
    for r in results:
        if r.purity is UNKNOWN_PURITY:
            continue  # this variant measured nothing — leave the node verdict as-is
        if r.purity is None:
            if node_pure is None:
                node_pure = True
        else:
            node_pure = False
            if impurity is None:
                impurity = r.purity
    if node_pure is not None:  # additive: purity was measured (guard or restore) — record the verdict
        resp["pure"] = node_pure
        if impurity is not None:
            resp["impurity"] = impurity
    if any(r.disturbed for r in results):  # additive; omitted for the overwhelming majority that never trip
        resp["must_fork"] = True
    return resp
