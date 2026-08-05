#!/usr/bin/env bash
# Fixture stub standing in for "cardano-cli" in cli-surface-parity-selftest.sh.
#
# Tree:
#   alpha
#     one    (leaf)
#     two    (leaf)
#   beta     (leaf, TWO-PARAGRAPH description with an internal blank line and
#            an ANSI color code — this is what regressed the real parser
#            against real cardano-cli output; see cli-surface-parity.sh)
#   gamma    (leaf)
#
# Deliberately mimics optparse-applicative's "Available commands:" header
# and indentation style (2-space name column, 3+-space continuation).
set -uo pipefail

case "$*" in
"--help")
    cat <<'EOF'
Usage: fixture-reference-cli ( alpha | beta | gamma )

  Fixture top level.

Available options:
  -h,--help                Show this help text

Available commands:
  alpha                     Alpha command group.
  beta                      Beta leaf with two paragraphs.

EOF
    printf '                           \033[93mSecond paragraph after a blank line — the regression case.\033[0m\n'
    cat <<'EOF'
  gamma                     Gamma leaf command.
EOF
    ;;
"alpha --help")
    cat <<'EOF'
Usage: fixture-reference-cli alpha ( one | two )

  Alpha command group.

Available options:
  -h,--help                Show this help text

Available commands:
  one                       Alpha one leaf.
  two                       Alpha two leaf.
EOF
    ;;
"alpha one --help")
    cat <<'EOF'
Usage: fixture-reference-cli alpha one

  Alpha one leaf.

Available options:
  -h,--help                Show this help text
EOF
    ;;
"alpha two --help")
    cat <<'EOF'
Usage: fixture-reference-cli alpha two

  Alpha two leaf.

Available options:
  -h,--help                Show this help text
EOF
    ;;
"beta --help")
    cat <<'EOF'
Usage: fixture-reference-cli beta

  Beta leaf with two paragraphs.

Available options:
  -h,--help                Show this help text
EOF
    ;;
"gamma --help")
    cat <<'EOF'
Usage: fixture-reference-cli gamma

  Gamma leaf command.

Available options:
  -h,--help                Show this help text
EOF
    ;;
*)
    echo "fixture-reference-cli: unrecognized invocation: $*" >&2
    exit 1
    ;;
esac
