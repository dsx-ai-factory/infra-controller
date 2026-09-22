// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

func TestTaskFrom(t *testing.T) {
	triggerID := uuid.New()
	tests := []struct {
		name string
		task *taskdef.Task
	}{
		{
			name: "event rule execution trigger",
			task: &taskdef.Task{
				TriggerType: operation.TriggerTypeEventRuleExecution,
				TriggerID:   &triggerID,
			},
		},
		{
			name: "trigger without occurrence ID",
			task: &taskdef.Task{TriggerType: operation.TriggerTypeAPI},
		},
		{
			name: "no trigger",
			task: &taskdef.Task{},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			converted, err := TaskFrom(TaskTo(test.task))

			require.NoError(t, err)
			assert.Equal(t, test.task.TriggerType, converted.TriggerType)
			assert.Equal(t, test.task.TriggerID, converted.TriggerID)
		})
	}

	zeroTriggerID := uuid.Nil
	invalid := []struct {
		name        string
		triggerType string
		triggerID   *uuid.UUID
		wantErr     string
	}{
		{
			name:        "unknown trigger type",
			triggerType: "unknown",
			triggerID:   &triggerID,
			wantErr:     `invalid persisted task trigger: unknown trigger type "unknown"`,
		},
		{
			name:      "ID without trigger type",
			triggerID: &triggerID,
			wantErr:   `invalid persisted task trigger: trigger type "" does not accept a trigger ID`,
		},
		{
			name:        "API trigger with ID",
			triggerType: string(operation.TriggerTypeAPI),
			triggerID:   &triggerID,
			wantErr:     `invalid persisted task trigger: trigger type "api" does not accept a trigger ID`,
		},
		{
			name:        "event-rule trigger without ID",
			triggerType: string(operation.TriggerTypeEventRuleExecution),
			wantErr:     `invalid persisted task trigger: trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
		{
			name:        "event-rule trigger with zero ID",
			triggerType: string(operation.TriggerTypeEventRuleExecution),
			triggerID:   &zeroTriggerID,
			wantErr:     `invalid persisted task trigger: trigger type "event_rule_execution" requires a non-zero trigger ID`,
		},
	}

	for _, test := range invalid {
		t.Run(test.name, func(t *testing.T) {
			converted, err := TaskFrom(&model.Task{
				TriggerType: test.triggerType,
				TriggerID:   test.triggerID,
			})

			require.Nil(t, converted)
			require.EqualError(t, err, test.wantErr)
		})
	}
}
