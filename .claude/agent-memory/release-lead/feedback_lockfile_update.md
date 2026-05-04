---
name: lockfile update for version bump
description: Use cargo update --workspace, not cargo generate-lockfile, when bumping workspace version
type: feedback
---

Use `cargo update --workspace` to update the Cargo.lock after a workspace version bump. Do NOT use `cargo generate-lockfile`.

**Why:** `cargo generate-lockfile` resolves ALL dependencies to their latest compatible versions, which can pull in transitive dependency upgrades that are incompatible with pinned crates. In this repo, it upgraded `mithril-aggregator-discovery` to a version incompatible with `mithril-client 0.13.2`, causing build failures. `cargo update --workspace` updates only the workspace packages (the dugite-* crates) and leaves all external dependency pins unchanged.

**How to apply:** After editing `[workspace.package] version` in root Cargo.toml, always run `cargo update --workspace` to update the lockfile. Verify with `cargo build --all-targets` before proceeding.
