"""Type-driven resolution — riptide's identity. A parameter is wired to the provider whose returned
**type** matches its annotation. This is a *discovery-layer* concern: it turns `param: T` into a
provider **name**, producing exactly the `deps: [name]` the frozen Rust `FixtureGraph` already
consumes — so the banner feature costs the engine nothing (ADR-E012)."""
from __future__ import annotations

import inspect
import typing

from ._errors import RiptideResolutionError


def provided_type(fn) -> type | None:
    """The type a provider hands back: its return annotation, unwrapping `Iterator[T]`/`Generator[T,
    ...]` for yield-style providers. `None` when it can't be determined (caller errors)."""
    hints = typing.get_type_hints(fn)
    ret = hints.get("return")
    if ret is None:
        return None
    if inspect.isgeneratorfunction(fn):
        # `-> Iterator[Db]`/`-> Generator[Db, ...]` ⇒ Db; tolerate a plain `-> Db` (common in
        # migrated code) by using the annotation itself when it carries no element type.
        args = typing.get_args(ret)
        return args[0] if args else ret
    return ret


def build_type_index(specs) -> dict:
    """`type -> [ProviderSpec]`. Several providers may share a type; ambiguity is resolved (or
    rejected) per-request in `resolve_params`, not here."""
    index: dict = {}
    for spec in specs:
        index.setdefault(spec.provides, []).append(spec)
    return index


def resolve_params(fn, index: dict, *, skip: tuple = ()) -> dict:
    """`param-name -> provider-name`, wired by type. Raises `RiptideResolutionError` for an
    unannotated parameter, an unprovided type, or an ambiguous type (disambiguate with
    `Annotated[T, "<provider-name>"]`)."""
    hints = typing.get_type_hints(fn, include_extras=True)
    deps: dict = {}
    for pname in inspect.signature(fn).parameters:
        if pname in skip:
            continue
        annotation = hints.get(pname)
        if annotation is None:
            raise RiptideResolutionError(
                f"{fn.__name__}({pname}): parameter has no type annotation — riptide wires by type, "
                f"so write `{pname}: <Type>`"
            )

        key, want_name = annotation, None
        if typing.get_origin(annotation) is typing.Annotated:
            key, *meta = typing.get_args(annotation)
            want_name = next((m for m in meta if isinstance(m, str)), None)

        candidates = list(index.get(key, ()))
        if want_name is not None:
            candidates = [c for c in candidates if c.name == want_name]

        if not candidates:
            qualifier = f" named {want_name!r}" if want_name else ""
            raise RiptideResolutionError(
                f"{fn.__name__}({pname}: {_type_name(key)}): no provider{qualifier} returns "
                f"{_type_name(key)}"
            )
        if len(candidates) > 1:
            names = ", ".join(sorted(c.name for c in candidates))
            raise RiptideResolutionError(
                f"{fn.__name__}({pname}: {_type_name(key)}): ambiguous — {len(candidates)} providers "
                f"({names}); disambiguate with Annotated[{_type_name(key)}, \"<provider-name>\"]"
            )
        deps[pname] = candidates[0].name
    return deps


def _type_name(t) -> str:
    return getattr(t, "__name__", str(t))
