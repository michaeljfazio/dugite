#!/usr/bin/env bash
#
# Start a local Prometheus + Grafana monitoring stack for Dugite development.
#
# Usage:
#   ./scripts/monitoring/start.sh          # Start monitoring
#   ./scripts/monitoring/start.sh stop     # Stop monitoring
#   ./scripts/monitoring/start.sh status   # Show container status
#
# Prerequisites: Docker (or Podman with docker CLI compatibility)
#
# Grafana:    http://localhost:3000  (admin/admin)
# Prometheus: http://localhost:9090
#
# The Dugite node must be running with metrics on port 12798 (default).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONFIG_DIR="$PROJECT_DIR/config/monitoring"
DATA_DIR="$PROJECT_DIR/.monitoring-data"

PROMETHEUS_PORT="${PROMETHEUS_PORT:-9090}"
GRAFANA_PORT="${GRAFANA_PORT:-3000}"
DUGITE_METRICS_PORT="${DUGITE_METRICS_PORT:-12798}"

PROMETHEUS_CONTAINER="dugite-prometheus"
GRAFANA_CONTAINER="dugite-grafana"

stop_monitoring() {
    echo "Stopping monitoring containers..."
    docker rm -f "$PROMETHEUS_CONTAINER" "$GRAFANA_CONTAINER" 2>/dev/null || true
    echo "Monitoring stopped."
}

show_status() {
    echo "=== Monitoring Status ==="
    for name in "$PROMETHEUS_CONTAINER" "$GRAFANA_CONTAINER"; do
        status=$(docker inspect --format '{{.State.Status}}' "$name" 2>/dev/null || echo "not running")
        echo "  $name: $status"
    done
}

start_monitoring() {
    # Stop any existing containers
    docker rm -f "$PROMETHEUS_CONTAINER" "$GRAFANA_CONTAINER" 2>/dev/null || true

    # Create data directories
    mkdir -p "$DATA_DIR/prometheus" "$DATA_DIR/grafana"

    echo "Starting Prometheus on port $PROMETHEUS_PORT..."
    docker run -d \
        --name "$PROMETHEUS_CONTAINER" \
        --add-host=host.docker.internal:host-gateway \
        -p "${PROMETHEUS_PORT}:9090" \
        -v "$CONFIG_DIR/prometheus.yml:/etc/prometheus/prometheus.yml:ro" \
        -v "$DATA_DIR/prometheus:/prometheus" \
        prom/prometheus:latest \
        --config.file=/etc/prometheus/prometheus.yml \
        --storage.tsdb.retention.time=30d \
        --web.enable-lifecycle

    echo "Starting Grafana on port $GRAFANA_PORT..."
    docker run -d \
        --name "$GRAFANA_CONTAINER" \
        --add-host=host.docker.internal:host-gateway \
        -p "${GRAFANA_PORT}:3000" \
        -v "$DATA_DIR/grafana:/var/lib/grafana" \
        -e GF_SECURITY_ADMIN_USER=admin \
        -e GF_SECURITY_ADMIN_PASSWORD=admin \
        -e GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH=/var/lib/grafana/dashboards/dugite.json \
        grafana/grafana:latest

    # Wait for Grafana to be ready
    echo -n "Waiting for Grafana..."
    for i in $(seq 1 30); do
        if curl -s "http://localhost:${GRAFANA_PORT}/api/health" | grep -q '"database": "ok"' 2>/dev/null; then
            echo " ready."
            break
        fi
        echo -n "."
        sleep 1
    done

    # Configure Prometheus data source via Grafana API.
    #
    # We point Grafana at host.docker.internal:${PROMETHEUS_PORT} rather than
    # the container name because podman's default network does NOT auto-resolve
    # container names by --name (only by short hex ID). Hitting the published
    # host port works on both docker-on-mac and podman.
    echo "Configuring Prometheus data source..."
    DS_URL="http://host.docker.internal:${PROMETHEUS_PORT}"
    DS_BODY='{
        "name": "Prometheus",
        "type": "prometheus",
        "url": "'"$DS_URL"'",
        "access": "proxy",
        "isDefault": true
    }'
    # Create or overwrite (PUT to /api/datasources/name/Prometheus updates
    # if it exists; POST creates first time).
    if curl -sf "http://admin:admin@localhost:${GRAFANA_PORT}/api/datasources/name/Prometheus" > /dev/null 2>&1; then
        DS_ID=$(curl -s "http://admin:admin@localhost:${GRAFANA_PORT}/api/datasources/name/Prometheus" \
            | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
        curl -s -X PUT "http://admin:admin@localhost:${GRAFANA_PORT}/api/datasources/${DS_ID}" \
            -H "Content-Type: application/json" -d "$DS_BODY" > /dev/null 2>&1
    else
        curl -s -X POST "http://admin:admin@localhost:${GRAFANA_PORT}/api/datasources" \
            -H "Content-Type: application/json" -d "$DS_BODY" > /dev/null 2>&1
    fi || true

    # Get the datasource UID
    DS_UID=$(curl -s "http://admin:admin@localhost:${GRAFANA_PORT}/api/datasources/name/Prometheus" | python3 -c "import sys,json; print(json.load(sys.stdin).get('uid',''))" 2>/dev/null || echo "")

    if [ -n "$DS_UID" ]; then
        # Import dashboard with the correct datasource UID substituted
        echo "Importing Dugite dashboard..."
        python3 -c "
import json, sys
with open('$CONFIG_DIR/grafana-dashboard.json') as f:
    dash = json.load(f)
# Replace datasource placeholder with actual UID
raw = json.dumps(dash)
raw = raw.replace('\${DS_PROMETHEUS}', '$DS_UID')
dash = json.loads(raw)
# Remove __inputs/__requires (only needed for import UI)
dash.pop('__inputs', None)
dash.pop('__requires', None)
payload = {'dashboard': dash, 'overwrite': True}
print(json.dumps(payload))
" | curl -s -X POST "http://admin:admin@localhost:${GRAFANA_PORT}/api/dashboards/db" \
            -H "Content-Type: application/json" \
            -d @- > /dev/null 2>&1
        echo "Dashboard imported."
    else
        echo "Warning: Could not determine datasource UID. Import the dashboard manually from Grafana UI."
    fi

    echo ""
    echo "=== Monitoring Stack Ready ==="
    echo "  Grafana:    http://localhost:${GRAFANA_PORT}  (admin/admin)"
    echo "  Prometheus: http://localhost:${PROMETHEUS_PORT}"
    echo "  Dashboard:  http://localhost:${GRAFANA_PORT}/d/dugite-node/dugite-node"
    echo ""
    echo "Prometheus scrapes all standard dugite metrics ports:"
    echo "  12796=preview/relay  12797=preview/bp  12798=default"
    echo "  12799=preprod        12800=mainnet"
    echo "Unused ports show DOWN at http://localhost:${PROMETHEUS_PORT}/targets — that's fine."
    echo "To stop: $0 stop"
}

case "${1:-start}" in
    start)   start_monitoring ;;
    stop)    stop_monitoring ;;
    status)  show_status ;;
    *)
        echo "Usage: $0 {start|stop|status}"
        exit 1
        ;;
esac
