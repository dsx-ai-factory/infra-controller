// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"time"
)

const (
	// readinessCacheTTL is how long a successful dependency check is reused, so the
	// kubelet's frequent readiness probes call Temporal and Core gRPC at most once
	// per interval.
	readinessCacheTTL = 30 * time.Second
	// readinessCheckTimeout bounds each dependency check, so a readiness probe still
	// answers within its timeoutSeconds when a dependency hangs.
	readinessCheckTimeout = 2 * time.Second
)

// dependencyCheck is a readiness check whose success is reused for
// readinessCacheTTL. A failure is not cached, so the next probe sees a dependency
// that recovered.
type dependencyCheck struct {
	name        string
	check       func(context.Context) error
	now         func() time.Time
	mu          sync.Mutex
	succeededAt time.Time
}

func (d *dependencyCheck) run(ctx context.Context) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if !d.succeededAt.IsZero() && d.now().Sub(d.succeededAt) < readinessCacheTTL {
		return nil
	}
	ctx, cancel := context.WithTimeout(ctx, readinessCheckTimeout)
	defer cancel()
	err := d.check(ctx)
	if err != nil {
		return err
	}
	d.succeededAt = d.now()
	return nil
}

// probes serves the Kubernetes liveness and readiness probes.
type probes struct {
	liveness     func() error
	dependencies []*dependencyCheck
}

func newProbes() *probes {
	return &probes{
		liveness: func() error {
			return ManagerAccess.API.Orchestrator.CheckLiveness()
		},
		dependencies: []*dependencyCheck{
			{
				name: "Temporal",
				check: func(ctx context.Context) error {
					return ManagerAccess.API.Orchestrator.CheckReadiness(ctx)
				},
				now: time.Now,
			},
			{
				name: "Core gRPC",
				check: func(ctx context.Context) error {
					return ManagerAccess.API.CoreGrpc.CheckReadiness(ctx)
				},
				now: time.Now,
			},
		},
	}
}

// handleLiveness fails once the Temporal worker is gone for good, so Kubernetes
// restarts the Site Agent.
func (p *probes) handleLiveness(w http.ResponseWriter, r *http.Request) {
	err := p.liveness()
	if err != nil {
		http.Error(w, "Temporal worker is not running: "+err.Error(), http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}

// handleReadiness fails while the Site Agent cannot reach Temporal or Core gRPC,
// and lists every dependency that failed.
func (p *probes) handleReadiness(w http.ResponseWriter, r *http.Request) {
	var failures []string
	for _, dependency := range p.dependencies {
		err := dependency.run(r.Context())
		if err != nil {
			failures = append(failures, dependency.name+": "+err.Error())
		}
	}
	if len(failures) > 0 {
		http.Error(w, strings.Join(failures, "\n"), http.StatusServiceUnavailable)
		return
	}
	fmt.Fprintln(w, "ok")
}
