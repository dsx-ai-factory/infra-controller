// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestBuildRestartSteps(t *testing.T) {
	for _, tc := range []struct {
		name      string
		stage     int
		noTargets bool
		noRule    bool
		wantError string
	}{
		{name: "bind after sparse stages", stage: 8},
		{name: "empty snapshot adds no work", noTargets: true},
		{name: "missing main rule", noRule: true, wantError: "require a main rule"},
		{name: "stage overflow returns no partial plan", stage: int(^uint(0)>>1) - 1, wantError: "no stage number"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rule := &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{{Stage: tc.stage}}}
			if tc.noRule {
				rule = nil
			}
			components := []taskdef.WorkflowComponent{{Type: devicetypes.ComponentTypeCompute, ComponentID: "compute"}}
			if tc.noTargets {
				components = nil
			}
			steps, err := buildRestartSteps(rule, components)
			if tc.wantError != "" {
				require.ErrorContains(t, err, tc.wantError)
				require.Nil(t, steps)
				return
			}
			require.NoError(t, err)
			if tc.noTargets {
				require.Empty(t, steps)
				return
			}
			require.Equal(t, tc.stage, rule.Steps[0].Stage)
			require.Len(t, steps, 2)
			for i, step := range steps {
				require.Equal(t, tc.stage+i+1, step.Definition.Stage)
				require.Equal(t, components, step.Components)
			}
			components[0].ComponentID = "later-change"
			require.Equal(t, "compute", steps[0].Components[0].ComponentID)
			require.Equal(t, "compute", steps[1].Components[0].ComponentID)
		})
	}
}
