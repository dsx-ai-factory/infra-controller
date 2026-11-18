// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"
)

// newHealthTestManager points ManagerAccess at fresh Site Agent data for one test.
func newHealthTestManager(t *testing.T, conf *conftypes.Config) *elektratypes.Elektra {
	t.Helper()
	previousAccess := ManagerAccess
	t.Cleanup(func() { ManagerAccess = previousAccess })
	data := &elektratypes.Elektra{Conf: conf, Managers: managertypes.NewManagerType(), Log: zerolog.Nop()}
	NewWorkflowManager(data, nil, &managerapi.ManagerConf{EB: conf})
	return data
}

// stoppedWorker returns the status of a worker that failed to start or stopped.
func stoppedWorker(err error) *workflowtypes.WorkerStatus {
	status := workflowtypes.NewWorkerStatus()
	status.SetErr(err)
	return status
}

type healthCheckClient struct {
	client.Client
	err error
	// during runs inside CheckHealth, to change the worker while it is being checked.
	during func()
}

func (c healthCheckClient) CheckHealth(context.Context, *client.CheckHealthRequest) (*client.CheckHealthResponse, error) {
	if c.during != nil {
		c.during()
	}
	if c.err != nil {
		return nil, c.err
	}
	return &client.CheckHealthResponse{}, nil
}

func TestAPI_CheckLiveness(t *testing.T) {
	stopErr := errors.New("namespace not found")
	tests := []struct {
		name    string
		worker  *workflowtypes.WorkerStatus
		wantErr error
	}{
		{name: "no connection attempt yet"},
		{name: "worker running", worker: workflowtypes.NewWorkerStatus()},
		{name: "worker stopped", worker: stoppedWorker(stopErr), wantErr: stopErr},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := newHealthTestManager(t, &conftypes.Config{})
			data.Managers.Workflow.State.SetWorker(tt.worker)
			assert.ErrorIs(t, (&API{}).CheckLiveness(), tt.wantErr)
		})
	}
}

func TestAPI_CheckConnection(t *testing.T) {
	stopErr := errors.New("namespace not found")
	healthErr := errors.New("health check error: connection refused")
	tests := []struct {
		name string
		// worker returns the worker the latest connection attempt started.
		worker     func(state *workflowtypes.State) *workflowtypes.WorkerStatus
		wantHealth computils.CompStatus
		wantErr    string
	}{
		{
			name:       "no connection attempt yet",
			worker:     func(*workflowtypes.State) *workflowtypes.WorkerStatus { return nil },
			wantHealth: computils.CompNotKnown,
		},
		{
			name:       "worker stopped",
			worker:     func(*workflowtypes.State) *workflowtypes.WorkerStatus { return stoppedWorker(stopErr) },
			wantHealth: computils.CompUnhealthy,
			wantErr:    stopErr.Error(),
		},
		{
			name: "a Temporal client cannot reach the frontend",
			worker: func(*workflowtypes.State) *workflowtypes.WorkerStatus {
				return workflowtypes.NewWorkerStatus(healthCheckClient{}, healthCheckClient{err: healthErr})
			},
			wantHealth: computils.CompUnhealthy,
			wantErr:    healthErr.Error(),
		},
		{
			name: "worker running and connected",
			worker: func(*workflowtypes.State) *workflowtypes.WorkerStatus {
				return workflowtypes.NewWorkerStatus(healthCheckClient{}, healthCheckClient{})
			},
			wantHealth: computils.CompHealthy,
		},
		{
			name: "a reconnect finishes during the check",
			worker: func(state *workflowtypes.State) *workflowtypes.WorkerStatus {
				return workflowtypes.NewWorkerStatus(healthCheckClient{during: func() {
					state.SetWorker(workflowtypes.NewWorkerStatus())
				}})
			},
			wantHealth: computils.CompNotKnown,
		},
		{
			name: "the worker stops during the check",
			worker: func(*workflowtypes.State) *workflowtypes.WorkerStatus {
				var status *workflowtypes.WorkerStatus
				status = workflowtypes.NewWorkerStatus(healthCheckClient{during: func() { status.SetErr(stopErr) }})
				return status
			},
			wantHealth: computils.CompUnhealthy,
			wantErr:    stopErr.Error(),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := newHealthTestManager(t, &conftypes.Config{})
			state := data.Managers.Workflow.State
			state.HealthStatus.Store(uint64(computils.CompNotKnown))
			state.SetWorker(tt.worker(state))

			(&API{}).CheckConnection(context.Background())

			assert.Equal(t, tt.wantHealth, computils.CompStatus(state.HealthStatus.Load()))
			assert.Equal(t, tt.wantErr, state.Err())
		})
	}
}

func TestStopWorker(t *testing.T) {
	stopErr := errors.New("task queue name cannot start with reserved prefix /_sys/")
	tests := []struct {
		name            string
		replaced        bool
		wantHealth      computils.CompStatus
		wantLivenessErr error
	}{
		{
			name:            "current worker fails liveness",
			wantHealth:      computils.CompUnhealthy,
			wantLivenessErr: stopErr,
		},
		{
			name:       "worker a reload replaced is ignored",
			replaced:   true,
			wantHealth: computils.CompHealthy,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := newHealthTestManager(t, &conftypes.Config{})
			state := data.Managers.Workflow.State
			state.HealthStatus.Store(uint64(computils.CompHealthy))
			status := workflowtypes.NewWorkerStatus()
			state.SetWorker(status)
			if tt.replaced {
				state.SetWorker(workflowtypes.NewWorkerStatus())
			}

			stopWorker(data, status, stopErr)

			assert.ErrorIs(t, status.Err(), stopErr)
			assert.Equal(t, tt.wantHealth, computils.CompStatus(state.HealthStatus.Load()))
			assert.ErrorIs(t, (&API{}).CheckLiveness(), tt.wantLivenessErr)
		})
	}
}

func TestOrchestrator(t *testing.T) {
	t.Run("failed reconnect fails liveness", func(t *testing.T) {
		// No client certificate exists under TemporalCertPath, so the attempt
		// fails before dialing Temporal.
		conf := &conftypes.Config{
			EnableTLS: true,
			Temporal:  conftypes.TemporalConfig{TemporalCertPath: t.TempDir()},
		}
		data := newHealthTestManager(t, conf)
		state := data.Managers.Workflow.State
		state.SetWorker(workflowtypes.NewWorkerStatus())

		Orchestrator()

		assert.ErrorContains(t, (&API{}).CheckLiveness(), "no such file or directory")
		assert.Equal(t, computils.CompUnhealthy, computils.CompStatus(state.HealthStatus.Load()))
	})
}
