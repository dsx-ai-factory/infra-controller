// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec_test

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/codec"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExecutionPlanRoundTrip(t *testing.T) {
	taskOperation := &operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationForcePowerOff,
	}

	info, err := taskOperation.Marshal()
	require.NoError(t, err)

	rackID := uuid.New()
	componentID := uuid.New()

	tests := map[string]eventrule.ExecutionPlan{
		"submit task": &eventrule.SubmitTaskPlan{
			Operation: operation.Wrapper{
				Type: taskOperation.Type(),
				Code: taskOperation.CodeString(),
				Info: info,
			},
			Description:      "Power off component",
			ConflictStrategy: operation.ConflictStrategyQueue,
			Targets: []operation.RackExecutionTarget{{
				RackID: rackID,
				ComponentsByType: operation.ComponentsByType{
					devicetypes.ComponentTypeCompute: []uuid.UUID{componentID},
				},
			}},
		},
		"send alert": &eventrule.SendAlertPlan{
			Severity: eventrule.SeverityCritical,
			Message:  "leak detected",
		},
		"noop": &eventrule.NoopPlan{Reason: "audit only"},
	}

	for name, plan := range tests {
		t.Run(name, func(t *testing.T) {
			encoded, err := codec.MarshalExecutionPlan(plan)
			require.NoError(t, err)

			decoded, err := codec.UnmarshalExecutionPlan(encoded)
			require.NoError(t, err)

			require.Equal(t, plan, decoded)
		})
	}
}

func TestExecutionPlanUnmarshalRejectsUnknownVersion(t *testing.T) {
	plan, err := codec.UnmarshalExecutionPlan([]byte(`{"version":2,"type":"noop","plan":{}}`))

	require.ErrorContains(t, err, "unknown execution plan version 2")
	require.Nil(t, plan)
}
