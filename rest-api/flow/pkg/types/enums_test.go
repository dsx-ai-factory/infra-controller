// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestParseComponentType(t *testing.T) {
	tests := map[string]struct {
		value    string
		expected ComponentType
		wantErr  bool
	}{
		"compute": {value: "COMPUTE", expected: ComponentTypeCompute},
		"cdu":     {value: "CDU", expected: ComponentTypeCDU},
		"unknown": {value: "UNKNOWN", wantErr: true},
		"invalid": {value: "GPU", wantErr: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			actual, err := ParseComponentType(test.value)
			if test.wantErr {
				require.Error(t, err)
				require.Equal(t, ComponentTypeUnknown, actual)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.expected, actual)
		})
	}
}

func TestComponentTypeValidate(t *testing.T) {
	tests := map[string]struct {
		componentType ComponentType
		wantError     bool
	}{
		"compute":     {componentType: ComponentTypeCompute},
		"nvswitch":    {componentType: ComponentTypeNVSwitch},
		"power shelf": {componentType: ComponentTypePowerShelf},
		"tor switch":  {componentType: ComponentTypeTORSwitch},
		"ums":         {componentType: ComponentTypeUMS},
		"cdu":         {componentType: ComponentTypeCDU},
		"unknown":     {componentType: ComponentTypeUnknown, wantError: true},
		"empty":       {wantError: true},
		"invalid":     {componentType: ComponentType("INVALID"), wantError: true},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.componentType.Validate()
			if test.wantError {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
		})
	}
}
