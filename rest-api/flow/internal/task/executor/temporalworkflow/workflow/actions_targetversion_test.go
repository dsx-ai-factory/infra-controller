// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestExtractComponentTargetVersion(t *testing.T) {
	tests := map[string]struct {
		rawVersion    string
		componentType devicetypes.ComponentType
		expected      string
	}{
		"empty string returns empty": {
			rawVersion:    "",
			componentType: devicetypes.ComponentTypeCompute,
			expected:      "",
		},
		"layered JSON — compute section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30","uefi":"2.22.1"},"nvswitch":{"nvos":"1.2.3"}}`,
			componentType: devicetypes.ComponentTypeCompute,
			expected:      `{"bmc":"7.10.30","uefi":"2.22.1"}`,
		},
		"layered JSON — nvswitch section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":{"nvos":"1.2.3","cpld":"4.5.6"}}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      `{"nvos":"1.2.3","cpld":"4.5.6"}`,
		},
		"layered JSON — powershelf section extracted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"powershelf":{"firmware":"1.0.0"}}`,
			componentType: devicetypes.ComponentTypePowerShelf,
			expected:      `{"firmware":"1.0.0"}`,
		},
		"layered JSON — missing key returns empty (component omitted)": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":{"nvos":"1.2.3"}}`,
			componentType: devicetypes.ComponentTypePowerShelf,
			expected:      "",
		},
		"layered JSON — string scalar value is unquoted": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"},"nvswitch":"2.0.0"}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "2.0.0",
		},
		"layered JSON — string scalar with escapes is unquoted": {
			rawVersion:    `{"nvswitch":"r1.3.9-alpha"}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "r1.3.9-alpha",
		},
		"old flat JSON — no known keys, returns as-is for backward compat": {
			rawVersion:    `{"bmc":"7.10.30","uefi":"2.22.1"}`,
			componentType: devicetypes.ComponentTypeCompute,
			expected:      `{"bmc":"7.10.30","uefi":"2.22.1"}`,
		},
		"non-JSON string — returns as-is": {
			rawVersion:    "2.0.0",
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "2.0.0",
		},
		"only one component type present — other types get empty": {
			rawVersion:    `{"compute":{"bmc":"7.10.30"}}`,
			componentType: devicetypes.ComponentTypeNVSwitch,
			expected:      "",
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			result := extractComponentTargetVersion(tc.rawVersion, tc.componentType)
			assert.Equal(t, tc.expected, result)
		})
	}
}
