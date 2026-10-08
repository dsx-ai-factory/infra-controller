// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExtractComponentTargetVersion(t *testing.T) {
	tests := map[string]struct {
		rawVersion    string
		componentType devicetypes.ComponentType
		expected      string
		selected      bool
	}{
		"empty string returns empty": {
			rawVersion:    "",
			componentType: devicetypes.ComponentTypeCompute,
			expected:      "",
			selected:      true,
		},
		"layered JSON - compute section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30","uefi":"2.22.1"},"nvswitch":{"nvos":"1.2.3"}}`,
			componentType: devicetypes.ComponentTypeCompute,
			expected:      `{"bmc":"7.10.30","uefi":"2.22.1"}`,
			selected:      true,
		},
		"layered JSON - nvswitch section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":{"nvos":"1.2.3","cpld":"4.5.6"}}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      `{"nvos":"1.2.3","cpld":"4.5.6"}`,
			selected:      true,
		},
		"layered JSON - powershelf section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"powershelf":{"firmware":"1.0.0"}}`,
			componentType: devicetypes.ComponentTypePowerShelf,
			expected:      `{"firmware":"1.0.0"}`,
			selected:      true,
		},
		"layered JSON - missing key omits component": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":{"nvos":"1.2.3"}}`,
			componentType: devicetypes.ComponentTypePowerShelf,
			expected:      "",
			selected:      false,
		},
		"layered JSON - string scalar value is unquoted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":"2.0.0"}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "2.0.0",
			selected:      true,
		},
		"layered JSON - string scalar with escapes is unquoted": {
			rawVersion:    `{"nvswitch":"r1.3.9-alpha"}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "r1.3.9-alpha",
			selected:      true,
		},
		"old flat JSON - no known keys, returns as-is for backward compat": {
			rawVersion:    `{"bmc":"7.10.30","uefi":"2.22.1"}`,
			componentType: devicetypes.ComponentTypeCompute,
			expected:      `{"bmc":"7.10.30","uefi":"2.22.1"}`,
			selected:      true,
		},
		"non-JSON string - returns as-is": {
			rawVersion:    "2.0.0",
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "2.0.0",
			selected:      true,
		},
		"only one component type present - other types are omitted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"}}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "",
			selected:      false,
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result, selected := extractComponentTargetVersion(tc.rawVersion, tc.componentType)
			assert.Equal(t, tc.expected, result)
			assert.Equal(t, tc.selected, selected)
		})
	}
}
