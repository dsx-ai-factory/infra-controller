// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestBuildTargets(t *testing.T) {
	for _, tc := range []struct {
		name     string
		ids      []string
		wantIDs  []string
		wantType common.IdentifierType
	}{
		{"ingested", []string{"machine-1", "machine-2"}, []string{"machine-1", "machine-2"}, common.IdentifierTypeManagerID},
		{"mixed", []string{"machine-1", ""}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
		{"missing first", []string{"", "machine-2"}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
		{"pre ingestion", []string{"", ""}, []string{"mac-1", "mac-2"}, common.IdentifierTypeMACAddress},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := &task.ExecutionInfo{Components: []task.WorkflowComponent{
				{Type: devicetypes.ComponentTypeCompute, ComponentID: tc.ids[0], MACAddress: "mac-1"},
				{Type: devicetypes.ComponentTypeCompute, ComponentID: tc.ids[1], MACAddress: "mac-2"},
			}}
			target := buildTargets(info)[devicetypes.ComponentTypeCompute]
			assert.Equal(t, tc.wantIDs, target.Identifiers)
			assert.Equal(t, tc.wantType, target.IdentifierType)
		})
	}
}

func TestChildWorkflowExecutionTimeout(t *testing.T) {
	tests := []struct {
		name           string
		maxParallel    int
		componentCount int
		want           time.Duration
	}{
		{
			name:           "unlimited uses one batch",
			maxParallel:    0,
			componentCount: 5,
			want:           3*time.Minute + 30*time.Second,
		},
		{
			name:           "limit equal to count uses one batch",
			maxParallel:    3,
			componentCount: 3,
			want:           3*time.Minute + 30*time.Second,
		},
		{
			name:           "partial final batch extends main operation budget",
			maxParallel:    2,
			componentCount: 5,
			want:           5*time.Minute + 50*time.Second,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			step := operationrules.SequenceStep{
				MaxParallel: tc.maxParallel,
				Timeout:     time.Minute,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionGetPowerStatus,
				},
				PreOperation: []operationrules.ActionConfig{
					{
						Name:    operationrules.ActionGetPowerStatus,
						Timeout: 10 * time.Second,
					},
				},
				PostOperation: []operationrules.ActionConfig{
					{
						Name:    operationrules.ActionSleep,
						Timeout: 20 * time.Second,
					},
				},
			}

			assert.Equal(
				t,
				tc.want,
				childWorkflowExecutionTimeout(step, tc.componentCount),
			)
		})
	}
}
