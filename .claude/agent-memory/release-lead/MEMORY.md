# Release Lead Memory Index

- [v1.1.0-alpha release](project_release_v1_1_0_alpha.md) — release details, milestone commit, pre-existing macOS CI cross-compile blocker
- [gh release create --notes flag](feedback_gh_release_flags.md) — use --notes not --body for gh release create body text
- [v1.3.0 release](project_release_v1_3_0.md) — ChainSel correctness, ledger parity, network stability; use cargo update --workspace not generate-lockfile
- [lockfile update method](feedback_lockfile_update.md) — use cargo update --workspace (not generate-lockfile) to avoid pulling in incompatible transitive dep upgrades
- [v1.4.0 release](project_release_v1_4_0.md) — first canonical block milestone; forge hardening, Dijkstra era, 215+ tests; CI has 6 jobs total (~45 min); pre-push rebase needed
- [Bump Helm chart on every release](feedback_helm_chart_bump.md) — charts/dugite-node Chart.yaml (appVersion + chart version) must be bumped as part of the release, not after
