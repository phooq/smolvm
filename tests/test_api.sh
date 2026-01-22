#!/bin/bash
#
# HTTP API integration tests for smolvm.
#
# Tests the `smolvm serve` command and API endpoints.
#
# Usage:
#   ./tests/test_api.sh

source "$(dirname "$0")/common.sh"
init_smolvm

echo ""
echo "=========================================="
echo "  smolvm HTTP API Tests"
echo "=========================================="
echo ""

# API server configuration
API_PORT=18080
API_URL="http://127.0.0.1:$API_PORT"
SERVER_PID=""

# =============================================================================
# Setup / Teardown
# =============================================================================

start_server() {
    log_info "Starting API server on port $API_PORT..."
    $SMOLVM serve --listen "127.0.0.1:$API_PORT" &
    SERVER_PID=$!

    # Wait for server to be ready
    local retries=30
    while [[ $retries -gt 0 ]]; do
        if curl -s "$API_URL/health" >/dev/null 2>&1; then
            log_info "Server started (PID: $SERVER_PID)"
            return 0
        fi
        sleep 0.1
        ((retries--))
    done

    log_fail "Server failed to start"
    return 1
}

stop_server() {
    if [[ -n "$SERVER_PID" ]]; then
        log_info "Stopping API server (PID: $SERVER_PID)..."
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        SERVER_PID=""
    fi
}

cleanup() {
    stop_server
    # Clean up any test sandboxes
    curl -s -X DELETE "$API_URL/api/v1/sandboxes/test-sandbox" >/dev/null 2>&1 || true
    curl -s -X DELETE "$API_URL/api/v1/sandboxes/another-sandbox" >/dev/null 2>&1 || true
}

trap cleanup EXIT

# =============================================================================
# Health Check
# =============================================================================

test_health_endpoint() {
    local response
    response=$(curl -s "$API_URL/health")
    [[ "$response" == *'"status":"ok"'* ]]
}

# =============================================================================
# Sandbox CRUD
# =============================================================================

test_create_sandbox() {
    local response status
    response=$(curl -s -w "\n%{http_code}" -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": "test-sandbox"}')
    status=$(echo "$response" | tail -1)
    [[ "$status" == "200" ]]
}

test_create_duplicate_sandbox() {
    # First create should succeed
    curl -s -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": "another-sandbox"}' >/dev/null

    # Second create should fail with 409 Conflict
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": "another-sandbox"}')
    [[ "$status" == "409" ]]
}

test_create_sandbox_empty_name() {
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": ""}')
    [[ "$status" == "400" ]]
}

test_create_sandbox_invalid_name() {
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": "../etc/passwd"}')
    [[ "$status" == "400" ]]
}

test_list_sandboxes() {
    local response
    response=$(curl -s "$API_URL/api/v1/sandboxes")
    [[ "$response" == *'"sandboxes":'* ]]
}

test_get_sandbox() {
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/api/v1/sandboxes/test-sandbox")
    [[ "$status" == "200" ]]
}

test_get_nonexistent_sandbox() {
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/api/v1/sandboxes/nonexistent-sandbox-12345")
    [[ "$status" == "404" ]]
}

test_delete_sandbox() {
    # Create a sandbox to delete
    curl -s -X POST "$API_URL/api/v1/sandboxes" \
        -H "Content-Type: application/json" \
        -d '{"name": "to-delete"}' >/dev/null

    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$API_URL/api/v1/sandboxes/to-delete")
    [[ "$status" == "200" ]]
}

test_delete_nonexistent_sandbox() {
    local status
    status=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$API_URL/api/v1/sandboxes/nonexistent-12345")
    [[ "$status" == "404" ]]
}

# =============================================================================
# Run Tests
# =============================================================================

# Start server first
if ! start_server; then
    echo -e "${RED}Failed to start server, aborting tests${NC}"
    exit 1
fi

# Health
run_test "Health endpoint returns ok" test_health_endpoint || true

# Sandbox CRUD
run_test "Create sandbox" test_create_sandbox || true
run_test "Create duplicate sandbox returns 409" test_create_duplicate_sandbox || true
run_test "Create sandbox with empty name returns 400" test_create_sandbox_empty_name || true
run_test "Create sandbox with path traversal returns 400" test_create_sandbox_invalid_name || true
run_test "List sandboxes" test_list_sandboxes || true
run_test "Get sandbox" test_get_sandbox || true
run_test "Get nonexistent sandbox returns 404" test_get_nonexistent_sandbox || true
run_test "Delete sandbox" test_delete_sandbox || true
run_test "Delete nonexistent sandbox returns 404" test_delete_nonexistent_sandbox || true

print_summary "HTTP API Tests"
