#!/usr/bin/env bash
# Fixture stub standing in for "dugite-cli" in cli-surface-parity-selftest.sh.
# Variant selected by $FIXTURE_VARIANT:
#
#   full      — mirrors fixture-reference-cli.sh exactly (alpha/{one,two},
#               beta, gamma). Expected: 0 missing, 0 superset, PASS.
#   missing   — drops "alpha two" entirely. Expected: 1 missing (alpha two),
#               the deliberate RED demonstration.
#   superset  — full tree PLUS an extra leaf "delta" with no cardano-cli
#               counterpart. Expected: 0 missing, 1 superset, still PASS
#               (superset is informational, never failing).
#
# Uses clap-derive's header spelling ("Commands:", not "Available commands:")
# and its no-wrapping style, so the selftest also exercises both header forms
# and confirms the parser doesn't accidentally depend on cardano-cli's
# Haskell-specific formatting.
set -uo pipefail

VARIANT="${FIXTURE_VARIANT:-full}"

case "$*" in
"--help")
    if [[ "$VARIANT" == "superset" ]]; then
        cat <<'EOF'
Usage: fixture-target-cli <COMMAND>

Commands:
  alpha   Alpha command group
  beta    Beta leaf
  gamma   Gamma leaf command
  delta   Delta leaf with no cardano-cli counterpart

Options:
  -h, --help  Print help
EOF
    else
        cat <<'EOF'
Usage: fixture-target-cli <COMMAND>

Commands:
  alpha   Alpha command group
  beta    Beta leaf
  gamma   Gamma leaf command

Options:
  -h, --help  Print help
EOF
    fi
    ;;
"alpha --help")
    if [[ "$VARIANT" == "missing" ]]; then
        cat <<'EOF'
Usage: fixture-target-cli alpha <COMMAND>

Commands:
  one   Alpha one leaf

Options:
  -h, --help  Print help
EOF
    else
        cat <<'EOF'
Usage: fixture-target-cli alpha <COMMAND>

Commands:
  one   Alpha one leaf
  two   Alpha two leaf

Options:
  -h, --help  Print help
EOF
    fi
    ;;
"alpha one --help")
    cat <<'EOF'
Alpha one leaf

Usage: fixture-target-cli alpha one

Options:
  -h, --help  Print help
EOF
    ;;
"alpha two --help")
    cat <<'EOF'
Alpha two leaf

Usage: fixture-target-cli alpha two

Options:
  -h, --help  Print help
EOF
    ;;
"beta --help")
    cat <<'EOF'
Beta leaf

Usage: fixture-target-cli beta

Options:
  -h, --help  Print help
EOF
    ;;
"gamma --help")
    cat <<'EOF'
Gamma leaf command

Usage: fixture-target-cli gamma

Options:
  -h, --help  Print help
EOF
    ;;
"delta --help")
    cat <<'EOF'
Delta leaf with no cardano-cli counterpart

Usage: fixture-target-cli delta

Options:
  -h, --help  Print help
EOF
    ;;
*)
    echo "error: unrecognized subcommand or path: $*" >&2
    exit 2
    ;;
esac
