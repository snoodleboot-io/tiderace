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
