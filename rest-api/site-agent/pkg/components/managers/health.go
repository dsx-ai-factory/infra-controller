// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/rs/zerolog/log"

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
}

// handleLivenessRequest fails once only a restart brings the Site Agent back: its Site was
// re-paired, or its Temporal worker is gone for good. Kubernetes then restarts it, whether
// or not the Site Agent is Ready. Any client that reaches the pod can read the response, so
// it leaves out the error, which is logged.
func handleLivenessRequest(w http.ResponseWriter, r *http.Request) {
	if err := ManagerAccess.API.Bootstrap.CheckRegistration(); err != nil {
		log.Warn().Err(err).Msg("Managers: Site was re-paired, failing the liveness check")
		http.Error(w, "Site was re-paired", http.StatusServiceUnavailable)
		return
	}
	if err := ManagerAccess.API.Orchestrator.CheckLiveness(); err != nil {
		http.Error(w, "Temporal worker is not running", http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}

// handleReadinessRequest reports the latest Temporal and Core gRPC state, the same
// state behind the health metrics, and lists every dependency that is not healthy. Like
// the liveness response, it leaves out the errors, which are logged.
func handleReadinessRequest(w http.ResponseWriter, r *http.Request) {
	managers := ManagerAccess.Data.EB.Managers
	var failures []string
	if health := computils.CompStatus(managers.Workflow.State.HealthStatus.Load()); health != computils.CompHealthy {
		failures = append(failures, "Temporal: "+health.String())
	}
	if health := computils.CompStatus(managers.CoreGrpc.State.HealthStatus.Load()); health != computils.CompHealthy {
		failures = append(failures, "Core gRPC: "+health.String())
	}
	if len(failures) > 0 {
		http.Error(w, strings.Join(failures, "\n"), http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}
