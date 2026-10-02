"""What a node's run will execute, decided before anything is set up (TID-123, step 3).

`Engine.run` used to decide all of this inline, interleaved with the gates that end a run early
and with the execution itself. Now the gates (`Engine._gate`) answer with a ready response or let
the node through; the plan (`Engine._plan`) turns the node's requests, marks and cases into the
closure, the variant ids and the cases `-k` keeps, or answers early when the node has nothing to
run (every case deselected, a whole-node skip, a collection failure); and `run` routes and
executes what the [`Plan`] says. Nothing here sets a fixture up: the ids come from the marks and
the registry, never from a fixture, so a node `-k` is about to deselect costs no setup (TID-99).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .nodes import Target
import enum
import itertools
from typing import TYPE_CHECKING

from .nodes import module_key as _module_key
from .pytest_compat import _HookItem, _own_markers
from .results import Outcome
from .safe import safe_getattr as _safe_getattr, safe_hasattr as _safe_hasattr
from .selection import pytest_major as _pytest_major
if TYPE_CHECKING:
    from .discovery import _Config, Discovery


@dataclass
class Plan:
    """A node's execution plan: what to build, which cases to run, how to name them."""

    node: Target
    names: set  # what `-k` / `-m` match against: path names, `::` segments, mark names
    marks: list  # the node's native marks, as written
    requested: dict  # `param -> provider` bindings, before parametrize splits them
    fixture_requested: dict  # the subset the fixture graph resolves
    closure: list  # the fixture closure, widest first
    indirect: set  # parametrize argnames routed to a fixture's `request.param`
    case_kwargs_list: list  # one kwargs map per direct-parametrize case (`[{}]` when none)
    combos: list  # one fixture-param map per axis product (`[{}]` when none)
    combo_id_maps: list  # aligned with `combos`: the author's id per axis, or `None`
    parametrized_node: bool
    variant_ids: list = field(default_factory=list)  # every case's id, pytest-disambiguated
    selected: set = field(default_factory=set)  # the indices into `variant_ids` that `-k` keeps

    @property
    def per_combo(self) -> int:
        """How many variants each fixture-param combo expands into."""
        return len(self.case_kwargs_list)


class _GenerateTestsError(Exception):
    """A `metafunc.parametrize` that pytest would refuse — reported on the test, as pytest reports it."""


class _SyntheticMark:
    """A `parametrize` mark that came from `metafunc.parametrize` rather than a decorator; the same
    shape `_parametrize_cases` and `_indirect_names` already read."""

    __slots__ = ("name", "args", "kwargs")

    def __init__(self, argnames, argvalues, ids, indirect):
        self.name = "parametrize"
        self.args = (argnames, list(argvalues))
        self.kwargs = {"ids": ids, "indirect": indirect}


class _MetaFunc:
    """What a `pytest_generate_tests(metafunc)` hook is handed (TID-85): the test's fixture names,
    its function/module/class, a `definition` that answers marker queries, a `config`, and
    `parametrize`, which records an axis for the test exactly as a `@pytest.mark.parametrize` would."""

    def __init__(self, node_id: str, func, module, cls, fixturenames: list, markers: list,
                 config: _Config):
        self.function = func
        self.module = module
        self.cls = cls
        self.fixturenames = list(fixturenames)
        self.config = config
        self.definition = _HookItem(node_id, node_id.rsplit("::", 1)[-1], markers)
        self._marks: list = []

    def parametrize(self, argnames, argvalues, indirect=False, ids=None, scope=None, *,
                    _param_mark=None):
        names = ([n.strip() for n in argnames.split(",") if n.strip()]
                 if isinstance(argnames, str) else list(argnames))
        for name in names:
            if name not in self.fixturenames:
                raise _GenerateTestsError(
                    f"In {self.definition.name}: function uses no argument '{name}'")
        self._marks.append(_SyntheticMark(names, argvalues, ids, indirect))


def _generate_tests_marks(node_id: str, func, module, cls, fixturenames: list, markers: list,
                          config: _Config, disc: Discovery) -> list:
    """Run the `pytest_generate_tests` hooks that apply to this test — its module's own first, then
    its conftests deepest to root — and return the parametrize marks they declared, in pytest's
    order (which is the order their ids appear in the node id). Cached per node: hooks are
    deterministic and `_cases`/`_indirect` both ask."""
    cached = disc.hook_marks.get(node_id)
    if cached is not None:
        return cached
    hooks = []
    own = _safe_getattr(module, "pytest_generate_tests", None)
    if callable(own):
        hooks.append(own)
    for conftest in disc.conftests_governing(_module_key(node_id)):
        hook = _safe_getattr(conftest, "pytest_generate_tests", None)
        if callable(hook):
            hooks.append(hook)
    marks: list = []
    if hooks:
        metafunc = _MetaFunc(node_id, func, module, cls, fixturenames, markers, config)
        for hook in hooks:
            hook(metafunc)
        marks = metafunc._marks
    disc.hook_marks[node_id] = marks
    return marks


def _indirect_names(func, *outer, hook_marks=None) -> set:
    """Argnames a `parametrize` marks as **indirect**, anywhere in the owner chain — or in a
    `pytest_generate_tests` hook's call (TID-85).

    `indirect=True` (or a list of names) does not hand the value to the test: pytest gives it to the
    *fixture* of that name as `request.param`, and the test receives whatever the fixture returns. So
    an indirect name stays a fixture request, where a direct one overrides any fixture sharing its
    name (TID-57). Getting this backwards hands the test the raw parametrize value — typically the
    fixture function itself, which then fails on the first attribute the test touches."""
    out: set = set()
    for mark in list(hook_marks or ()) + _own_markers(func, *outer):
        if getattr(mark, "name", "") != "parametrize":
            continue
        indirect = (getattr(mark, "kwargs", None) or {}).get("indirect")
        if not indirect:
            continue
        names = mark.args[0]
        names = ([n.strip() for n in names.split(",") if n.strip()] if isinstance(names, str)
                 else list(names))
        out.update(names if indirect is True else
                   [n for n in names if n in set(indirect)])
    return out


def _parametrize_cases(func, *outer, hook_marks=None) -> list[dict]:
    """Expand `@pytest.mark.parametrize` on ``func`` — and on its class and module — into cases.

    The corpus is authored against pytest, so a test whose arguments come from
    `parametrize` looks, to a runner that only knows fixtures, like a test
    requesting fixtures nobody provides — it is then called bare and dies on
    "missing 1 required positional argument". Reading the marker turns those
    into ordinary cases, which the existing `case_kwargs` path already runs.

    Values are returned as name→value maps rather than positionally, because
    `parametrize`'s argnames need not match the signature order.

    Stacked marks multiply, as in pytest. Each case carries its **explicit id** when the author gave
    one — `ids=[...]`, `ids=callable`, or `pytest.param(..., id=...)` — because those ids are
    selectors, and a generated `[size1]` where pytest prints `[decimal]` cannot be pasted from one
    runner into the other. Returns `(kwargs, explicit_id_or_None)` per case.

    Stacked marks with ids on only *some* axes fall back to generated ids for the whole case rather
    than splicing the two schemes, which would produce an id matching neither runner.

    `outer` is the rest of the owner chain, narrowest first: the class, then the module. pytest applies
    a mark on a class to every method the class collects, and the same for a module-level `pytestmark`
    — reading only the function missed both. A class parametrized with 5 values and holding 4 methods
    is 20 tests in pytest and was 4 here, each failing on the argument nobody supplied (TID-53).

    Order matters and is pytest's, not ours: axes run narrowest first, so `test[1-A]` puts the
    function's own parameter before the class's, and the class's value varies fastest across the
    generated cases. `_own_markers` reports widest first, which is why the chain is passed in reverse
    here.
    """
    # Hook-declared axes first: pytest calls `pytest_generate_tests` hooks before it applies the
    # decorator marks (its own mark handling is one such hook, registered earliest and so called
    # last), and an id is the axes in call order — `test_x[sqlite-1]` for a conftest's `backend` and
    # the function's own `n` (TID-85).
    marks = list(hook_marks or ()) + [
        m for m in _own_markers(func, *outer) if getattr(m, "name", "") == "parametrize"
    ]
    if not marks:
        return []
    axes: list[list[tuple]] = []
    for mark in marks:
        names = mark.args[0]
        names = (
            [n.strip() for n in names.split(",") if n.strip()] if isinstance(names, str)
            else list(names)
        )
        ids_kw = (getattr(mark, "kwargs", None) or {}).get("ids")
        axis: list[tuple] = []
        for position, entry in enumerate(mark.args[1]):
            # `pytest.param(...)` carries `.values`/`.marks`; both are checked so a
            # plain dict argvalue (which has a `.values` *method*) is not mistaken for one.
            explicit = None
            # Safe probes: `entry` is an arbitrary parametrize value, and a lazy proxy passed as one raises
            # on attribute access exactly as it does as a module global (TID-43).
            if _is_param_set(entry):
                raw = tuple(entry.values)
                explicit = _safe_getattr(entry, "id", None)
            elif len(names) == 1:
                raw = (entry,)
            else:
                raw = tuple(entry)
            if explicit is None and ids_kw is not None:
                explicit = _explicit_id(ids_kw, raw, position)
            axis.append((dict(zip(names, raw)), None if explicit is None else str(explicit), position))
        axes.append(axis)
    cases: list[tuple] = []
    for combo in itertools.product(*axes):
        merged: dict = {}
        pieces: list[tuple] = []  # per axis: (its argnames, the author's id or None)
        positions: dict = {}  # per argname: the value's position in its axis (TID-86)
        for piece, piece_id, axis_pos in combo:
            merged.update(piece)
            pieces.append((tuple(piece), piece_id))
            positions.update({name: axis_pos for name in piece})
        if pieces and all(pid is not None for _, pid in pieces):
            case_id = "-".join(pid for _, pid in pieces)
        elif any(pid is not None for _, pid in pieces):
            # Mixed: an explicit id on some axes and none on others. pytest keeps the explicit
            # piece and generates the rest per axis — `test_x[EU-sqlite]` for `ids=["EU", "US"]`
            # on `region` and a bare `backend` (TID-85) — so hand `_variant_parts` the axes.
            case_id = pieces
        else:
            case_id = None
        cases.append((merged, case_id, positions))
    return cases


def _explicit_id(ids_kw, values: tuple, position: int):
    """The author-supplied id for one case, from `ids=[...]` or `ids=callable`.

    A callable is applied per value and joined, as pytest does; a callable that returns `None` for a
    value means "generate this part", and since the parts cannot be mixed here that degrades the
    whole case to a generated id rather than a half-built one."""
    try:
        if callable(ids_kw):
            produced = [ids_kw(v) for v in values]
            return None if any(p is None for p in produced) else "-".join(str(p) for p in produced)
        return ids_kw[position]
    except Exception:  # noqa: BLE001 — a malformed `ids` must not take the test down
        return None


def _id_part(value, argname: str, index: int) -> str:
    """One parameter's contribution to a pytest-style `[...]` id, matching pytest's own spelling.

    Parity matters here rather than being cosmetic: these ids are selectors. Someone who copies
    `test_rejected[(SELECT 1)]` out of a pytest run and pastes it into tiderace has to hit the same
    test, so this follows `_pytest.python._idval` rather than inventing a scheme:

    * strings keep printable ASCII verbatim (spaces, quotes, brackets and all) and escape the rest,
      which is exactly `ascii_escaped` — `"таблица"` becomes `\\u0442\\u0430\\u0431\\u043b\\u0438\\u0446\\u0430`;
    * scalars print as themselves (`3`, `True`, `None`, `1.5`);
    * anything else is `argname` + its index, because a `repr` would embed addresses and stop being
      stable between runs.
    """
    if isinstance(value, enum.Enum):
        return str(value)  # `OpaquePolicy.REPR_CONTENT`; before the int branch, since IntEnum is one
    if isinstance(value, str):
        return value.encode("unicode_escape").decode("ascii")
    if isinstance(value, bytes):
        # pytest's `ascii_escaped` for bytes: non-ASCII as `\xNN`, then anything non-printable the
        # same way, so `b"\x1b[45m123\x1b[0m"` is `\x1b[45m123\x1b[0m` and `b"\xff"` is `\xff` —
        # not `expect0`, and not the doubly-escaped `\\xff` (TID-86).
        text = value.decode("ascii", "backslashreplace")
        return "".join(c if c.isprintable() else f"\\x{ord(c):02x}" for c in text)
    if value is None or isinstance(value, (bool, int, float)):
        return str(value)
    # Classes and functions id by name in pytest. A parametrize value is an arbitrary object, and a lazy
    # proxy passed as one raises on attribute access just as it does as a module global (TID-43).
    name = _safe_getattr(value, "__name__", None)
    if isinstance(name, str):
        return name
    return f"{argname}{index}"


def _is_param_set(value) -> bool:
    """A `pytest.param(...)` — `ParameterSet` — probed safely: a lazy proxy raises on attribute access."""
    return _safe_hasattr(value, "values") and _safe_hasattr(value, "marks") and _safe_hasattr(value, "id")


def _param_value(value):
    """What a fixture's `request.param` is for one entry of `params=`: the value itself, or, for a
    `pytest.param(...)`, its values — one value unwrapped, several as a tuple — as pytest hands it
    over. anyio's `anyio_backend` is `pytest.param(("asyncio", {...}), id="asyncio")`, and every
    test on it received the ParameterSet instead of the tuple (TID-86)."""
    if _is_param_set(value):
        values = tuple(value.values)
        return values[0] if len(values) == 1 else values
    return value


def _fixture_param_id(fdef, index: int, value):
    """The author-supplied id for one parametrized-FIXTURE case, or None to generate one.

    `@pytest.fixture(params=[...], ids=[...])` takes the same shapes `parametrize` does, so this
    mirrors `_explicit_id`. Kept separate because a fixture's ids live on its definition rather than
    on a mark, and the two are resolved at different points."""
    # `pytest.param(..., id="asyncio")` as a fixture param carries its own id (TID-86); `ids=` on
    # the fixture applies to the rest.
    if _is_param_set(value) and _safe_getattr(value, "id", None) is not None:
        return str(value.id)
    ids = getattr(fdef, "param_ids", None)
    if ids is None:
        return None
    try:
        produced = ids(value) if callable(ids) else ids[index]
    except Exception:  # noqa: BLE001 — a malformed `ids` must not take the test down
        return None
    return None if produced is None else str(produced)


def _variant_parts(combo: dict, combo_ids: dict, case_kwargs: dict, index: int,
                   explicit=None, combo_pos: dict | None = None, case_pos: dict | None = None) -> str:
    """The inside of a variant's `[...]`, before duplicates are disambiguated.

    Parametrized-fixture values come first, then the test's own `parametrize` values, each in
    declaration order, and an author-supplied id wins over anything generated. `explicit` is the
    whole case's id (a string), none (generate every part), or a list of per-axis
    `(argnames, id-or-None)` — an explicit piece where the author gave one, generated where not."""
    cpos = combo_pos or {}
    kpos = case_pos or {}
    parts = [
        combo_ids.get(k) if combo_ids.get(k) is not None else _id_part(v, k, cpos.get(k, index))
        for k, v in combo.items()
    ]
    if isinstance(explicit, str):
        parts.append(explicit)
    elif explicit is not None:
        for names, piece in explicit:
            if piece is not None:
                parts.append(piece)
            else:
                parts += [_id_part(case_kwargs[n], n, kpos.get(n, index))
                          for n in names if n in case_kwargs]
    else:
        parts += [_id_part(v, k, kpos.get(k, index)) for k, v in case_kwargs.items()]
    return "-".join(parts)


def _disambiguate(parts: list) -> list:
    """Suffix colliding ids the way pytest does — **every** member of a clash, indexed from 0.

    Two cases whose values print alike produce the same text, and an id that collides cannot select.
    pytest turns `as_tool, as_tool` into `as_tool0, as_tool1`; suffixing only the second (leaving the
    first bare) is the obvious alternative and does not match, so a copied id would miss. An id that
    ends in a digit takes a `_` before the index under pytest 8 and later — `1_0`, `1_1` rather than
    `10`, `11` — and does not under 7 (TID-88); the suite's own pytest decides."""
    seen: dict[str, int] = {}
    counts: dict[str, int] = {}
    for text in parts:
        counts[text] = counts.get(text, 0) + 1
    out = []
    underscore = _pytest_major() >= 8
    for text in parts:
        if counts[text] == 1:
            out.append(text)
            continue
        n = seen.get(text, 0)
        seen[text] = n + 1
        sep = "_" if underscore and text and text[-1].isdigit() else ""
        out.append(f"{text}{sep}{n}")
    return out


def _aggregate(outcomes: list[tuple[str, str]]) -> tuple[str, str]:
    """Collapse parametrization variants into one node outcome (worst wins — `Outcome.worst`)."""
    return Outcome.worst(outcomes)
