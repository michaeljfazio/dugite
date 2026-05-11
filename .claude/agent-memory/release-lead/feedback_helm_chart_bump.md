---
name: Bump Helm chart on every release
description: Release lifecycle must include updating charts/dugite-node/Chart.yaml — both appVersion (to match the new node version) and chart version (so helm upgrade detects the change)
type: feedback
---

When cutting a new release, the chart at `charts/dugite-node/Chart.yaml` must be bumped as part of the release lifecycle, not as a separate follow-up.

**Why:** During v1.4.0, the chart's `appVersion` was discovered stuck at `1.0.3-alpha` — it had silently missed the 1.1.0, 1.2.0, and 1.3.0 releases. The user had to ask for a separate chart-update step after the GitHub release was already published. The chart's `image.tag` defaults to `appVersion` (`values.yaml`: `tag: ""  # Defaults to Chart appVersion`), so a stale appVersion means `helm install`/`upgrade` deploys an old image. This is a real correctness bug for chart consumers, not cosmetic.

**How to apply:**
- Add a "bump Helm chart" step to the release checklist alongside the workspace `Cargo.toml` version bump. Do it in the SAME bump commit (or as a sibling commit on the same release) — not after the GitHub release is published.
- Update both fields in `charts/dugite-node/Chart.yaml`:
  - `appVersion: "X.Y.Z"` — match the new node version exactly (no `v` prefix, quoted string)
  - `version: A.B.C` — bump the chart's own SemVer. Default rule: minor bump on minor node release, patch bump on patch node release. Helm clients use this for upgrade detection and caching, so it MUST change whenever appVersion changes.
- Run `helm lint charts/dugite-node` before committing. Treat `[INFO]` notes as fine; treat `[WARNING]` or `[ERROR]` as blockers.
- Existing chart bump commit convention: `chore(chart): bump appVersion to X.Y.Z, chart version to A.B.C`. See `30587917f` for the v1.4.0 catch-up format.
- Only one chart exists in the repo today (`charts/dugite-node`). If more are added later, bump all of them.
