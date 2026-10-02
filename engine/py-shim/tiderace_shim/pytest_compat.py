"""Marks, in one dialect (TID-123, step 1).

A test carries marks in two spellings: pytest's (`@pytest.mark.skipif(cond, reason=…)` — an object
with `name`, `args`, `kwargs`) and tiderace's native ones (`@tiderace.skip_if(…)` — a `Mark` with
`kind`, `reason`, `condition`, `strict`). The shim used to fold each with its own code — a native
`_skip_decision` / `_apply_xfail` and a pytest `_marker_skip_reason` / `_fold_pytest_marks` — and
two node-like classes each implemented `add_marker` / `iter_markers` with opposite orders. Here
both dialects are normalised to one [`Mark`] at read time, folded once, and iterated in pytest's
order: closest first.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Iterable, Iterator
from .nodes import Target
from .safe import safe_getattr as _safe_getattr


@dataclass(frozen=True)
class Mark:
    """One mark, whichever way it was written. `kind` is `skip`, `skip_if`, `xfail`, or the mark's
    own name (`usefixtures`, `parametrize`, `slow`, …); `name` is what `-m` selects on."""

    kind: str
    name: str
    reason: str
    condition: Any  # a bool, a string left unevaluated, or `None` when the mark has none
    strict: bool
    default_reason: str  # what the report says when the author gave no reason
    source: Any  # the object as the author wrote it — `usefixtures` args, `parametrize` readers

    @property
    def skips(self) -> bool:
        """Whether this mark skips the test. A `skip_if` with a string condition does not: the
        expression is left unevaluated, because guessing at it could silently skip a test that
        should have run — the failure that cannot be seen."""
        if self.kind == "skip":
            return True
        return self.kind == "skip_if" and isinstance(self.condition, bool) and self.condition

    @property
    def expects_failure(self) -> bool:
        """Whether this `xfail` applies: the bare form always, a `False` condition never, and a
        string condition is taken as applying — the caution in the other direction, since an xfail
        that does not fire only ever reports a real failure."""
        return self.kind == "xfail" and self.condition is not False


def normalise(mark: Any) -> Mark:
    """Either dialect as a [`Mark`]. Native marks carry `kind`; pytest's carry `name` / `args` /
    `kwargs`."""
    if isinstance(mark, Mark):
        return mark
    kind = getattr(mark, "kind", None)
    if kind is not None:  # tiderace's own (`tiderace._spec.Mark`)
        name = getattr(mark, "name", "") or kind
        return Mark(kind, name, getattr(mark, "reason", "") or "",
                    getattr(mark, "condition", None) if kind == "skip_if" else None,
                    bool(getattr(mark, "strict", False)), kind, mark)
    name = getattr(mark, "name", "") or ""
    kwargs = getattr(mark, "kwargs", None) or {}
    args = getattr(mark, "args", ()) or ()
    reason = kwargs.get("reason") or ""
    condition: Any = None
    if name == "skipif":
        condition = args[0] if args else kwargs.get("condition")
        return Mark("skip_if", name, reason, condition, False, "skipif", mark)
    if name == "skip":
        if not reason and args and isinstance(args[0], str):
            reason = args[0]
        return Mark("skip", name, reason, None, False, "skip", mark)
    if name == "xfail":
        # `@pytest.mark.xfail(sys.platform == "win32", reason=...)` puts the condition first
        # positionally; the bare form has none and always applies.
        condition = args[0] if args and not isinstance(args[0], str) else kwargs.get("condition", True)
        return Mark("xfail", name, reason, condition, bool(kwargs.get("strict")), "xfail", mark)
    return Mark(name, name, reason, None, False, name, mark)


def normalise_all(marks: Iterable[Any]) -> list[Mark]:
    return [normalise(m) for m in marks]


def skip_reason(marks: Iterable[Mark]) -> str | None:
    """The reason the closest skipping mark gives, or `None` when none applies."""
    for m in marks:
        if m.skips:
            return m.reason or m.default_reason
    return None


def apply_xfail(mark: Mark, outcome: str, detail: str) -> tuple[str, str]:
    """Fold one applying `xfail` into an outcome: a fail or error becomes `xfail`; a pass becomes
    `xpass`, or `failed` when the mark is `strict`; a skip stays a skip."""
    if outcome in ("failed", "error"):
        return "xfail", mark.reason or detail
    if outcome == "passed":
        if mark.strict:
            return "failed", f"[xpass strict] {mark.reason}".strip()
        return "xpass", mark.reason
    return outcome, detail


def fold(marks: Iterable[Mark], outcome: str, detail: str, *, runtime: bool = False) -> tuple[str, str]:
    """Fold a test's marks into its outcome, closest first: the first applying `xfail` decides;
    an unconditional `skip` met on the way — a marker added while the test ran, since a static
    one is decided before the test starts — makes it a skip."""
    for m in marks:
        if m.kind == "skip":
            return "skipped", m.reason or ("skipped at runtime" if runtime else m.default_reason)
        if m.expects_failure and outcome in ("failed", "error", "passed"):
            return apply_xfail(m, outcome, detail)
    return outcome, detail


class MarkerBearer:
    """What a `request.node` and a collection hook's `item` share: marks attached to this node,
    iterated closest first — the one added last is the closest, as pytest's `iter_markers`
    walks from the item outwards."""

    own_markers: list

    def add_marker(self, marker: Any, append: bool = True) -> None:
        """Attach a marker, as pytest allows from a fixture, a test body, or a collection hook.
        `append` makes it the closest; `append=False` the farthest."""
        if append:
            self.own_markers.append(marker)
        else:
            self.own_markers.insert(0, marker)

    def iter_markers(self, name: str | None = None) -> Iterator[Any]:
        for m in reversed(self.own_markers):
            if name is None or getattr(m, "name", None) == name:
                yield m

    def get_closest_marker(self, name: str, default: Any = None) -> Any:
        return next(self.iter_markers(name), default)


class _Node(MarkerBearer):
    """`request.node` — what pytest calls the item under test.

    Fixtures reach for it to name the thing they are building for (`request.node.name` in a temp-file
    or database-name prefix) and, less often, to attach a marker while the run is in flight. Both
    were `AttributeError` before: the fixture request had no `node` at all, and the test request had
    the node *id string* rather than an object (TID-51)."""

    __slots__ = ("nodeid", "name", "originalname", "cls", "function", "own_markers")

    def __init__(self, node_id: str, func=None, instance=None):
        self.nodeid = node_id
        # pytest's `name` is the last component, parametrize id included: `test_x[case]`.
        self.name = node_id.rpartition("::")[2] or node_id
        self.originalname = self.name.partition("[")[0]
        self.cls = type(instance) if instance is not None else None
        self.function = func
        self.own_markers: list = []

    def __repr__(self) -> str:
        return f"<Node {self.nodeid}>"


def _runtime_outcome(node, outcome: str, detail: str) -> tuple:
    """Fold markers added during the run into the outcome (`xfail`, `skip`).

    Applied here rather than beside the static marks because a marker added at runtime exists only in
    the process that ran the test."""
    if node is None or not node.own_markers:
        return outcome, detail
    return fold(normalise_all(node.iter_markers()), outcome, detail, runtime=True)


def _own_markers(*owners) -> list:
    """The `@pytest.mark.*` marks on a chain of owners, widest first (module → class → function).

    pytest stores them as a `pytestmark` list on whatever they decorate, so gathering them is just
    reading that attribute at each level. `__tiderace_marks__` is the native analogue and is read
    separately by `_marks`; this is the pytest-compat side."""
    out = []
    for owner in owners:
        if owner is None:
            continue
        marks = _safe_getattr(owner, "pytestmark", None)  # owners can carry a raising metaclass
        if not marks:
            continue
        # pytest accepts both spellings — `pytestmark = pytest.mark.slow` and
        # `pytestmark = [pytest.mark.slow, ...]` — and a bare `MarkDecorator` is not iterable, so
        # extending on it raises `TypeError` and takes the whole discovery pass down with it.
        out.extend(marks if isinstance(marks, (list, tuple)) else [marks])
    return out


class _HookItem(MarkerBearer):
    """The `item` a `pytest_collection_modifyitems` hook is handed (TID-20).

    Only the surface real conftests use: `nodeid` / `name` to identify it, `keywords` and
    `own_markers` / `iter_markers` / `get_closest_marker` to inspect it, and `add_marker` to change
    it. The overwhelmingly common shape — the one this ticket was filed for — is

        for item in items:
            if "needs_kuzu" in item.keywords:
                item.add_marker(pytest.mark.skip(reason="pass --real to run Kuzu tests"))

    which needs exactly `keywords` and `add_marker`."""

    __slots__ = ("nodeid", "name", "own_markers", "keywords")

    def __init__(self, nodeid: str, name: str, markers: list):
        self.nodeid = nodeid
        self.name = name
        self.own_markers = list(markers)
        # pytest's `keywords` is a mapping that answers `in` for mark names, the node name, and the
        # module. Membership is what conftests actually use it for.
        self.keywords = {getattr(m, "name", str(m)): m for m in self.own_markers}
        self.keywords[name] = True
        self.keywords[nodeid] = True

    def add_marker(self, marker, append: bool = True) -> None:
        super().add_marker(marker, append)
        self.keywords[getattr(marker, "name", str(marker))] = marker

    def __repr__(self) -> str:  # a hook that logs its items should print something useful
        return f"<Item {self.nodeid}>"


def _pytest_markers(node: Target) -> list:
    """The `@pytest.mark.*` objects on a test, from its module, class and function."""
    return list(_own_markers(*node.owners))
