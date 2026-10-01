"""The shim's answers, spelled once (TID-121).

A result frame is `{"node_id", "outcome", "detail", …}`; an *expansion* carries `"expanded": True`
and the `variants` that are the whole answer — an empty list is a node pytest would not have in the
tally at all (deselected, a fixture that looked like a test, a class with nothing inherited). The
dicts used to be written out by hand at twenty-five sites; this module is the one place the wire
shape lives, and the Rust side reads exactly what it always has.
"""
from __future__ import annotations

import enum
from typing import Any


class Outcome(str, enum.Enum):
    """A test's outcome on the wire. `str` so a value compares to, and serialises as, its text."""

    PASSED = "passed"
    FAILED = "failed"
    ERROR = "error"
    SKIPPED = "skipped"
    XFAIL = "xfail"
    XPASS = "xpass"

    @staticmethod
    def worst(outcomes: list[tuple[str, str]]) -> tuple[str, str]:
        """Collapse parametrization variants into one node outcome — the one ordering: an error
        over a failure over a skip over a pass (an xfail or xpass counts as a pass here)."""
        return max(outcomes, key=lambda o: _SEVERITY.get(o[0], 0))


_SEVERITY = {Outcome.ERROR: 3, Outcome.FAILED: 2, Outcome.SKIPPED: 1, Outcome.PASSED: 0}

# Purity tri-state: a reason string (impure), `None` (measured pure), or `UNKNOWN_PURITY` (not
# measured — the test forked, ran async, or was trusted-pure so the snapshot was skipped). Only a
# *measured pure* verdict is recordable for the bare-no-fork fast path (ADR-E014 / TID-1).
UNKNOWN_PURITY = object()


def response(node_id: str, outcome: str, **fields: Any) -> dict:
    """A result frame: the node, its outcome, and whatever else the caller has to say."""
    return {"node_id": node_id, "outcome": outcome, **fields}


def errored(node_id: str, detail: str, **fields: Any) -> dict:
    return response(node_id, Outcome.ERROR, detail=detail, **fields)


def skipped(node_id: str, reason: str, **fields: Any) -> dict:
    """A skip. `skip_origin=` names the module whose import skipped it (TID-55); `keywords=` what
    `-k` would match against (TID-102)."""
    return response(node_id, Outcome.SKIPPED, detail=reason, **fields)


def empty_expansion(node_id: str, **fields: Any) -> dict:
    """A node with nothing in the tally: deselected by `-m` / `-k`, ignored, a fixture the
    collector took for a test, or a class with no inherited tests."""
    return response(node_id, Outcome.PASSED, expanded=True, variants=[], **fields)


def expansion(node_id: str, outcome: str, detail: str, variants: list, **fields: Any) -> dict:
    """A node whose answer is its variants — the inherited methods of a class (TID-26)."""
    return response(node_id, outcome, detail=detail, expanded=True, variants=variants, **fields)


def variant(node_id: str, outcome: str, detail: str, duration_ms: int, **fields: Any) -> dict:
    """One case of a parametrized node, or one inherited method of a class."""
    return {"node_id": node_id, "outcome": outcome, "detail": detail, "duration_ms": duration_ms,
            **fields}


def with_purity(result: dict, purity: Any, *, reason_key: str | None = None) -> dict:
    """Encode the purity tri-state onto `result`: `pure: True` when measured pure, `pure: False`
    (and the reason under `reason_key`, when asked) when measured impure, nothing when unknown."""
    if purity is None:
        result["pure"] = True
    elif purity is not UNKNOWN_PURITY:
        result["pure"] = False
        if reason_key:
            result[reason_key] = purity
    return result


def purity_from(result: dict) -> Any:
    """Decode the tri-state back: `pure` omitted ⇒ unknown; True ⇒ measured pure; False ⇒ the
    recorded reason, or `"impure"`."""
    if "pure" not in result:
        return UNKNOWN_PURITY
    if result["pure"]:
        return None
    return result.get("impurity") or "impure"
