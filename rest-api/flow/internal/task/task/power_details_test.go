// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestPowerTaskDetails_RelevanceFor keeps direct selection separate from shelf
// effects, so a tray response never claims an unselected tray was requested.
func TestPowerTaskDetails_RelevanceFor(t *testing.T) {
	trayID := uuid.New()
	shelfID := uuid.New()

	cases := []struct {
		name          string
		componentType devicetypes.ComponentType
		targets       map[devicetypes.ComponentType][]uuid.UUID
		want          PowerTaskRelevance
	}{
		{
			// Selection wins when the same task also selects a shelf.
			name:          "selected tray with rack effects",
			componentType: devicetypes.ComponentTypeCompute,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute:    {trayID},
				devicetypes.ComponentTypePowerShelf: {shelfID},
			},
			want: PowerTaskSelected,
		},
		{
			// NVSwitch selection must use the requested type, not assume Compute.
			name:          "selected NVSwitch",
			componentType: devicetypes.ComponentTypeNVSwitch,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeNVSwitch: {trayID},
			},
			want: PowerTaskSelected,
		},
		{
			// A selected shelf must remain selected rather than only rack-affected.
			name:          "selected shelf takes precedence over rack effect",
			componentType: devicetypes.ComponentTypePowerShelf,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypePowerShelf: {trayID},
			},
			want: PowerTaskSelected,
		},
		{
			// A shelf changes rack power without selecting every compute tray.
			name:          "shelf affects an unselected tray",
			componentType: devicetypes.ComponentTypeCompute,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypePowerShelf: {shelfID},
			},
			want: PowerTaskRackAffected,
		},
		{
			// A UUID alone must not match a target recorded as another type.
			name:          "same UUID under another type",
			componentType: devicetypes.ComponentTypeCompute,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeNVSwitch: {trayID},
			},
			want: PowerTaskUnrelated,
		},
		{
			// Other compute trays can have independent operations in this rack.
			name:          "different tray",
			componentType: devicetypes.ComponentTypeCompute,
			targets: map[devicetypes.ComponentType][]uuid.UUID{
				devicetypes.ComponentTypeCompute:    {uuid.New()},
				devicetypes.ComponentTypePowerShelf: {},
			},
			want: PowerTaskUnrelated,
		},
		{
			// Older tasks with no recorded targets cannot imply all-rack selection.
			name:          "no recorded targets",
			componentType: devicetypes.ComponentTypeCompute,
			want:          PowerTaskUnrelated,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			details := PowerTaskDetails{
				Attributes: taskcommon.TaskAttributes{ComponentsByType: tc.targets},
			}

			got := details.RelevanceFor(trayID, tc.componentType)

			assert.Equal(t, tc.want, got)
		})
	}
}
