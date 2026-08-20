# Architect Agent Memory

## Design Decisions

- [P2P Governor Upgrade Architecture](project_p2p_governor.md) — Five-phase plan for issue #138: BLP connect fix, state-machine timeouts, churn correctness, peer-sharing integration, known-peer target enforcement.
- [#1088 snapshot ordering: sort at the boundary](project_1088_snapshot_ordering_sort_at_boundary.md) — ~40 nondeterministic map fields; option (a) over OrdMap conversion (133 sites, B-tree depth on the hot maps).

## Structural Debt

- [SNAPSHOT 38 extend-in-place is void](project_snapshot38_extend_in_place_void.md) — v2.8.0 is tagged, so 38→39 is mandatory; its guard fails locally but is vacuous in CI (shallow checkout fetches no tags).
