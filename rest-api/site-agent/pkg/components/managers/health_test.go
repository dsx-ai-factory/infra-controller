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
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
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

type fakeFlowGrpc struct {
	managerapi.FlowGrpcInterface
	check func(context.Context)
}

func (f fakeFlowGrpc) CheckConnection(ctx context.Context) { f.check(ctx) }

type fakeBootstrap struct {
	managerapi.BootstrapInterface
	registrationErr error
}

func (f fakeBootstrap) CheckRegistration() error { return f.registrationErr }

// newHealthTestManager points ManagerAccess at fresh Site Agent data for one test.
func newHealthTestManager(t *testing.T, api *managerapi.ManagerAPI) *elektratypes.Elektra {
	t.Helper()
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	conf := &conftypes.Config{}
	data := &elektratypes.Elektra{Managers: managertypes.NewManagerType(), Conf: conf}
	ManagerAccess = &Manager{
		API:  api,
		Data: &managerapi.ManagerData{EB: data},
		Conf: &managerapi.ManagerConf{EB: conf},
	}
	return data
}

func TestCheckHealth(t *testing.T) {
	assertDeadline := func(t *testing.T, ctx context.Context) {
		t.Helper()
		deadline, ok := ctx.Deadline()
		require.True(t, ok)
		assert.WithinDuration(t, time.Now().Add(healthCheckTimeout), deadline, time.Second)
	}
	tests := []struct {
		name            string
		flowGrpcEnabled bool
		wantChecked     []string
	}{
		{name: "Flow gRPC disabled", wantChecked: []string{"Temporal", "Core gRPC"}},
		{name: "Flow gRPC enabled", flowGrpcEnabled: true, wantChecked: []string{"Temporal", "Core gRPC", "Flow gRPC"}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var checked []string
			check := func(name string) func(context.Context) {
				return func(ctx context.Context) {
					assertDeadline(t, ctx)
					checked = append(checked, name)
				}
			}
			data := newHealthTestManager(t, &managerapi.ManagerAPI{
				Orchestrator: fakeOrchestrator{check: check("Temporal")},
				CoreGrpc:     fakeCoreGrpc{check: check("Core gRPC")},
				FlowGrpc:     fakeFlowGrpc{check: check("Flow gRPC")},
			})
			data.Conf.FlowGrpc.Enabled = tt.flowGrpcEnabled

			checkHealth()

			assert.Equal(t, tt.wantChecked, checked)
		})
	}
}

func TestHandleLivenessRequest(t *testing.T) {
	tests := []struct {
		name            string
		registrationErr error
		workerErr       error
		wantCode        int
		wantBody        string
	}{
		{name: "worker running", wantCode: http.StatusOK, wantBody: "ok\n"},
		{
			name:            "Site re-paired",
			registrationErr: errors.New("site-registration holds Site ID 5b0e7c1a, not d2f4b0c6"),
			wantCode:        http.StatusServiceUnavailable,
			wantBody:        "Site was re-paired\n",
		},
		{
			name:      "worker stopped",
			workerErr: errors.New("failed reaching server: dial tcp 10.0.5.12:7233: connect: connection refused"),
			wantCode:  http.StatusServiceUnavailable,
			wantBody:  "Temporal worker is not running\n",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			newHealthTestManager(t, &managerapi.ManagerAPI{
				Bootstrap:    fakeBootstrap{registrationErr: tt.registrationErr},
				Orchestrator: fakeOrchestrator{livenessErr: tt.workerErr},
			})
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
		// record sets the dependency state the latest checks left behind.
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
				data.Managers.Workflow.State.SetErr("health check error: dial tcp 10.0.5.12:7233: connect: connection refused")
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
				data.Managers.CoreGrpc.State.Err.Store("rpc error: code = Unavailable desc = dial tcp 10.0.5.13:1079: connect: connection refused")
			},
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal: Unhealthy\nCore gRPC: Unhealthy\n",
		},
		{
			name: "dependencies not connected yet",
			record: func(data *elektratypes.Elektra) {
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompNotKnown))
			},
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Temporal: Unhealthy\nCore gRPC: NotKnown\n",
		},
		{
			name: "enabled Flow gRPC is reported",
			record: func(data *elektratypes.Elektra) {
				data.Conf.FlowGrpc.Enabled = true
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompHealthy))
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
				data.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
			},
			wantCode: http.StatusServiceUnavailable,
			wantBody: "Flow gRPC: Unhealthy\n",
		},
		{
			name: "disabled Flow gRPC is ignored",
			record: func(data *elektratypes.Elektra) {
				data.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompHealthy))
				data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompHealthy))
				data.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompUnhealthy))
			},
			wantCode: http.StatusOK,
			wantBody: "ok\n",
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
