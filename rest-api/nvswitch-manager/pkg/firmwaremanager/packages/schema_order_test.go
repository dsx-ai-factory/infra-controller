// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package packages

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestFirmwarePackage_GetOrderedComponents(t *testing.T) {
	tests := []struct {
		name           string
		components     []string
		componentOrder []string
		want           []string
	}{
		{
			name:       "default uses canonical update order",
			components: []string{"nvos", "bios", "cpld", "bmc"},
			want:       []string{"bmc", "cpld", "bios", "nvos"},
		},
		{
			name:       "default skips absent components",
			components: []string{"nvos", "bios", "bmc"},
			want:       []string{"bmc", "bios", "nvos"},
		},
		{
			name:           "explicit subset does not append other components",
			components:     []string{"bmc", "cpld", "bios", "nvos"},
			componentOrder: []string{"bios", "bmc"},
			want:           []string{"bios", "bmc"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			components := make(map[string]ComponentDef, len(tt.components))
			for _, name := range tt.components {
				components[name] = ComponentDef{
					Version:  "1.0.0",
					File:     name + ".bin",
					Strategy: "redfish",
				}
			}
			pkg := FirmwarePackage{
				Version:        "1.0.0",
				Components:     components,
				ComponentOrder: tt.componentOrder,
			}

			assert.Equal(t, tt.want, pkg.GetOrderedComponents())
		})
	}
}
