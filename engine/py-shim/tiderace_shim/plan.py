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
