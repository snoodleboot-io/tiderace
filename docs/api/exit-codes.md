# Exit Codes

| Code | Meaning |
|---|---|
| `0` | All tests passed (or all skipped by impact analysis) |
| `1` | One or more tests failed |
| `2` | tiderace internal error (collection failure, DB error, etc.) |

CI systems use exit code `1` to mark a build as failed.
