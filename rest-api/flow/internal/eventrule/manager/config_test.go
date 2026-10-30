// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestConfig_Validate(t *testing.T) {
	tests := map[string]struct {
		mutate  func(*Config)
		wantErr string
	}{
		"valid": {},
		"missing store": {
			mutate: func(config *Config) {
				config.Store = nil
			},
			wantErr: "event-rule store is required",
		},
		"missing scheduler instance id": {
			mutate: func(config *Config) {
				config.Scheduler.InstanceID = ""
			},
			wantErr: "scheduler instance ID: execution claim owner is empty",
		},
		"invalid scheduler runtime": {
			mutate: func(config *Config) {
				config.Scheduler.Runtime.PollInterval = 0
			},
			wantErr: "scheduler poll interval must be positive",
		},
		"invalid scheduler policy": {
			mutate: func(config *Config) {
				config.Scheduler.Policy.MaxAttempts = 0
			},
			wantErr: "retry max attempts must be positive",
		},
		"missing inventory reader": {
			mutate: func(config *Config) {
				config.Inventory = nil
			},
			wantErr: "inventory reader is required",
		},
		"missing task manager": {
			mutate: func(config *Config) {
				config.TaskManager = nil
			},
			wantErr: "task manager is required",
		},
		"missing event handling reader": {
			mutate: func(config *Config) {
				config.HandlingReader = nil
			},
			wantErr: "event handling reader is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			config := testManagerConfig()
			if test.mutate != nil {
				test.mutate(&config)
			}

			err := config.Validate()
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}

			require.NoError(t, err)
		})
	}
}

type configTaskManager struct{}

type configHandlingReader struct{}

func (configHandlingReader) GetEventHandling(
	context.Context,
	eventrule.Type,
	eventrule.ResourceKind,
	uuid.UUID,
	int,
	*eventrule.HandlingCursor,
) ([]eventrule.Handling, bool, error) {
	return nil, false, nil
}

func (configTaskManager) SubmitTask(
	context.Context,
	*operation.Request,
) ([]uuid.UUID, error) {
	return nil, nil
}
