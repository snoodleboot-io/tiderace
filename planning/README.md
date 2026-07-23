# Planning

Development planning docs for tiderace. This is **internal, feature-oriented planning** — not user documentation.

- **User documentation** and **whole-system design** (architecture, module design, the cross-cutting ADR log, "how it works") live in [`../docs/`](../docs/).
- **Per-feature** planning — the PRD, ADR(s), and design doc *specific to one feature* — lives here, inside that feature's folder.

## Lifecycle folders

A feature moves through three stages, each a folder:

| Folder        | Meaning                                              |
| ------------- | ---------------------------------------------------- |
| `backlog/`    | Planned, not yet started. New feature folders begin here. |
| `current/`    | In active development.                               |
| `completed/`  | Shipped. Kept as a historical record.                |

To advance a feature, move its folder between these directories.

## Feature folder layout

Each feature is a folder (kebab-case name). Inside it, the planning artifacts for that
feature:

```
backlog/
└── my-feature/
    ├── PRD.md       # Product requirements — problem, goals, scope, non-goals
    ├── ADR.md       # Architecture decision(s) specific to this feature
    └── DESIGN.md    # Design doc — how it will be built
```

Add `RFC.md`, notes, diagrams, or other artifacts as needed — these three are the baseline.
See [`backlog/_template/`](backlog/_template/) for starter stubs.
