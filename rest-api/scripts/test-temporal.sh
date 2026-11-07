#!/bin/bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -e

NAMESPACE="${NAMESPACE:-temporal}"
NICO_REST_NAMESPACE="${NICO_REST_NAMESPACE:-nico-rest}"
TIMEOUT="${TIMEOUT:-600}"

usage() {
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  mtls      Verify Temporal mTLS configuration"
    echo "  rotation  Test certificate rotation"
    echo "  all       Run both tests"
    exit 1
}

# ============================================================================
# Common Functions
# ============================================================================

PASSED=0
FAILED=0

run_test() {
    local name=$1
    local cmd=$2

    echo -n "  $name... "
    if eval "$cmd" > /dev/null 2>&1; then
        echo "PASS"
        ((PASSED++))
        return 0
    else
        echo "FAIL"
        ((FAILED++))
        return 1
    fi
}

get_cert_serial() {
    local secret_name=$1
    local ns="${2:-$NAMESPACE}"
    kubectl -n "$ns" get secret "$secret_name" -o jsonpath='{.data.tls\.crt}' 2>/dev/null | \
        base64 -d | openssl x509 -noout -serial 2>/dev/null | cut -d= -f2
}

check_temporal_health() {
    kubectl -n "$NAMESPACE" get pods -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=frontend \
        -o jsonpath='{.items[0].status.phase}' 2>/dev/null | grep -q "Running"
}

# ============================================================================
# mTLS Verification
# ============================================================================

test_mtls() {
    echo "=========================================="
    echo "Temporal mTLS Verification Test"
    echo "=========================================="
    echo ""

    echo "Step 1: Checking Temporal pods..."
    run_test "Frontend pod running" \
        "kubectl -n $NAMESPACE get pods -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=frontend -o jsonpath='{.items[0].status.phase}' | grep -q Running"
    run_test "History pod running" \
        "kubectl -n $NAMESPACE get pods -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=history -o jsonpath='{.items[0].status.phase}' | grep -q Running"
    run_test "Matching pod running" \
        "kubectl -n $NAMESPACE get pods -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=matching -o jsonpath='{.items[0].status.phase}' | grep -q Running"
    run_test "Worker pod running" \
        "kubectl -n $NAMESPACE get pods -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=worker -o jsonpath='{.items[0].status.phase}' | grep -q Running"
    echo ""

    echo "Step 2: Checking TLS certificates..."
    run_test "server-interservice-certs exists" \
        "kubectl -n $NAMESPACE get secret server-interservice-certs"
    run_test "server-cloud-certs exists" \
        "kubectl -n $NAMESPACE get secret server-cloud-certs"
    run_test "server-site-certs exists" \
        "kubectl -n $NAMESPACE get secret server-site-certs"
    run_test "temporal-client-cloud-certs exists" \
        "kubectl -n $NICO_REST_NAMESPACE get secret temporal-client-cloud-certs"
    echo ""

    echo "Step 3: Checking cert-manager Certificate status..."
    run_test "server-interservice-cert Ready" \
        "kubectl -n $NAMESPACE get certificate server-interservice-cert -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -q True"
    run_test "server-cloud-cert Ready" \
        "kubectl -n $NAMESPACE get certificate server-cloud-cert -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -q True"
    run_test "server-site-cert Ready" \
        "kubectl -n $NAMESPACE get certificate server-site-cert -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -q True"
    run_test "temporal-client-cert Ready" \
        "kubectl -n $NICO_REST_NAMESPACE get certificate temporal-client-cert -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}' | grep -q True"
    echo ""

    echo "Step 4: Checking services..."
    run_test "temporal-frontend service exists" \
        "kubectl -n $NAMESPACE get service temporal-frontend"
    run_test "temporal-history service exists" \
        "kubectl -n $NAMESPACE get service temporal-history-headless"
    run_test "temporal-matching service exists" \
        "kubectl -n $NAMESPACE get service temporal-matching-headless"
    echo ""

    echo "=========================================="
    echo "mTLS Test Results: Passed=$PASSED Failed=$FAILED"
    echo "=========================================="

    [ $FAILED -eq 0 ]
}

# ============================================================================
# Certificate Rotation Test
# ============================================================================

test_rotation() {
    echo "=========================================="
    echo "Temporal Certificate Rotation Test"
    echo "=========================================="
    echo ""

    echo "Step 1: Recording current certificate serial numbers..."
    INTERSERVICE_SERIAL_BEFORE=$(get_cert_serial "server-interservice-certs")
    CLOUD_SERIAL_BEFORE=$(get_cert_serial "server-cloud-certs")
    SITE_SERIAL_BEFORE=$(get_cert_serial "server-site-certs")
    CLIENT_SERIAL_BEFORE=$(get_cert_serial "temporal-client-cloud-certs" "$NICO_REST_NAMESPACE")

    echo "  server-interservice-certs: $INTERSERVICE_SERIAL_BEFORE"
    echo "  server-cloud-certs:        $CLOUD_SERIAL_BEFORE"
    echo "  server-site-certs:         $SITE_SERIAL_BEFORE"
    echo "  temporal-client-cloud-certs:     $CLIENT_SERIAL_BEFORE"
    echo ""

    echo "Step 2: Verifying Temporal is healthy before rotation..."
    if check_temporal_health; then
        echo "  Temporal frontend is healthy"
    else
        echo "  ERROR: Temporal frontend is not healthy"
        exit 1
    fi
    echo ""

    echo "Step 3: Deleting certificate secrets to trigger rotation..."
    kubectl -n "$NAMESPACE" delete secret server-interservice-certs --ignore-not-found
    kubectl -n "$NAMESPACE" delete secret server-cloud-certs --ignore-not-found
    kubectl -n "$NAMESPACE" delete secret server-site-certs --ignore-not-found
    kubectl -n "$NICO_REST_NAMESPACE" delete secret temporal-client-cloud-certs --ignore-not-found
    echo ""

    echo "Step 4: Waiting for cert-manager to reissue certificates..."
    kubectl -n "$NAMESPACE" wait --for=condition=Ready certificate/server-interservice-cert --timeout="${TIMEOUT}s"
    kubectl -n "$NAMESPACE" wait --for=condition=Ready certificate/server-cloud-cert --timeout="${TIMEOUT}s"
    kubectl -n "$NAMESPACE" wait --for=condition=Ready certificate/server-site-cert --timeout="${TIMEOUT}s"
    kubectl -n "$NICO_REST_NAMESPACE" wait --for=condition=Ready certificate/temporal-client-cloud-cert --timeout="${TIMEOUT}s"
    echo ""

    echo "Step 5: Verifying new certificate serial numbers..."
    INTERSERVICE_SERIAL_AFTER=$(get_cert_serial "server-interservice-certs")
    CLOUD_SERIAL_AFTER=$(get_cert_serial "server-cloud-certs")
    SITE_SERIAL_AFTER=$(get_cert_serial "server-site-certs")
    CLIENT_SERIAL_AFTER=$(get_cert_serial "temporal-client-cloud-certs" "$NICO_REST_NAMESPACE")

    ROTATION_SUCCESS=true
    [ "$INTERSERVICE_SERIAL_BEFORE" != "$INTERSERVICE_SERIAL_AFTER" ] || ROTATION_SUCCESS=false
    [ "$CLOUD_SERIAL_BEFORE" != "$CLOUD_SERIAL_AFTER" ] || ROTATION_SUCCESS=false
    [ "$SITE_SERIAL_BEFORE" != "$SITE_SERIAL_AFTER" ] || ROTATION_SUCCESS=false
    [ "$CLIENT_SERIAL_BEFORE" != "$CLIENT_SERIAL_AFTER" ] || ROTATION_SUCCESS=false
    echo ""

    echo "Step 6: Restarting Temporal pods..."
    kubectl -n "$NAMESPACE" rollout restart deployment/temporal-frontend
    kubectl -n "$NAMESPACE" rollout restart deployment/temporal-history
    kubectl -n "$NAMESPACE" rollout restart deployment/temporal-matching
    kubectl -n "$NAMESPACE" rollout restart deployment/temporal-worker
    echo ""

    echo "Step 7: Waiting for Temporal pods to be ready..."
    kubectl -n "$NAMESPACE" rollout status deployment/temporal-frontend --timeout="${TIMEOUT}s" 2>/dev/null || \
        echo "  Warning: kubectl rollout status timed out (Kind API server issue)"
    kubectl -n "$NAMESPACE" rollout status deployment/temporal-history --timeout="${TIMEOUT}s" 2>/dev/null || true
    kubectl -n "$NAMESPACE" rollout status deployment/temporal-matching --timeout="${TIMEOUT}s" 2>/dev/null || true
    kubectl -n "$NAMESPACE" rollout status deployment/temporal-worker --timeout="${TIMEOUT}s" 2>/dev/null || true
    echo ""

    echo "Step 8: Verifying Temporal is healthy after rotation..."
    if check_temporal_health 2>/dev/null; then
        echo "  Temporal frontend is healthy"
    else
        echo "  WARNING: Temporal frontend not immediately ready, waiting..."
        kubectl -n "$NAMESPACE" wait --for=condition=ready \
            pod -l app.kubernetes.io/name=temporal,app.kubernetes.io/component=frontend \
            --timeout=60s 2>/dev/null || \
            echo "  Note: kubectl wait timed out, but rotation may have succeeded"
        if check_temporal_health 2>/dev/null; then
            echo "  Temporal frontend is healthy"
        else
            echo "  ERROR: Temporal frontend is not healthy after rotation"
            exit 1
        fi
    fi
    echo ""

    echo "=========================================="
    echo "Certificate Rotation Test Results"
    echo "=========================================="
    if [ "$ROTATION_SUCCESS" = true ]; then
        echo "SUCCESS: All certificates were rotated"
        exit 0
    else
        echo "FAILURE: Some certificates were not rotated"
        exit 1
    fi
}

# ============================================================================
# Main
# ============================================================================

case "${1:-}" in
    mtls)
        test_mtls
        ;;
    rotation)
        test_rotation
        ;;
    all)
        test_mtls
        echo ""
        test_rotation
        ;;
    *)
        usage
        ;;
esac
