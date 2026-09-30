// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
)

type fakeOrchestrator struct {
	managerapi.OrchestratorInterface
	livenessErr error
	check       func(context.Context)
}

func (f fakeOrchestrator) CheckLiveness() error { return f.livenessErr }

func (f fakeOrchestrator) CheckConnection(ctx context.Context) { f.check(ctx) }

type fakeCoreGrpc struct {
	managerapi.CoreGrpcInterface
	check func(context.Context)
}

func (f fakeCoreGrpc) CheckConnection(ctx context.Context) { f.check(ctx) }

// newHealthTestManager points ManagerAccess at fresh Site Agent data for one test.
func newHealthTestManager(t *testing.T, api *managerapi.ManagerAPI) *elektratypes.Elektra {
	t.Helper()
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	data := &elektratypes.Elektra{Managers: managertypes.NewManagerType()}
	ManagerAccess = &Manager{API: api, Data: &managerapi.ManagerData{EB: data}}
	return data
}

func TestCheckHealth(t *testing.T) {
	tests := []struct {
		name       string
		coreHealth computils.CompStatus
		wantHealth computils.CompStatus
	}{
		{name: "every dependency healthy", coreHealth: computils.CompHealthy, wantHealth: computils.CompHealthy},
		{name: "a dependency unhealthy", coreHealth: computils.CompUnhealthy, wantHealth: computils.CompUnhealthy},
	}
	assertDeadline := func(t *testing.T, ctx context.Context) {
		t.Helper()
		deadline, ok := ctx.Deadline()
		require.True(t, ok)
		assert.WithinDuration(t, time.Now().Add(healthCheckTimeout), deadline, time.Second)
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var checked []string
			var data *elektratypes.Elektra
			data = newHealthTestManager(t, &managerapi.ManagerAPI{
				Orchestrator: fakeOrchestrator{check: func(ctx context.Context) {
					assertDeadline(t, ctx)
					checked = append(checked, "Temporal")
					data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompHealthy))
				}},
				CoreGrpc: fakeCoreGrpc{check: func(ctx context.Context) {
					assertDeadline(t, ctx)
					checked = append(checked, "Core gRPC")
					data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(tt.coreHealth))
				}},
			})

			checkHealth()

			assert.Equal(t, []string{"Temporal", "Core gRPC"}, checked)
			assert.Equal(t, tt.wantHealth, computils.CompStatus(data.HealthStatus.Load()))
		})
	}
}

func TestHandleLivenessRequest(t *testing.T) {
	tests := []struct {
		name     string
		err      error
		wantCode int
		wantBody string
	}{
		{name: "worker running", wantCode: http.StatusOK, wantBody: "ok\n"},
		{
			name:     "worker stopped",
			err:      errors.New("task queue name cannot start with reserved prefix /_sys/"),
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal worker is not running: task queue name cannot start with reserved prefix /_sys/\n",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			newHealthTestManager(t, &managerapi.ManagerAPI{Orchestrator: fakeOrchestrator{livenessErr: tt.err}})
			response := httptest.NewRecorder()
			handleLivenessRequest(response, httptest.NewRequest(http.MethodGet, computils.LivenessStatus, nil))
			assert.Equal(t, tt.wantCode, response.Code)
			assert.Equal(t, tt.wantBody, response.Body.String())
		})
	}
}

func TestHandleReadinessRequest(t *testing.T) {
	tests := []struct {
		name string
		// record sets the Temporal and Core gRPC state the latest checks left behind.
		record   func(data *elektratypes.Elektra)
		wantCode int
		wantBody string
	}{
		{
			name: "every dependency healthy",
			record: func(data *elektratypes.Elektra) {
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompHealthy))
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
			},
			wantCode: http.StatusOK,
			wantBody: "ok\n",
		},
		{
			name: "every failing dependency is reported",
			record: func(data *elektratypes.Elektra) {
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
				data.Managers.Workflow.State.SetErr("health check error: connection refused")
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
				data.Managers.CoreGrpc.State.SetErr("context deadline exceeded")
			},
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal: health check error: connection refused\nCore gRPC: context deadline exceeded\n",
		},
		{
			name: "dependencies not connected yet",
			record: func(data *elektratypes.Elektra) {
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompNotKnown))
			},
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal: not connected yet\nCore gRPC: not connected yet\n",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := newHealthTestManager(t, &managerapi.ManagerAPI{})
			tt.record(data)
			response := httptest.NewRecorder()
			handleReadinessRequest(response, httptest.NewRequest(http.MethodGet, computils.ReadinessStatus, nil))
			assert.Equal(t, tt.wantCode, response.Code)
			assert.Equal(t, tt.wantBody, response.Body.String())
		})
	}
}
