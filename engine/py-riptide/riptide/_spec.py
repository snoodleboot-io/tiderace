"""Pure-data shapes the decorators stamp onto user functions — the riptide-owned attribute protocol
the shim/collector reads (the native analogue of pytest's `_fixture_function_marker`)."""
from __future__ import annotations

from dataclasses import dataclass

# The five scopes the Rust engine already owns (engine mechanics we keep — not a pytest import).
SCOPES = ("function", "class", "module", "package", "session")


@dataclass(frozen=True)
class ProviderSpec:
    """Stamped on a provider as `__riptide_provider__`. `provides` is the *type* injected by; `name`
    is the provider's stable identity (the key the Rust name-keyed graph consumes)."""

    provides: type
    scope: str
    autouse: bool
    name: str
    is_yield: bool


@dataclass(frozen=True)
class Case:
    """One parametrization variant. `index` + `id` mirror the Rust `ParamValue{id, index}` so each
    variant gets a distinct closure hash; `values` are the positional args bound to the test."""

    id: str
    index: int
    values: tuple
