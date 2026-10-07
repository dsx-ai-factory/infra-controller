// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwaremanager provides firmware update orchestration for NV-Switch trays.
package firmwaremanager

import (
	"context"
	"fmt"
	"net"
	"strconv"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"

	log "github.com/sirupsen/logrus"
)

// isTransientError returns true if the error is likely transient and worth retrying.
// Transient errors include connection timeouts, network unreachable, etc.
// Permanent errors include authentication failures, invalid requests, etc.
func isTransientError(err error) bool {
	if err == nil {
		return false
	}
	errStr := strings.ToLower(err.Error())

	// Connection/network errors are transient
	transientPatterns := []string{
		"timeout",
		"connection refused",
		"connection reset",
		"no route to host",
		"network is unreachable",
		"i/o timeout",
		"dial tcp",
		"context deadline exceeded",
		"temporary failure",
		"eof",
	}

	for _, pattern := range transientPatterns {
		if strings.Contains(errStr, pattern) {
			return true
		}
	}

	return false
}

const (
	// DefaultReachabilityTimeout is the default timeout for waiting for a host to become reachable.
	DefaultReachabilityTimeout = 5 * time.Minute
	// DefaultReachabilityInterval is the default interval between reachability checks.
	DefaultReachabilityInterval = 5 * time.Second
	// PostReachabilityDelay is extra time to wait after a host becomes reachable (for services to start).
	PostReachabilityDelay = 60 * time.Second
)

// ResetTray performs a Redfish ComputerSystem.Reset action on the NV-Switch tray.
func ResetTray(ctx context.Context, tray *nvswitch.NVSwitchTray, resetType redfish.ResetType) error {
	log.Infof("Resetting NV-Switch tray %s via BMC at %s (action=%s)", tray.UUID, tray.BMC.IP, resetType)

	client, err := redfish.New(ctx, tray.BMC, false)
	if err != nil {
		return fmt.Errorf("failed to create Redfish client: %w", err)
	}
	defer client.Logout()

	resp, err := client.ResetSystem(resetType)
	if err != nil {
		return fmt.Errorf("%s request failed: %w", resetType, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 300 {
		return fmt.Errorf("%s returned status %d", resetType, resp.StatusCode)
	}

	log.Infof("%s initiated successfully for tray %s", resetType, tray.UUID)
	return nil
}

// PowerCycleTray performs a power cycle on the NV-Switch tray via Redfish.
func PowerCycleTray(ctx context.Context, tray *nvswitch.NVSwitchTray) error {
	return ResetTray(ctx, tray, redfish.ResetPowerCycle)
}

// WaitForReachable waits for a host to become reachable via TCP port 22 (SSH).
// Returns nil when the host is reachable, or an error on timeout/cancellation.
func WaitForReachable(ctx context.Context, ip net.IP, timeout time.Duration) error {
	if timeout == 0 {
		timeout = DefaultReachabilityTimeout
	}

	log.Infof("Waiting for %s to become reachable (timeout: %v)", ip, timeout)

	deadline := time.Now().Add(timeout)
	interval := DefaultReachabilityInterval

	for time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		// Try TCP connection to SSH port
		conn, err := net.DialTimeout("tcp", fmt.Sprintf("%s:22", ip.String()), 2*time.Second)
		if err == nil {
			conn.Close()
			log.Infof("%s is now reachable", ip)

			// Wait extra time for services to fully start
			log.Infof("Waiting %v for services to start...", PostReachabilityDelay)
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(PostReachabilityDelay):
			}

			return nil
		}

		log.Debugf("%s not yet reachable: %v", ip, err)
		time.Sleep(interval)
	}

	return fmt.Errorf("timeout waiting for %s to become reachable after %v", ip, timeout)
}

// WaitForUnreachable waits for a host to become unreachable (e.g., during reboot).
// Returns nil when the host is no longer responding, or an error on timeout/cancellation.
func WaitForUnreachable(ctx context.Context, ip net.IP, timeout time.Duration) error {
	if timeout == 0 {
		timeout = 2 * time.Minute
	}

	log.Infof("Waiting for %s to go down (timeout: %v)", ip, timeout)

	deadline := time.Now().Add(timeout)
	interval := 2 * time.Second

	for time.Now().Before(deadline) {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		// Try TCP connection
		conn, err := net.DialTimeout("tcp", fmt.Sprintf("%s:22", ip.String()), 2*time.Second)
		if err != nil {
			// Host is unreachable
			log.Infof("%s is now unreachable", ip)
			return nil
		}
		conn.Close()

		log.Debugf("%s still reachable, waiting...", ip)
		time.Sleep(interval)
	}

	return fmt.Errorf("timeout waiting for %s to become unreachable", ip)
}

// WaitForReboot waits for a host to go down and come back up.
// This is a combination of WaitForUnreachable followed by WaitForReachable.
func WaitForReboot(ctx context.Context, ip net.IP, downTimeout, upTimeout time.Duration) error {
	// Wait for host to go down
	if err := WaitForUnreachable(ctx, ip, downTimeout); err != nil {
		return fmt.Errorf("host did not go down: %w", err)
	}

	// Wait for host to come back up
	if err := WaitForReachable(ctx, ip, upTimeout); err != nil {
		return fmt.Errorf("host did not come back up: %w", err)
	}

	return nil
}

// IsReachable performs a single non-blocking check if a host is reachable via TCP on the specified port.
// Returns true if reachable, false otherwise. This is used for async polling.
func IsReachable(ip string, port int) bool {
	conn, err := net.DialTimeout("tcp", net.JoinHostPort(ip, strconv.Itoa(port)), 2*time.Second)
	if err != nil {
		return false
	}
	conn.Close()
	return true
}
