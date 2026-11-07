#!/bin/bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLOW_DIR="$SCRIPT_DIR/../flow"
NSM_DIR="$SCRIPT_DIR"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.local.yml"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log_info()  { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }
log_step()  { echo -e "${CYAN}[STEP]${NC} $1"; }

build_binaries() {
    log_info "Building Linux binaries for Docker..."

    log_step "Building Flow (linux/amd64)..."
    (cd "$FLOW_DIR" && GOOS=linux GOARCH=amd64 go build -o flow-linux .)
    mkdir -p "$FLOW_DIR/build"
    cp "$FLOW_DIR/flow-linux" "$FLOW_DIR/build/flow"
    log_info "Flow binary ready"

    log_step "Building NSM (linux/amd64)..."
    (cd "$NSM_DIR" && GOOS=linux GOARCH=amd64 go build -o nvswitch-manager-linux .)
    mkdir -p "$NSM_DIR/build"
    cp "$NSM_DIR/nvswitch-manager-linux" "$NSM_DIR/build/nvswitch-manager"
    log_info "NSM binary ready"

    log_info "All binaries built successfully."
}

start_services() {
    # Verify binaries exist
    if [ ! -f "$FLOW_DIR/build/flow" ]; then
        log_error "Flow binary not found. Run: ./local-dev.sh build"
        exit 1
    fi
    if [ ! -f "$NSM_DIR/build/nvswitch-manager" ]; then
        log_error "NSM binary not found. Run: ./local-dev.sh build"
        exit 1
    fi

    # Check if SSH tunnel is running
    if ! nc -z localhost 1080 2>/dev/null; then
        log_warn "No SOCKS proxy detected on localhost:1080"
        log_warn "NSM won't be able to reach NV-Switch trays without the SSH tunnel."
        log_warn "Start it with: ssh -D 1080 -L 50053:7.243.80.81:1079 -N ytl01-admin01"
        echo ""
    else
        log_info "SOCKS proxy detected on localhost:1080"
    fi

    log_info "Starting infrastructure + services..."
    docker compose -f "$COMPOSE_FILE" up --build "$@"
}

stop_services() {
    log_info "Stopping services and removing volumes..."
    docker compose -f "$COMPOSE_FILE" down -v
}

run_migrations() {
    log_info "Running DB migrations..."

    log_step "Running Flow migrations..."
    docker compose -f "$COMPOSE_FILE" exec flow /opt/flow/flow db migrate
    log_info "Flow migrations complete"

    log_step "Running NSM migrations..."
    docker compose -f "$COMPOSE_FILE" exec nsm ./nvswitch-manager migrate
    log_info "NSM migrations complete"
}

start_tunnel() {
    log_info "Starting SSH tunnel to ytl01-admin01..."
    log_info "  SOCKS proxy on localhost:1080  (NSM -> NV-Switch trays)"
    log_info "  Port forward localhost:50053   (Flow -> NICo at 7.243.80.81:1079)"
    log_info "Press Ctrl+C to stop the tunnel."
    echo ""
    ssh -D 1080 -L 50053:7.243.80.81:1079 -N ytl01-admin01
}

tail_logs() {
    docker compose -f "$COMPOSE_FILE" logs -f flow nsm "$@"
}

show_usage() {
    echo "Usage: $0 [command]"
    echo ""
    echo "Commands:"
    echo "  (none)    Build binaries and start everything"
    echo "  build     Build Linux binaries only"
    echo "  up        Start Docker services only (pass extra args, e.g. -d)"
    echo "  down      Stop all services and remove volumes"
    echo "  tunnel    Start SSH tunnel to ytl01-admin01"
    echo "  migrate   Run DB migrations for Flow and NSM"
    echo "  logs      Tail Flow + NSM logs"
    echo ""
    echo "Quick start:"
    echo "  Terminal 1:  ./local-dev.sh tunnel"
    echo "  Terminal 2:  ./local-dev.sh"
    echo "  Terminal 3:  ./local-dev.sh migrate  (once services are up)"
    echo ""
    echo "Services:"
    echo "  Flow:      localhost:50051  (gRPC)"
    echo "  NSM:       localhost:50052  (gRPC)"
    echo "  Postgres:  localhost:5432"
    echo "  Temporal:  localhost:7233"
    echo "  Vault:     localhost:8201"
}

case "${1:-}" in
    build)
        build_binaries
        ;;
    up)
        shift
        start_services "$@"
        ;;
    down)
        stop_services
        ;;
    tunnel)
        start_tunnel
        ;;
    migrate)
        run_migrations
        ;;
    logs)
        shift
        tail_logs "$@"
        ;;
    help|-h|--help)
        show_usage
        ;;
    "")
        build_binaries
        start_services
        ;;
    *)
        log_error "Unknown command: $1"
        show_usage
        exit 1
        ;;
esac
