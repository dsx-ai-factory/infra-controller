// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/stretchr/testify/require"
)

func TestNew(t *testing.T) {
	tests := map[string]struct {
		mutate          func(*Config)
		wantTypes       []eventrule.ActionType
		wantUnavailable eventrule.ActionType
		wantErr         string
	}{
		"registers required executors": {
			wantTypes: []eventrule.ActionType{
				eventrule.ActionTypeNoop,
				eventrule.ActionTypeSubmitTask,
			},
			wantUnavailable: eventrule.ActionTypeSendAlert,
		},
		"registers optional alert executor": {
			mutate: func(config *Config) {
				config.AlertSender = &recordingAlertSender{}
			},
			wantTypes: []eventrule.ActionType{
				eventrule.ActionTypeNoop,
				eventrule.ActionTypeSubmitTask,
				eventrule.ActionTypeSendAlert,
			},
		},
		"rejects missing task manager": {
			mutate:  func(config *Config) { config.TaskManager = nil },
			wantErr: "task manager is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			config := Config{TaskManager: &recordingTaskManager{}}
			if test.mutate != nil {
				test.mutate(&config)
			}

			registry, err := New(config)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.Nil(t, registry)

				return
			}

			require.NoError(t, err)
			for _, actionType := range test.wantTypes {
				_, err := registry.Executor(actionType)
				require.NoError(t, err)
			}

			if test.wantUnavailable != "" {
				_, err := registry.Executor(test.wantUnavailable)
				require.ErrorContains(t, err, "no executor registered")
			}
		})
	}
}

func TestRegistry(t *testing.T) {
	request := newValidExecutionRequest(t)
	action := eventrule.Action{Name: "noop", Spec: &eventrule.Noop{}}

	t.Run("returns registered executor", func(t *testing.T) {
		registry := &Registry{executors: make(map[eventrule.ActionType]Executor)}

		require.NoError(t, registry.register(eventrule.ActionTypeNoop, NoopExecutor{}))

		actionExecutor, err := registry.Executor(eventrule.ActionTypeNoop)

		require.NoError(t, err)
		require.NoError(t, actionExecutor.Execute(context.Background(), request))
		require.NoError(t, registry.ValidateActions([]eventrule.Action{action}))
	})

	t.Run("rejects duplicate registration", func(t *testing.T) {
		registry := &Registry{executors: make(map[eventrule.ActionType]Executor)}

		require.NoError(t, registry.register(eventrule.ActionTypeNoop, NoopExecutor{}))

		require.ErrorContains(
			t,
			registry.register(eventrule.ActionTypeNoop, NoopExecutor{}),
			"already registered",
		)
	})

	t.Run("rejects unsupported action", func(t *testing.T) {
		registry := &Registry{executors: make(map[eventrule.ActionType]Executor)}

		require.ErrorContains(
			t,
			registry.ValidateActions([]eventrule.Action{action}),
			"no executor registered",
		)

		_, err := registry.Executor(eventrule.ActionTypeNoop)

		require.ErrorContains(t, err, "no executor registered")
	})
}

func TestNoopExecutor_Execute(t *testing.T) {
	request := newValidExecutionRequest(t)
	tests := map[string]struct {
		mutate  func(*ExecutionRequest)
		wantErr string
	}{
		"completed": {},
		"rejects wrong plan": {
			mutate: func(r *ExecutionRequest) {
				r.Plan = &eventrule.SendAlertPlan{Severity: eventrule.SeverityWarning}
			},
			wantErr: "received plan",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			input := request
			input.Plan = eventrule.CloneExecutionPlan(request.Plan)

			if test.mutate != nil {
				test.mutate(&input)
			}

			err := (NoopExecutor{}).Execute(context.Background(), input)

			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.ErrorIs(t, err, ErrTerminal)
				return
			}

			require.NoError(t, err)
		})
	}
}
