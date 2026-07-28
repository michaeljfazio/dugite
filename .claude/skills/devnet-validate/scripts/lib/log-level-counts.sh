# Shared log-level counting for devnet-validate evidence tooling.
#
# Sourced by analyze-evidence.sh AND generate-release-report.sh so the two can
# never disagree on what counts as an error/warn (#916: the report generator
# used a case-insensitive substring match, so `error=` fields on INFO lines
# inflated log_errors by thousands while the analyzer said NO ANOMALIES).
#
# Case-sensitive, anchored to the log-level token position so the lowercase
# substring `error=...` in benign WARN/INFO lines doesn't match.
# cardano-node 11.0.1+ uses new-tracer namespaces — match both legacy
# `TraceForgedInvalidBlock` and new `AddBlockValidation.InvalidBlock` /
# `Forge.Loop.ForgedInvalidBlock`.

LOG_ERROR_PATTERN=' ERROR | panicked|TraceForgedInvalidBlock|AddBlockValidation\.InvalidBlock|Forge\.Loop\.ForgedInvalidBlock'
LOG_WARN_PATTERN=' WARN | stale intersection'

# count_log_errors <logfile> — echoes the number of error-class lines.
count_log_errors() {
    grep -cE "$LOG_ERROR_PATTERN" "$1" || true
}

# count_log_warns <logfile> — echoes the number of warn-class lines.
count_log_warns() {
    grep -cE "$LOG_WARN_PATTERN" "$1" || true
}
