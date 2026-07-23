"""Pure-data shapes the decorators stamp onto user functions — the tiderace-owned attribute protocol
the shim/collector reads (the native analogue of pytest's `_fixture_function_marker`)."""
from __future__ import annotations

from dataclasses import dataclass

# The five scopes the Rust engine already owns (engine mechanics we keep — not a pytest import).
SCOPES = ("function", "class", "module", "package", "session")


@dataclass(frozen=True)
class ProviderSpec:
    """Stamped on a provider as `__tiderace_provider__`. `provides` is the *type* injected by; `name`
    is the provider's stable identity (the key the Rust name-keyed graph consumes)."""

    provides: type
    scope: str
    autouse: bool
    name: str
    is_yield: bool
    params: tuple = ()  # provider-level parametrization (B5): the provider fans out, one value per param


@dataclass(frozen=True)
class Case:
    """One parametrization variant. `index` + `id` mirror the Rust `ParamValue{id, index}` so each
    variant gets a distinct closure hash; `values` are the positional args bound to the test."""

    id: str
    index: int
    values: tuple


@dataclass(frozen=True)
class Mark:
    """One native mark stamped (appended) onto a test as `__tiderace_marks__`. `kind` is the discriminator
    the shim acts on: `skip`/`skip_if` short-circuit before setup; `xfail` inverts the outcome; `tag` is
    selection metadata only (the Rust collector filters on it later — no execution effect)."""

    kind: str  # "skip" | "skip_if" | "xfail" | "tag"
    reason: str = ""
    condition: bool = True  # skip_if: whether the skip applies
    strict: bool = False  # xfail: an unexpected pass becomes a failure
    name: str = ""  # tag: the tag label
