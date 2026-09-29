// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"
	"time"

	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"

	activitypkg "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestVerifyReachabilityEnforcesDeadlineBetweenBatches(t *testing.T) {
	env := (&testsuite.WorkflowTestSuite{}).NewTestWorkflowEnvironment()
	env.RegisterActivityWithOptions(mockGetPowerStatus, activity.RegisterOptions{
		Name: activitypkg.NameGetPowerStatus,
	})
	env.OnActivity(mockGetPowerStatus, mock.Anything, mock.Anything).
		After(2*time.Second).
		Return(map[string]operations.PowerStatus{
			"a": operations.PowerStatusOn,
			"b": operations.PowerStatusOn,
		}, nil)

	env.ExecuteWorkflow(func(ctx workflow.Context) error {
		ctx = workflow.WithActivityOptions(ctx, workflow.ActivityOptions{
			StartToCloseTimeout: 5 * time.Second,
		})
		target := common.Target{
			Type:         devicetypes.ComponentTypeCompute,
			ComponentIDs: []string{"a", "b"},
		}
		return verifyReachability(
			ctx,
			map[devicetypes.ComponentType]common.Target{target.Type: target},
			[]string{"Compute"},
			3*time.Second,
			time.Second,
			true,
			1,
		)
	})

	require.ErrorContains(t, env.GetWorkflowError(), "timeout")
}
