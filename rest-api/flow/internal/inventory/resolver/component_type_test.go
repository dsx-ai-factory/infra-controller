// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/stretchr/testify/require"
)

func TestComponentTypeToFlow(t *testing.T) {
	tests := map[string]struct {
		componentType devicetypes.ComponentType
		want          flowtypes.ComponentType
		wantErr       bool
	}{
		"compute":     {devicetypes.ComponentTypeCompute, flowtypes.ComponentTypeCompute, false},
		"NVSwitch":    {devicetypes.ComponentTypeNVSwitch, flowtypes.ComponentTypeNVSwitch, false},
		"power shelf": {devicetypes.ComponentTypePowerShelf, flowtypes.ComponentTypePowerShelf, false},
		"ToR switch":  {devicetypes.ComponentTypeToRSwitch, flowtypes.ComponentTypeTORSwitch, false},
		"UMS":         {devicetypes.ComponentTypeUMS, flowtypes.ComponentTypeUMS, false},
		"CDU":         {devicetypes.ComponentTypeCDU, flowtypes.ComponentTypeCDU, false},
		"unknown":     {devicetypes.ComponentTypeUnknown, flowtypes.ComponentTypeUnknown, true},
		"invalid":     {devicetypes.ComponentType(100), flowtypes.ComponentTypeUnknown, true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			got, err := ComponentTypeToFlow(test.componentType)
			if test.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			require.Equal(t, test.want, got)
		})
	}
}
