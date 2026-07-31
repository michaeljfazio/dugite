#!/usr/bin/env python3
"""Generate the third-party licenses documentation page from Cargo metadata.

Usage:
    python3 scripts/dev/generate-licenses.py > docs/src/reference/third-party-licenses.md
"""

import json
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone


def git_short_sha():
    r = subprocess.run(["git", "rev-parse", "--short", "HEAD"],
                       capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else "unknown"


def main():
    result = subprocess.run(
        ["cargo", "metadata", "--format-version=1"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print("Error: cargo metadata failed", file=sys.stderr)
        sys.exit(1)

    meta = json.loads(result.stdout)
    # Exclude first-party workspace members. Use cargo's own workspace_members
    # list rather than a name prefix — a prefix silently stops matching the day
    # the project is renamed, which is exactly how this script spent months
    # listing every dugite-* crate as a third-party dependency.
    ws_ids = set(meta.get("workspace_members", []))
    ws_names = {pkg["name"] for pkg in meta["packages"] if pkg["id"] in ws_ids}

    # Collect deps (latest version per name)
    deps = {}
    for pkg in meta["packages"]:
        if pkg["name"] in ws_names:
            continue
        name = pkg["name"]
        if name not in deps or pkg["version"] > deps[name]["version"]:
            deps[name] = {
                "version": pkg["version"],
                "license": (pkg.get("license") or "Unknown").strip(),
                "repository": (pkg.get("repository") or ""),
                "description": (pkg.get("description") or "").strip()[:120],
            }

    def normalize_license(lic):
        return lic.replace("/", " OR ")

    license_groups = defaultdict(list)
    for name in sorted(deps.keys()):
        d = deps[name]
        license_groups[normalize_license(d["license"])].append((name, d))

    summary = defaultdict(int)
    for lic, pkgs in license_groups.items():
        summary[lic] += len(pkgs)

    # Key direct dependencies
    key_crates = [
        # Async runtime & networking
        "tokio", "tokio-util", "hyper", "reqwest", "socket2", "hickory-resolver",
        # gRPC (dugite-rpc)
        "tonic", "prost",
        # Serialization
        "serde", "serde_json", "minicbor", "bincode", "toml",
        # Crypto
        "blake2", "blake2b_simd", "sha2", "sha3",
        "ed25519-dalek", "curve25519-dalek", "blst", "k256",
        "kes-summed-ed25519", "vrf_dalek",
        # Numerics
        "num-bigint", "num-rational", "dashu-int",
        # Storage
        "memmap2", "fs2", "imbl", "crc32fast", "zstd", "tar",
        # Encoding helpers
        "hex", "bs58", "bech32", "base64",
        # Mithril
        "mithril-client",
        # CLI / TUI
        "clap", "ratatui", "crossterm", "indicatif",
        # Observability
        "tracing", "tracing-subscriber",
        # Concurrency
        "dashmap", "parking_lot", "arc-swap", "rayon",
        # Misc
        "rand", "chrono",
    ]

    lines = []
    lines.append("# Third-Party Licenses")
    lines.append("")
    lines.append("Dugite depends on a number of open-source Rust crates. This page documents")
    lines.append("all third-party dependencies and their license terms.")
    lines.append("")
    lines.append(f"**Total dependencies:** {len(deps)}")
    lines.append("")
    lines.append(
        f"_Generated from `Cargo.lock` on {datetime.now(timezone.utc):%Y-%m-%d} "
        f"at commit `{git_short_sha()}`. Regenerate with `just licenses` after any "
        "dependency change — nothing in CI does it for you._"
    )
    lines.append("")
    lines.append(
        "Dugite itself is licensed under **Apache-2.0**. Counts below are per "
        "unique crate name (the highest version, where a crate appears at "
        "several versions) across all target platforms, so they include "
        "target-gated dependencies such as the `windows-*` family that are not "
        "built on Linux or macOS."
    )
    lines.append("")

    lines.append("## License Summary")
    lines.append("")
    lines.append("| License | Count |")
    lines.append("|---------|-------|")
    for lic in sorted(summary.keys(), key=lambda l: -summary[l]):
        lines.append(f"| {lic} | {summary[lic]} |")
    lines.append("")

    # Flag anything that is not unambiguously permissive, computed from the
    # data so this note cannot go stale independently of the tables.
    PERMISSIVE_TOKENS = (
        "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "BSD-1-Clause",
        "ISC", "Zlib", "Unlicense", "CC0-1.0", "MIT-0", "Unicode-3.0",
        "BlueOak-1.0.0", "BSL-1.0", "CDLA-Permissive-2.0",
        "Apache-2.0 WITH LLVM-exception", "MPL-2.0",
    )

    def is_plain_permissive(lic):
        """True when every alternative in the expression is permissive."""
        if lic == "Unknown":
            return False
        # An OR expression is fine if ANY alternative is plainly permissive.
        alts = [a.strip() for a in lic.split(" OR ")]
        for alt in alts:
            body = alt.strip("()")
            if all(
                any(part.strip() == tok for tok in PERMISSIVE_TOKENS)
                for part in body.split(" AND ")
            ):
                return True
        return False

    flagged = sorted(
        (name, deps[name])
        for name in deps
        if not is_plain_permissive(normalize_license(deps[name]["license"]))
    )
    if flagged:
        lines.append("## Licenses Needing Review")
        lines.append("")
        lines.append(
            "Everything not covered by a plainly permissive license (MIT, "
            "Apache-2.0, BSD, ISC, Zlib, CC0, Unicode-3.0, BlueOak, "
            "CDLA-Permissive, or MPL-2.0 file-level copyleft). Review these "
            "before shipping a binary distribution:"
        )
        lines.append("")
        lines.append("| Crate | Version | License |")
        lines.append("|-------|---------|---------|")
        for name, d in flagged:
            lines.append(f"| {name} | {d['version']} | {d['license']} |")
        lines.append("")

    lines.append("## Key Dependencies")
    lines.append("")
    lines.append("These are the primary libraries that Dugite directly depends on:")
    lines.append("")
    lines.append("| Crate | Version | License | Description |")
    lines.append("|-------|---------|---------|-------------|")
    for crate_name in key_crates:
        if crate_name in deps:
            d = deps[crate_name]
            desc = d["description"].replace("|", "-")
            if len(desc) > 80:
                desc = desc[:77] + "..."
            repo = d["repository"]
            name_link = f"[{crate_name}]({repo})" if repo else crate_name
            lines.append(f"| {name_link} | {d['version']} | {d['license']} | {desc} |")
    lines.append("")

    lines.append("## All Dependencies")
    lines.append("")
    lines.append("Complete list of all third-party crates used by Dugite, sorted alphabetically.")
    lines.append("")
    lines.append("| Crate | Version | License |")
    lines.append("|-------|---------|---------|")
    for name in sorted(deps.keys()):
        d = deps[name]
        repo = d["repository"]
        name_link = f"[{name}]({repo})" if repo else name
        lines.append(f"| {name_link} | {d['version']} | {d['license']} |")
    lines.append("")
    lines.append("## Regenerating This Page")
    lines.append("")
    lines.append("This page is generated from `Cargo.lock` metadata. To regenerate after dependency changes:")
    lines.append("")
    lines.append("```bash")
    lines.append("just licenses")
    lines.append("# equivalently:")
    lines.append("python3 scripts/dev/generate-licenses.py > docs/src/reference/third-party-licenses.md")
    lines.append("```")
    lines.append("")
    lines.append("Native (non-Rust) code reaches the build through a handful of crates worth")
    lines.append("naming explicitly:")
    lines.append("")
    lines.append("| Component | Arrives via | Notes |")
    lines.append("|-----------|-------------|-------|")
    lines.append("| BLS12-381 (`blst`, C) | direct dependency of `dugite-uplc` | Plutus BLS builtins; also pulled transitively by `mithril-stm` |")
    lines.append("| zstd (C) | `zstd` / `zstd-sys` | Mithril snapshot decompression |")
    lines.append("| ring / aws-lc (C, asm) | rustls stack under `reqwest`, `tonic`, `mithril-client` | TLS |")
    lines.append("| libsecp256k1 (C) | **not used** | `dugite-uplc` deliberately uses pure-Rust `k256` instead of `secp256k1-sys` |")
    lines.append("| libsodium | **not used** | VRF is pure Rust via a forked `curve25519-dalek` |")
    lines.append("")
    lines.append("Four dependencies are git-pinned rather than published to crates.io:")
    lines.append("`vrf_dalek` and `curve25519-dalek-fork` (the IETF-03 VRF-compatible fork,")
    lines.append("required for Praos leader election), plus `amaru-uplc` and `cddl`, which are")
    lines.append("both development/conformance-only.")

    print("\n".join(lines))


if __name__ == "__main__":
    main()
