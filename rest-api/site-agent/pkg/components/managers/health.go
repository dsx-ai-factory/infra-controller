// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"time"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

const (
	// healthCheckInterval is how often the Site Agent checks Temporal and Core gRPC.
	// The readiness probe and the health metrics report the latest results, so the
	// kubelet's probe rate adds no calls.
	healthCheckInterval = 30 * time.Second
	// healthCheckTimeout bounds each dependency check.
	healthCheckTimeout = 5 * time.Second
)

// StartHealthChecker checks Temporal and Core gRPC every healthCheckInterval. Each
// check records its result in its manager's state.
func StartHealthChecker() {
	ticker := time.NewTicker(healthCheckInterval)
	defer ticker.Stop()
	for {
		checkHealth()
		<-ticker.C
	}
}

func checkHealth() {
	checks := []func(context.Context){
		ManagerAccess.API.Orchestrator.CheckConnection,
		ManagerAccess.API.CoreGrpc.CheckConnection,
	}
	for _, check := range checks {
		ctx, cancel := context.WithTimeout(context.Background(), healthCheckTimeout)
		check(ctx)
		cancel()
	}
	computils.UpdateState(ManagerAccess.Data.EB)
}

// handleLivenessRequest fails once the Temporal worker is gone for good, so
// Kubernetes restarts the Site Agent.
func handleLivenessRequest(w http.ResponseWriter, r *http.Request) {
	err := ManagerAccess.API.Orchestrator.CheckLiveness()
	if err != nil {
		http.Error(w, "Temporal worker is not running: "+err.Error(), http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}

// handleReadinessRequest reports the latest Temporal and Core gRPC state, the same
// state behind the health metrics, and lists every dependency that is not healthy.
func handleReadinessRequest(w http.ResponseWriter, r *http.Request) {
	managers := ManagerAccess.Data.EB.Managers
	var failures []string
	if computils.CompStatus(managers.Workflow.State.HealthStatus.Load()) != computils.CompHealthy {
		failures = append(failures, "Temporal: "+failureReason(managers.Workflow.State.Err()))
	}
	if computils.CompStatus(managers.CoreGrpc.State.HealthStatus.Load()) != computils.CompHealthy {
		failures = append(failures, "Core gRPC: "+failureReason(managers.CoreGrpc.State.Err()))
	}
	if len(failures) > 0 {
		http.Error(w, strings.Join(failures, "\n"), http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}

// failureReason returns the last recorded error, which stays empty until an attempt
// or a check fails.
func failureReason(err string) string {
	if err == "" {
		return "not connected yet"
	}
	return err
}
