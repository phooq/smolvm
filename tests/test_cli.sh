#!/bin/bash
#
# CLI tests for smolvm.
#
# Tests basic CLI functionality like --version and --help.
# Does not require VM environment.
#
# Usage:
#   ./tests/test_cli.sh

source "$(dirname "$0")/common.sh"
init_smolvm

echo ""
echo "=========================================="
echo "  smolvm CLI Tests"
echo "=========================================="
echo ""

# =============================================================================
# Version and Help
# =============================================================================

test_version() {
    local output
    output=$($SMOLVM --version 2>&1)
    [[ "$output" == *"smolvm"* ]]
}

test_help() {
    local output
    output=$($SMOLVM --help 2>&1)
    [[ "$output" == *"sandbox"* ]] && \
    [[ "$output" == *"microvm"* ]] && \
    [[ "$output" == *"container"* ]]
}

test_sandbox_help() {
    local output
    output=$($SMOLVM sandbox --help 2>&1)
    [[ "$output" == *"run"* ]]
}

test_sandbox_run_platform_flag() {
    # Verify --platform flag exists in sandbox run help
    local output
    output=$($SMOLVM sandbox run --help 2>&1)
    [[ "$output" == *"--platform"* ]] && \
    [[ "$output" == *"linux/arm64"* ]] && \
    [[ "$output" == *"linux/amd64"* ]]
}

test_pack_platform_flag() {
    # Verify --platform flag exists in pack help
    local output
    output=$($SMOLVM pack --help 2>&1)
    [[ "$output" == *"--platform"* ]] && \
    [[ "$output" == *"linux/arm64"* ]] && \
    [[ "$output" == *"linux/amd64"* ]]
}

test_microvm_help() {
    local output
    output=$($SMOLVM microvm --help 2>&1)
    [[ "$output" == *"start"* ]] && \
    [[ "$output" == *"stop"* ]] && \
    [[ "$output" == *"status"* ]]
}

test_container_help() {
    local output
    output=$($SMOLVM container --help 2>&1)
    [[ "$output" == *"create"* ]] && \
    [[ "$output" == *"start"* ]] && \
    [[ "$output" == *"stop"* ]] && \
    [[ "$output" == *"list"* ]] && \
    [[ "$output" == *"remove"* ]]
}

# =============================================================================
# Invalid Commands
# =============================================================================

test_invalid_subcommand() {
    # Should fail for invalid subcommand
    ! $SMOLVM nonexistent-command 2>/dev/null
}

test_sandbox_run_missing_image() {
    # Should fail when image is not provided
    ! $SMOLVM sandbox run 2>/dev/null
}

# =============================================================================
# Run Tests
# =============================================================================

run_test "Version command" test_version || true
run_test "Help command" test_help || true
run_test "Sandbox help" test_sandbox_help || true
run_test "Sandbox run --platform flag" test_sandbox_run_platform_flag || true
run_test "Pack --platform flag" test_pack_platform_flag || true
run_test "Microvm help" test_microvm_help || true
run_test "Container help" test_container_help || true
run_test "Invalid subcommand fails" test_invalid_subcommand || true
run_test "Sandbox run without image fails" test_sandbox_run_missing_image || true

print_summary "CLI Tests"
