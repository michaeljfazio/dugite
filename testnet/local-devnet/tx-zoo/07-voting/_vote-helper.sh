#!/usr/bin/env bash
# Shared helper for the 07-voting scripts.
# Resolves the InfoAction proposal id stored by 06a.

zoo_gov_action_id() {
    local f="$ZOO_BUILT/gov-action-info.id"
    if [ ! -s "$f" ]; then
        return 1
    fi
    cat "$f"
}

# vote_file_for VOTER_KIND DECISION
# VOTER_KIND in: drep | spo | cc-hot
# DECISION   in: yes | no | abstain
# Writes a vote file referencing the InfoAction and prints its path.
zoo_vote_file() {
    local kind="$1" decision="$2" voter_vkey="$3" out="$4"
    local action_id; action_id=$(zoo_gov_action_id) || return 1
    local tx_hash="${action_id%#*}"
    local tx_ix="${action_id#*#}"
    local decision_flag
    case "$decision" in
        yes)     decision_flag="--yes" ;;
        no)      decision_flag="--no"  ;;
        abstain) decision_flag="--abstain" ;;
        *) return 1 ;;
    esac
    local kind_flag
    case "$kind" in
        drep)   kind_flag="--drep-verification-key-file" ;;
        spo)    kind_flag="--cold-verification-key-file" ;;
        cc-hot) kind_flag="--cc-hot-verification-key-file" ;;
        *) return 1 ;;
    esac
    cardano-cli conway governance vote create \
        "$decision_flag" \
        --governance-action-tx-id "$tx_hash" \
        --governance-action-index "$tx_ix" \
        "$kind_flag" "$voter_vkey" \
        --out-file "$out"
}
