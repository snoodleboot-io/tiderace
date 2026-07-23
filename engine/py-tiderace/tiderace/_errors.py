"""Native authoring errors — tiderace's own taxonomy (the Python-authoring analogue of the Rust
`FixtureError`). Raised at definition/resolution time, never a silent fallback."""
from __future__ import annotations


class TideraceError(Exception):
    """Base for every tiderace authoring-surface error."""


class TideraceDefinitionError(TideraceError):
    """A bad declaration — e.g. `@provides` on a function whose provided type can't be determined,
    or an unknown scope. Surfaced where the decorator is applied."""


class TideraceResolutionError(TideraceError):
    """A dependency can't be wired by type: an unannotated parameter, no provider for the requested
    type, or an ambiguous type with several providers. Ambiguity is a hard error — never 'first wins'
    (that would reintroduce exactly the implicit magic type-DI exists to remove)."""
