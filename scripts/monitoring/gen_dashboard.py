#!/usr/bin/env python3
"""Generate the Dugite Grafana dashboard JSON.

The dashboard surfaces every metric exposed by ``dugite-node`` (see
``crates/dugite-node/src/metrics.rs``) and is organised into themed rows.

Run:

    python3 scripts/monitoring/gen_dashboard.py \
        > config/monitoring/grafana-dashboard.json
"""
from __future__ import annotations

import json
import sys
from typing import Any

DS = {"type": "prometheus", "uid": "${DS_PROMETHEUS}"}

# --- Layout helpers -------------------------------------------------------
# Grafana uses a 24-column grid. We allocate panels left-to-right inside the
# current row, breaking onto a new line when we run out of width.

_next_id = 1


def _alloc_id() -> int:
    global _next_id
    val = _next_id
    _next_id += 1
    return val


class Layout:
    def __init__(self) -> None:
        self.y = 0
        self.x = 0
        self.row_h = 0

    def place(self, w: int, h: int) -> dict[str, int]:
        if self.x + w > 24:
            self.y += self.row_h
            self.x = 0
            self.row_h = 0
        pos = {"h": h, "w": w, "x": self.x, "y": self.y}
        self.x += w
        self.row_h = max(self.row_h, h)
        return pos

    def row_break(self) -> None:
        if self.x > 0:
            self.y += self.row_h
            self.x = 0
            self.row_h = 0


LAYOUT = Layout()


def row(title: str) -> dict[str, Any]:
    LAYOUT.row_break()
    pos = {"h": 1, "w": 24, "x": 0, "y": LAYOUT.y}
    LAYOUT.y += 1
    return {
        "collapsed": False,
        "gridPos": pos,
        "id": _alloc_id(),
        "title": title,
        "type": "row",
    }


def target(expr: str, *, legend: str = "", instant: bool = False,
           ref: str = "A", interval: str = "") -> dict[str, Any]:
    t = {
        "datasource": DS,
        "expr": expr,
        "legendFormat": legend,
        "refId": ref,
    }
    if instant:
        t["instant"] = True
    if interval:
        t["interval"] = interval
    return t


def _common_field(unit: str = "", decimals: int | None = None,
                  thresholds: list[dict[str, Any]] | None = None,
                  min_v: Any = None, max_v: Any = None,
                  mappings: list[dict[str, Any]] | None = None,
                  color_mode: str = "thresholds",
                  color_fixed: str | None = None) -> dict[str, Any]:
    color: dict[str, Any] = {"mode": color_mode}
    if color_fixed is not None:
        color["fixedColor"] = color_fixed
    defaults: dict[str, Any] = {
        "color": color,
        "mappings": mappings or [],
        "thresholds": {
            "mode": "absolute",
            "steps": thresholds or [{"color": "green", "value": None}],
        },
    }
    if unit:
        defaults["unit"] = unit
    if decimals is not None:
        defaults["decimals"] = decimals
    if min_v is not None:
        defaults["min"] = min_v
    if max_v is not None:
        defaults["max"] = max_v
    return {"defaults": defaults, "overrides": []}


def stat(*, title: str, expr: str, w: int = 4, h: int = 4, unit: str = "",
         decimals: int | None = None, thresholds: list[dict[str, Any]] | None = None,
         legend: str = "", graph: str = "area", color_mode: str = "value",
         text_mode: str = "auto", mappings: list[dict[str, Any]] | None = None,
         color_fixed: str | None = None, desc: str = "",
         color_scheme: str = "thresholds") -> dict[str, Any]:
    panel = {
        "datasource": DS,
        "fieldConfig": _common_field(
            unit=unit, decimals=decimals, thresholds=thresholds,
            mappings=mappings, color_mode=color_scheme,
            color_fixed=color_fixed,
        ),
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "colorMode": color_mode,
            "graphMode": graph,
            "justifyMode": "auto",
            "orientation": "auto",
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "textMode": text_mode,
        },
        "targets": [target(expr, legend=legend)],
        "title": title,
        "type": "stat",
    }
    if desc:
        panel["description"] = desc
    return panel


def gauge(*, title: str, expr: str, w: int = 6, h: int = 5, unit: str = "",
          min_v: Any = 0, max_v: Any = 100, decimals: int | None = None,
          thresholds: list[dict[str, Any]] | None = None,
          legend: str = "", desc: str = "") -> dict[str, Any]:
    panel = {
        "datasource": DS,
        "fieldConfig": _common_field(
            unit=unit, decimals=decimals, thresholds=thresholds,
            min_v=min_v, max_v=max_v,
        ),
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "orientation": "auto",
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "showThresholdLabels": False,
            "showThresholdMarkers": True,
        },
        "targets": [target(expr, legend=legend)],
        "title": title,
        "type": "gauge",
    }
    if desc:
        panel["description"] = desc
    return panel


def timeseries(*, title: str, targets: list[dict[str, Any]], w: int = 12, h: int = 7,
               unit: str = "short", decimals: int | None = None, stacking: str = "none",
               fill_opacity: int = 12, line_width: int = 2,
               draw_style: str = "line", point_size: int = 5,
               legend_placement: str = "bottom", legend_calcs: list[str] | None = None,
               overrides: list[dict[str, Any]] | None = None,
               desc: str = "", min_v: Any = None, max_v: Any = None) -> dict[str, Any]:
    fld = _common_field(unit=unit, decimals=decimals, min_v=min_v, max_v=max_v)
    fld["defaults"]["color"] = {"mode": "palette-classic"}
    fld["defaults"]["custom"] = {
        "axisCenteredZero": False,
        "axisColorMode": "text",
        "axisLabel": "",
        "axisPlacement": "auto",
        "barAlignment": 0,
        "drawStyle": draw_style,
        "fillOpacity": fill_opacity,
        "gradientMode": "opacity",
        "hideFrom": {"legend": False, "tooltip": False, "viz": False},
        "lineInterpolation": "smooth",
        "lineWidth": line_width,
        "pointSize": point_size,
        "scaleDistribution": {"type": "linear"},
        "showPoints": "never",
        "spanNulls": True,
        "stacking": {"group": "A", "mode": stacking},
        "thresholdsStyle": {"mode": "off"},
    }
    if overrides is not None:
        fld["overrides"] = overrides
    panel = {
        "datasource": DS,
        "fieldConfig": fld,
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "legend": {
                "calcs": legend_calcs or ["mean", "last"],
                "displayMode": "table",
                "placement": legend_placement,
                "showLegend": True,
            },
            "tooltip": {"mode": "multi", "sort": "desc"},
        },
        "targets": targets,
        "title": title,
        "type": "timeseries",
    }
    if desc:
        panel["description"] = desc
    return panel


def bargauge(*, title: str, targets: list[dict[str, Any]], w: int = 8, h: int = 6,
             unit: str = "short", min_v: Any = 0, max_v: Any = None,
             thresholds: list[dict[str, Any]] | None = None,
             display_mode: str = "gradient", orientation: str = "horizontal",
             desc: str = "") -> dict[str, Any]:
    panel = {
        "datasource": DS,
        "fieldConfig": _common_field(
            unit=unit, thresholds=thresholds, min_v=min_v, max_v=max_v,
        ),
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "displayMode": display_mode,
            "minVizHeight": 10,
            "minVizWidth": 0,
            "orientation": orientation,
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "showUnfilled": True,
            "valueMode": "color",
        },
        "targets": targets,
        "title": title,
        "type": "bargauge",
    }
    if desc:
        panel["description"] = desc
    return panel


def piechart(*, title: str, targets: list[dict[str, Any]], w: int = 8, h: int = 7,
             unit: str = "short", desc: str = "") -> dict[str, Any]:
    fld = _common_field(unit=unit)
    fld["defaults"]["color"] = {"mode": "palette-classic"}
    panel = {
        "datasource": DS,
        "fieldConfig": fld,
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "displayLabels": ["name", "percent"],
            "legend": {
                "displayMode": "list",
                "placement": "right",
                "showLegend": True,
                "values": ["value"],
            },
            "pieType": "donut",
            "reduceOptions": {
                "calcs": ["lastNotNull"],
                "fields": "",
                "values": False,
            },
            "tooltip": {"mode": "multi", "sort": "desc"},
        },
        "targets": targets,
        "title": title,
        "type": "piechart",
    }
    if desc:
        panel["description"] = desc
    return panel


def barchart(*, title: str, targets: list[dict[str, Any]], w: int = 12, h: int = 7,
             unit: str = "short", orientation: str = "horizontal",
             desc: str = "") -> dict[str, Any]:
    fld = _common_field(unit=unit)
    fld["defaults"]["color"] = {"mode": "palette-classic"}
    panel = {
        "datasource": DS,
        "fieldConfig": fld,
        "gridPos": LAYOUT.place(w, h),
        "id": _alloc_id(),
        "options": {
            "barRadius": 0,
            "barWidth": 0.7,
            "fullHighlight": False,
            "groupWidth": 0.7,
            "legend": {
                "displayMode": "list",
                "placement": "bottom",
                "showLegend": True,
            },
            "orientation": orientation,
            "showValue": "auto",
            "stacking": "none",
            "tooltip": {"mode": "single", "sort": "none"},
            "xTickLabelRotation": 0,
            "xTickLabelSpacing": 0,
        },
        "targets": targets,
        "title": title,
        "type": "barchart",
    }
    if desc:
        panel["description"] = desc
    return panel


# --- Common thresholds ----------------------------------------------------

PCT_SYNC = [
    {"color": "red", "value": None},
    {"color": "orange", "value": 50},
    {"color": "yellow", "value": 90},
    {"color": "green", "value": 99},
]
PEER_OK = [
    {"color": "red", "value": None},
    {"color": "orange", "value": 1},
    {"color": "yellow", "value": 5},
    {"color": "green", "value": 10},
]
TIP_AGE = [
    {"color": "green", "value": None},
    {"color": "yellow", "value": 60},
    {"color": "orange", "value": 120},
    {"color": "red", "value": 300},
]
DISK_AVAILABLE = [
    {"color": "red", "value": None},
    {"color": "orange", "value": 5_000_000_000},
    {"color": "yellow", "value": 20_000_000_000},
    {"color": "green", "value": 50_000_000_000},
]
CPU = [
    {"color": "green", "value": None},
    {"color": "yellow", "value": 60},
    {"color": "orange", "value": 80},
    {"color": "red", "value": 95},
]

ON_OFF_MAP = [
    {"options": {"0": {"color": "text", "index": 0, "text": "OFF"},
                 "1": {"color": "green", "index": 1, "text": "ON"}},
     "type": "value"},
]

DIFFUSION_MAP = [
    {"options": {"0": {"color": "blue", "index": 0, "text": "Initiator"},
                 "1": {"color": "green", "index": 1, "text": "InitiatorAndResponder"}},
     "type": "value"},
]


# --- Build dashboard ------------------------------------------------------

panels: list[dict[str, Any]] = []


# --- Row: Headline --------------------------------------------------------
panels.append(row("Headline"))

panels.append(gauge(
    title="Sync Progress",
    expr="dugite_sync_progress_percent / 100",
    w=6, h=5, unit="percent", min_v=0, max_v=100, decimals=2,
    thresholds=PCT_SYNC,
    desc="Headers-since-genesis / max(known peer tip). 100 = caught up.",
))
panels.append(stat(
    title="Tip Age",
    expr="dugite_tip_age_seconds",
    w=4, h=5, unit="s",
    thresholds=TIP_AGE,
    desc="Wall-clock age of our current chain tip. Anything > a few minutes "
         "while peers report newer slots means we are falling behind.",
))
panels.append(stat(
    title="Block Height",
    expr="dugite_block_number",
    w=4, h=5, unit="short",
    graph="area", color_mode="value",
    desc="Absolute block number (slot 0 = genesis block 0).",
))
panels.append(stat(
    title="Epoch",
    expr="dugite_epoch_number",
    w=4, h=5, unit="short", graph="area",
    desc="Current absolute epoch number.",
))
panels.append(stat(
    title="Slot",
    expr="dugite_slot_number",
    w=3, h=5, unit="short", graph="area",
    desc="Current absolute slot number.",
))
panels.append(stat(
    title="Peers",
    expr="dugite_peers_connected",
    w=3, h=5, unit="short",
    thresholds=PEER_OK,
    desc="Sum of all currently connected upstream/downstream peers.",
))


# --- Row: Identity & Build -----------------------------------------------
panels.append(row("Node Identity & Static Config"))

panels.append(stat(
    title="Network Magic",
    expr="dugite_network_magic",
    w=3, h=3, unit="short",
    mappings=[{"type": "value", "options": {
        "1": {"text": "preprod", "color": "blue", "index": 0},
        "2": {"text": "preview", "color": "purple", "index": 1},
        "764824073": {"text": "mainnet", "color": "green", "index": 2},
    }}],
    color_mode="background",
    desc="Cardano network magic (1=preprod, 2=preview, 764824073=mainnet).",
))
panels.append(stat(
    title="Block Producer",
    expr="dugite_is_block_producer",
    w=3, h=3, mappings=ON_OFF_MAP,
    color_mode="background",
    desc="1 when configured with KES + VRF + opcert (forging enabled).",
))
panels.append(stat(
    title="Diffusion Mode",
    expr="dugite_diffusion_mode",
    w=4, h=3, mappings=DIFFUSION_MAP,
    color_mode="background",
    desc="0 = Initiator-only (client), 1 = Initiator-and-Responder (relay/BP).",
))
panels.append(stat(
    title="Peer Sharing",
    expr="dugite_peer_sharing_enabled",
    w=3, h=3, mappings=ON_OFF_MAP,
    color_mode="background",
    desc="Whether this node advertises and consumes the peer-sharing mini-protocol.",
))
panels.append(stat(
    title="Active Slot Coeff",
    expr="dugite_active_slots_coeff_x1000 / 1000",
    w=3, h=3, decimals=3,
    desc="Praos f-coefficient — expected fraction of active slots.",
))
panels.append(stat(
    title="Slot Length",
    expr="dugite_slot_length_ms / 1000",
    w=3, h=3, unit="s", decimals=1,
    desc="Wall-clock duration of a single slot.",
))
panels.append(stat(
    title="Epoch Length",
    expr="dugite_epoch_length",
    w=3, h=3, unit="short",
    desc="Number of slots per epoch.",
))
panels.append(stat(
    title="Uptime",
    expr="dugite_uptime_seconds",
    w=2, h=3, unit="s",
    desc="Seconds since the node process started.",
))


# --- Row: Sync & Throughput ----------------------------------------------
panels.append(row("Sync & Throughput"))

panels.append(timeseries(
    title="Sync Progress (%)",
    targets=[target("dugite_sync_progress_percent / 100", legend="sync %")],
    w=12, h=7, unit="percent", min_v=0, max_v=100,
    fill_opacity=20,
))
panels.append(timeseries(
    title="Block Height vs Best Known Peer Slot",
    targets=[
        target("dugite_block_number", legend="block #"),
        target("dugite_slot_number", legend="our slot"),
        target("dugite_max_peer_tip_slot", legend="max peer tip slot"),
    ],
    w=12, h=7, unit="short",
    desc="Our position vs the highest slot any peer has advertised — gap = "
         "how far we still need to sync.",
))
panels.append(timeseries(
    title="Tip Age & ChainSync Idle",
    targets=[
        target("dugite_tip_age_seconds", legend="tip age"),
        target("dugite_chainsync_idle_seconds", legend="chainsync idle"),
    ],
    w=12, h=6, unit="s",
    desc="Tip age (chain-time vs wall-clock) and time since last ChainSync "
         "MsgRollForward — both should hover near zero at tip.",
))
panels.append(timeseries(
    title="Block Throughput (per 1m)",
    targets=[
        target("rate(dugite_blocks_received_total[1m]) * 60",
               legend="received /min"),
        target("rate(dugite_blocks_applied_total[1m]) * 60",
               legend="applied /min"),
    ],
    w=12, h=6, unit="short", decimals=1,
))
panels.append(timeseries(
    title="Rollbacks & Apply Failures",
    targets=[
        target("rate(dugite_rollback_count_total[5m]) * 60",
               legend="rollbacks /min"),
        target("rate(dugite_block_apply_failures_total[5m]) * 60",
               legend="apply failures /min"),
    ],
    w=12, h=6, unit="short",
    desc="Healthy chains see < 1 rollback/min and zero apply failures.",
))
panels.append(timeseries(
    title="Ledger Replay Duration",
    targets=[target("dugite_ledger_replay_duration_seconds", legend="replay")],
    w=12, h=6, unit="s",
    desc="Last cold-start replay duration (immutable → tip). Zero in steady state.",
))


# --- Row: Peers -----------------------------------------------------------
panels.append(row("Peers"))

panels.append(stat(
    title="Connected (total)",
    expr="dugite_peers_connected",
    w=4, h=4, thresholds=PEER_OK,
))
panels.append(stat(
    title="Inbound",
    expr="dugite_peers_inbound",
    w=4, h=4,
))
panels.append(stat(
    title="Outbound",
    expr="dugite_peers_outbound",
    w=4, h=4,
))
panels.append(stat(
    title="Duplex",
    expr="dugite_peers_duplex",
    w=4, h=4,
    desc="Peers we're using as both client and server simultaneously.",
))
panels.append(stat(
    title="Hot",
    expr="dugite_peers_hot",
    w=4, h=4, color_fixed="red", color_scheme="fixed",
    desc="Peers in the Hot state — active block-fetch / tx-submission.",
))
panels.append(stat(
    title="Warm + Cold",
    expr="dugite_peers_warm + dugite_peers_cold",
    w=4, h=4, color_fixed="orange", color_scheme="fixed",
))

panels.append(piechart(
    title="Peer State Distribution",
    targets=[
        target("dugite_peers_hot", legend="hot"),
        target("dugite_peers_warm", legend="warm"),
        target("dugite_peers_cold", legend="cold"),
    ],
    w=8, h=8,
    desc="Snapshot of peer-governor state buckets.",
))
panels.append(bargauge(
    title="Peer Governor Targets vs Actual",
    targets=[
        target("dugite_peer_governor_target{name=\"active\"}",
               legend="target active"),
        target("dugite_peers_hot", legend="actual hot"),
        target("dugite_peer_governor_target{name=\"established\"}",
               legend="target established"),
        target("dugite_peers_warm + dugite_peers_hot",
               legend="actual warm+hot"),
        target("dugite_peer_governor_target{name=\"known\"}",
               legend="target known"),
    ],
    w=8, h=8, unit="short", display_mode="lcd",
    desc="Targets drive the peer governor — actuals lagging behind targets "
         "indicates churn or topology shortage.",
))
panels.append(timeseries(
    title="Peer States Over Time",
    targets=[
        target("dugite_peers_cold", legend="cold"),
        target("dugite_peers_warm", legend="warm"),
        target("dugite_peers_hot", legend="hot"),
    ],
    w=8, h=8, unit="short", stacking="normal", fill_opacity=40,
))


# --- Row: Peer latency ---------------------------------------------------
panels.append(row("Peer Latency"))

panels.append(timeseries(
    title="RTT min / avg / max",
    targets=[
        target("dugite_peer_rtt_min_ms", legend="min"),
        target("dugite_peer_rtt_avg_ms", legend="avg"),
        target("dugite_peer_rtt_max_ms", legend="max"),
    ],
    w=12, h=7, unit="ms", decimals=1,
    desc="KeepAlive RTT samples across all connected peers (lower = closer).",
))
panels.append(stat(
    title="RTT Samples",
    expr="dugite_peer_rtt_samples",
    w=4, h=7,
    desc="Total KeepAlive RTT samples collected in the current window.",
))
panels.append(bargauge(
    title="RTT Distribution (bands, last sample)",
    targets=[
        target("dugite_peer_rtt_band_0_50", legend="0–50 ms"),
        target("dugite_peer_rtt_band_50_100", legend="50–100 ms"),
        target("dugite_peer_rtt_band_100_200", legend="100–200 ms"),
        target("dugite_peer_rtt_band_200_plus", legend="200+ ms"),
    ],
    w=8, h=7, unit="short", orientation="vertical", display_mode="gradient",
))
panels.append(timeseries(
    title="Handshake RTT (p50 / p95 / p99)",
    targets=[
        target("histogram_quantile(0.5, sum(rate(dugite_peer_handshake_rtt_ms_bucket[5m])) by (le))",
               legend="p50"),
        target("histogram_quantile(0.95, sum(rate(dugite_peer_handshake_rtt_ms_bucket[5m])) by (le))",
               legend="p95"),
        target("histogram_quantile(0.99, sum(rate(dugite_peer_handshake_rtt_ms_bucket[5m])) by (le))",
               legend="p99"),
    ],
    w=12, h=7, unit="ms", decimals=1,
))
panels.append(timeseries(
    title="Block Fetch Latency (p50 / p95 / p99)",
    targets=[
        target("histogram_quantile(0.5, sum(rate(dugite_peer_block_fetch_ms_bucket[5m])) by (le))",
               legend="p50"),
        target("histogram_quantile(0.95, sum(rate(dugite_peer_block_fetch_ms_bucket[5m])) by (le))",
               legend="p95"),
        target("histogram_quantile(0.99, sum(rate(dugite_peer_block_fetch_ms_bucket[5m])) by (le))",
               legend="p99"),
    ],
    w=12, h=7, unit="ms", decimals=1,
))


# --- Row: Connections ----------------------------------------------------
panels.append(row("Connection Manager"))

panels.append(stat(
    title="N2N Active",
    expr="dugite_n2n_connections_active",
    w=4, h=4, thresholds=PEER_OK,
))
panels.append(stat(
    title="N2C Active",
    expr="dugite_n2c_connections_active",
    w=4, h=4,
))
panels.append(stat(
    title="Duplex",
    expr="dugite_conn_duplex",
    w=4, h=4, color_fixed="green", color_scheme="fixed",
))
panels.append(stat(
    title="Unidirectional",
    expr="dugite_conn_unidirectional",
    w=4, h=4,
))
panels.append(stat(
    title="Full Duplex",
    expr="dugite_conn_full_duplex",
    w=4, h=4, color_fixed="green", color_scheme="fixed",
))
panels.append(stat(
    title="Terminating",
    expr="dugite_conn_terminating",
    w=4, h=4, color_fixed="orange", color_scheme="fixed",
))

panels.append(timeseries(
    title="Connection Counters (ConnectionManager)",
    targets=[
        target("dugite_conn_full_duplex", legend="full duplex"),
        target("dugite_conn_duplex", legend="duplex"),
        target("dugite_conn_unidirectional", legend="unidirectional"),
        target("dugite_conn_inbound", legend="inbound"),
        target("dugite_conn_outbound", legend="outbound"),
        target("dugite_conn_terminating", legend="terminating"),
    ],
    w=12, h=7, unit="short",
    desc="Mirrors Haskell ConnectionManagerCounters — useful for diagnosing "
         "connection lifecycle bugs (e.g. peers stuck in terminating).",
))
panels.append(timeseries(
    title="N2N / N2C Active Connections",
    targets=[
        target("dugite_n2n_connections_active", legend="N2N active"),
        target("dugite_n2c_connections_active", legend="N2C active"),
    ],
    w=12, h=7, unit="short",
))


# --- Row: Mempool & Tx Submission ----------------------------------------
panels.append(row("Mempool & Transaction Submission"))

panels.append(stat(
    title="Mempool Txs",
    expr="dugite_mempool_tx_count",
    w=4, h=4,
    desc="Number of transactions currently sitting in the mempool.",
))
panels.append(stat(
    title="Mempool Bytes",
    expr="dugite_mempool_bytes",
    w=4, h=4, unit="bytes",
))
panels.append(stat(
    title="Mempool Capacity",
    expr="dugite_mempool_tx_max",
    w=4, h=4,
    desc="Maximum number of txs the mempool is willing to hold.",
))
panels.append(stat(
    title="N2C Submitted",
    expr="dugite_n2c_txs_submitted_total",
    w=4, h=4,
))
panels.append(stat(
    title="N2C Accepted",
    expr="dugite_n2c_txs_accepted_total",
    w=4, h=4, color_fixed="green", color_scheme="fixed",
))
panels.append(stat(
    title="N2C Rejected",
    expr="dugite_n2c_txs_rejected_total",
    w=4, h=4, color_fixed="red", color_scheme="fixed",
))

panels.append(timeseries(
    title="Mempool Occupancy",
    targets=[
        target("dugite_mempool_tx_count", legend="txs"),
        target("dugite_mempool_tx_max", legend="capacity"),
    ],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="Mempool Size (bytes)",
    targets=[target("dugite_mempool_bytes", legend="bytes")],
    w=12, h=6, unit="bytes",
))

panels.append(timeseries(
    title="Transaction Rate (5m)",
    targets=[
        target("rate(dugite_transactions_received_total[5m]) * 60",
               legend="received /min"),
        target("rate(dugite_transactions_validated_total[5m]) * 60",
               legend="validated /min"),
        target("rate(dugite_transactions_rejected_total[5m]) * 60",
               legend="rejected /min"),
    ],
    w=12, h=6, unit="short", decimals=1,
))
panels.append(timeseries(
    title="N2C Submission Outcome",
    targets=[
        target("rate(dugite_n2c_txs_submitted_total[5m]) * 60",
               legend="submitted /min"),
        target("rate(dugite_n2c_txs_accepted_total[5m]) * 60",
               legend="accepted /min"),
        target("rate(dugite_n2c_txs_rejected_total[5m]) * 60",
               legend="rejected /min"),
    ],
    w=12, h=6, unit="short", decimals=1,
))


# --- Row: Validation / Protocol errors -----------------------------------
panels.append(row("Validation & Protocol Errors"))

panels.append(timeseries(
    title="Validation Errors by Type",
    targets=[
        target("rate(dugite_validation_errors_total[5m]) * 60",
               legend="{{error}}"),
    ],
    w=12, h=7, unit="short", decimals=1,
    desc="Phase-1 / Phase-2 / mempool validation rejections, broken down by "
         "predicate-failure tag.",
))
panels.append(timeseries(
    title="Protocol Errors by Type",
    targets=[
        target("rate(dugite_protocol_errors_total[5m]) * 60",
               legend="{{error}}"),
    ],
    w=12, h=7, unit="short", decimals=1,
    desc="Mini-protocol level errors (handshake, chainsync, blockfetch, "
         "txsubmission, keepalive).",
))
panels.append(barchart(
    title="Validation Error Totals (lifetime)",
    targets=[
        target("dugite_validation_errors_total",
               legend="{{error}}", instant=True),
    ],
    w=12, h=7, unit="short",
))
panels.append(barchart(
    title="Protocol Error Totals (lifetime)",
    targets=[
        target("dugite_protocol_errors_total",
               legend="{{error}}", instant=True),
    ],
    w=12, h=7, unit="short",
))


# --- Row: Block Production ----------------------------------------------
panels.append(row("Block Production"))

panels.append(stat(
    title="Blocks Forged",
    expr="dugite_blocks_forged_total",
    w=4, h=4, color_fixed="green", color_scheme="fixed",
))
panels.append(stat(
    title="Leader Checks",
    expr="dugite_leader_checks_total",
    w=4, h=4,
))
panels.append(stat(
    title="Not Elected",
    expr="dugite_leader_checks_not_elected_total",
    w=4, h=4,
))
panels.append(stat(
    title="Forge Failures",
    expr="dugite_forge_failures_total",
    w=4, h=4, color_fixed="red", color_scheme="fixed",
))
panels.append(stat(
    title="Slot Battles Lost",
    expr="dugite_forge_slot_battles_total",
    w=4, h=4, color_fixed="orange", color_scheme="fixed",
    desc="Times we forged but lost the slot battle (someone else's block won).",
))
panels.append(stat(
    title="No Subscribers",
    expr="dugite_forge_announce_no_subscribers_total",
    w=4, h=4, color_fixed="orange", color_scheme="fixed",
    desc="Forged block had no downstream peers to announce to.",
))

panels.append(timeseries(
    title="Leader Checks Rate",
    targets=[
        target("rate(dugite_leader_checks_total[5m]) * 60",
               legend="checks /min"),
        target("rate(dugite_leader_checks_not_elected_total[5m]) * 60",
               legend="not elected /min"),
    ],
    w=12, h=6, unit="short", decimals=1,
))
panels.append(timeseries(
    title="Forge Outcomes (cumulative)",
    targets=[
        target("dugite_blocks_forged_total", legend="forged"),
        target("dugite_blocks_announced_total", legend="announced"),
        target("dugite_forge_failures_total", legend="failures"),
        target("dugite_forge_race_lost_total", legend="race lost"),
        target("dugite_forge_slot_battles_total", legend="slot battles lost"),
        target("dugite_forge_announce_no_subscribers_total",
               legend="no subscribers"),
    ],
    w=12, h=6, unit="short",
))


# --- Row: Ledger State ---------------------------------------------------
panels.append(row("Ledger State"))

panels.append(stat(
    title="UTxO Set",
    expr="dugite_utxo_count",
    w=4, h=4,
))
panels.append(stat(
    title="Stake Pools",
    expr="dugite_pool_count",
    w=4, h=4,
))
panels.append(stat(
    title="Delegations",
    expr="dugite_delegation_count",
    w=4, h=4,
))
panels.append(stat(
    title="Treasury (ADA)",
    expr="dugite_treasury_lovelace / 1000000",
    w=4, h=4, unit="locale", decimals=0, color_fixed="green",
    color_scheme="fixed",
))
panels.append(stat(
    title="Reserves (ADA)",
    expr="dugite_reserves_lovelace / 1000000",
    w=4, h=4, unit="locale", decimals=0, color_fixed="blue",
    color_scheme="fixed",
))
panels.append(stat(
    title="Vote Delegations",
    expr="dugite_vote_delegation_count",
    w=4, h=4,
    desc="Stake credentials with an active DRep delegation.",
))

panels.append(timeseries(
    title="UTxO Set Size",
    targets=[target("dugite_utxo_count", legend="UTxOs")],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="Stake Pools & Delegations",
    targets=[
        target("dugite_pool_count", legend="pools"),
        target("dugite_delegation_count", legend="delegations"),
    ],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="Treasury & Reserves (ADA)",
    targets=[
        target("dugite_treasury_lovelace / 1000000", legend="treasury"),
        target("dugite_reserves_lovelace / 1000000", legend="reserves"),
    ],
    w=24, h=6, unit="locale", decimals=0,
    desc="Lovelace converted to ADA. Treasury grows from epoch fees + tau slice; "
         "reserves shrink each epoch via the rho schedule.",
))


# --- Row: Governance -----------------------------------------------------
panels.append(row("Governance (CIP-1694)"))

panels.append(stat(
    title="DReps Registered",
    expr="dugite_drep_count",
    w=4, h=4,
))
panels.append(stat(
    title="DReps Active",
    expr="dugite_drep_active",
    w=4, h=4, color_fixed="green", color_scheme="fixed",
))
panels.append(stat(
    title="Active Proposals",
    expr="dugite_proposal_count",
    w=4, h=4,
))
panels.append(stat(
    title="Dormant Epochs",
    expr="dugite_gov_dormant_epochs",
    w=4, h=4,
    desc="Consecutive epochs without any active proposal — pauses certain "
         "ratification timers.",
))
panels.append(stat(
    title="Constitution",
    expr="dugite_constitution_present",
    w=4, h=4, mappings=ON_OFF_MAP, color_mode="background",
))
panels.append(stat(
    title="No-Confidence",
    expr="dugite_committee_no_confidence",
    w=4, h=4, mappings=ON_OFF_MAP, color_mode="background",
    desc="1 if the constitutional committee is currently in a state of "
         "no-confidence.",
))

panels.append(stat(
    title="CC Members",
    expr="dugite_committee_total_count",
    w=4, h=3,
))
panels.append(stat(
    title="CC Hot Keys",
    expr="dugite_committee_hot_count",
    w=4, h=3,
))
panels.append(stat(
    title="CC Resigned",
    expr="dugite_committee_resigned_count",
    w=4, h=3,
))
panels.append(stat(
    title="CC Threshold (bps)",
    expr="dugite_committee_threshold_bps",
    w=4, h=3,
    desc="Committee approval threshold in basis points (5000 = 50%).",
))
panels.append(stat(
    title="DRep Registrations",
    expr="dugite_drep_registrations_total",
    w=4, h=3,
))
panels.append(stat(
    title="Vote Delegations",
    expr="dugite_vote_delegation_count",
    w=4, h=3,
))

panels.append(timeseries(
    title="DRep Population",
    targets=[
        target("dugite_drep_count", legend="registered"),
        target("dugite_drep_active", legend="active"),
    ],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="Governance Proposals & Dormant Epochs",
    targets=[
        target("dugite_proposal_count", legend="proposals"),
        target("dugite_gov_dormant_epochs", legend="dormant epochs"),
    ],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="Constitutional Committee",
    targets=[
        target("dugite_committee_total_count", legend="total"),
        target("dugite_committee_hot_count", legend="hot keys"),
        target("dugite_committee_resigned_count", legend="resigned"),
    ],
    w=12, h=6, unit="short",
))
panels.append(timeseries(
    title="DRep Registrations Rate",
    targets=[
        target("rate(dugite_drep_registrations_total[1h]) * 3600",
               legend="registrations /h"),
    ],
    w=12, h=6, unit="short", decimals=1,
))


# --- Row: Protocol Parameters --------------------------------------------
panels.append(row("Protocol Parameters"))

panels.append(stat(
    title="DRep Deposit (ADA)",
    expr="dugite_pparam_drep_deposit_lovelace / 1000000",
    w=4, h=3, unit="locale", decimals=0,
))
panels.append(stat(
    title="DRep Activity (epochs)",
    expr="dugite_pparam_drep_activity_epochs",
    w=4, h=3,
))
panels.append(stat(
    title="Gov Action Deposit (ADA)",
    expr="dugite_pparam_gov_action_deposit_lovelace / 1000000",
    w=4, h=3, unit="locale", decimals=0,
))
panels.append(stat(
    title="Gov Action Lifetime (ep)",
    expr="dugite_pparam_gov_action_lifetime_epochs",
    w=4, h=3,
))
panels.append(stat(
    title="CC Min Size",
    expr="dugite_pparam_committee_min_size",
    w=4, h=3,
))
panels.append(stat(
    title="CC Max Term (ep)",
    expr="dugite_pparam_committee_max_term_length",
    w=4, h=3,
))


# --- Row: System ---------------------------------------------------------
panels.append(row("System"))

panels.append(gauge(
    title="CPU %",
    expr="dugite_cpu_percent",
    w=6, h=5, unit="percent", min_v=0, max_v=100,
    thresholds=CPU, decimals=1,
))
panels.append(stat(
    title="RSS",
    expr="dugite_mem_resident_bytes",
    w=3, h=5, unit="bytes",
    desc="Process resident-set size.",
))
panels.append(stat(
    title="Peak RSS",
    expr="dugite_mem_peak_bytes",
    w=3, h=5, unit="bytes",
    desc="High-water mark of resident-set size since process start.",
))
panels.append(stat(
    title="Disk Available",
    expr="dugite_disk_available_bytes",
    w=3, h=5, unit="bytes",
    thresholds=DISK_AVAILABLE,
))
panels.append(stat(
    title="Disk Used",
    expr="dugite_disk_used_bytes",
    w=3, h=5, unit="bytes",
))
panels.append(gauge(
    title="Disk Used %",
    expr="(dugite_disk_used_bytes / dugite_disk_total_bytes) * 100",
    w=3, h=5, unit="percent", min_v=0, max_v=100, decimals=1,
    thresholds=[
        {"color": "green", "value": None},
        {"color": "yellow", "value": 75},
        {"color": "orange", "value": 90},
        {"color": "red", "value": 95},
    ],
))
panels.append(stat(
    title="Uptime",
    expr="dugite_uptime_seconds",
    w=3, h=5, unit="s",
))

panels.append(timeseries(
    title="Memory (RSS / Peak / Host Total)",
    targets=[
        target("dugite_mem_resident_bytes", legend="RSS"),
        target("dugite_mem_peak_bytes", legend="peak RSS"),
        target("dugite_mem_total_bytes", legend="host total"),
    ],
    w=12, h=6, unit="bytes",
    desc="Process RSS vs all-time peak vs total system memory.",
))

panels.append(timeseries(
    title="CPU Utilisation",
    targets=[
        target("dugite_cpu_percent", legend="cpu %"),
        target("rate(dugite_cpu_seconds_total[1m]) * 100",
               legend="cpu rate (smoothed)"),
    ],
    w=12, h=6, unit="percent", decimals=1,
))
panels.append(timeseries(
    title="Disk Usage Over Time",
    targets=[
        target("dugite_disk_used_bytes", legend="used"),
        target("dugite_disk_available_bytes", legend="available"),
    ],
    w=12, h=6, unit="bytes",
))
panels.append(timeseries(
    title="Config Reloads",
    targets=[
        target("rate(dugite_config_reload_total[5m]) * 60",
               legend="{{result}} /min"),
    ],
    w=12, h=6, unit="short", decimals=1,
    desc="SIGHUP / file-watch driven config reloads broken down by outcome.",
))


# --- Dashboard wrapper ---------------------------------------------------
dashboard = {
    "__inputs": [
        {
            "name": "DS_PROMETHEUS",
            "label": "Prometheus",
            "description": "Prometheus data source for Dugite metrics",
            "type": "datasource",
            "pluginId": "prometheus",
            "pluginName": "Prometheus",
        }
    ],
    "__requires": [
        {"type": "grafana", "id": "grafana", "name": "Grafana", "version": "10.0.0"},
        {"type": "datasource", "id": "prometheus", "name": "Prometheus", "version": "1.0.0"},
        {"type": "panel", "id": "stat", "name": "Stat", "version": ""},
        {"type": "panel", "id": "gauge", "name": "Gauge", "version": ""},
        {"type": "panel", "id": "timeseries", "name": "Time series", "version": ""},
        {"type": "panel", "id": "bargauge", "name": "Bar gauge", "version": ""},
        {"type": "panel", "id": "barchart", "name": "Bar chart", "version": ""},
        {"type": "panel", "id": "piechart", "name": "Pie chart", "version": ""},
    ],
    "annotations": {
        "list": [
            {
                "builtIn": 1,
                "datasource": {"type": "grafana", "uid": "-- Grafana --"},
                "enable": True,
                "hide": True,
                "iconColor": "rgba(0, 211, 255, 1)",
                "name": "Annotations & Alerts",
                "type": "dashboard",
            }
        ]
    },
    "description": (
        "Comprehensive monitoring dashboard for the Dugite Cardano node. "
        "Covers identity, sync, peers, latency, connection lifecycle, mempool, "
        "validation, block production, ledger state, CIP-1694 governance, "
        "protocol parameters, and system health."
    ),
    "editable": True,
    "fiscalYearStartMonth": 0,
    "graphTooltip": 1,
    "id": None,
    "links": [],
    "liveNow": False,
    "panels": panels,
    "refresh": "10s",
    "schemaVersion": 38,
    "style": "dark",
    "tags": ["dugite", "cardano"],
    "templating": {"list": []},
    "time": {"from": "now-1h", "to": "now"},
    "timepicker": {
        "refresh_intervals": ["5s", "10s", "30s", "1m", "5m", "15m", "30m", "1h", "2h", "1d"],
    },
    "timezone": "browser",
    "title": "Dugite Node",
    "uid": "dugite-node",
    "version": 2,
    "weekStart": "",
}


def main() -> None:
    json.dump(dashboard, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
