// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package packages

import (
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestFirmwarePackage_Validate(t *testing.T) {
	validComponent := ComponentDef{Version: "1.0", File: "bmc.bin", Strategy: "redfish"}
	tests := []struct {
		name          string
		component     ComponentDef
		mutatePackage func(*FirmwarePackage)
		wantField     string
		wantMessage   string
	}{
		{
			name:      "minimal valid package",
			component: validComponent,
		},
		{
			name:      "missing package version",
			component: validComponent,
			mutatePackage: func(pkg *FirmwarePackage) {
				pkg.Version = ""
			},
			wantField:   "version",
			wantMessage: "version is required",
		},
		{
			name:      "empty components",
			component: validComponent,
			mutatePackage: func(pkg *FirmwarePackage) {
				pkg.Components = map[string]ComponentDef{}
				pkg.ComponentOrder = nil
			},
			wantField:   "components",
			wantMessage: "at least one component is required",
		},
		{
			name:      "order references unknown component",
			component: validComponent,
			mutatePackage: func(pkg *FirmwarePackage) {
				pkg.ComponentOrder = []string{"bmc", "bios"}
			},
			wantField:   "component_order",
			wantMessage: "references unknown component: bios",
		},
		{
			name:        "missing component version",
			component:   ComponentDef{File: "bmc.bin", Strategy: "redfish"},
			wantField:   "components.bmc.version",
			wantMessage: "version is required",
		},
		{
			name:        "missing component file",
			component:   ComponentDef{Version: "1.0", Strategy: "redfish"},
			wantField:   "components.bmc.file",
			wantMessage: "file is required",
		},
		{
			name:        "missing component strategy",
			component:   ComponentDef{Version: "1.0", File: "bmc.bin"},
			wantField:   "components.bmc.strategy",
			wantMessage: "strategy is required",
		},
		{
			name:        "unknown component strategy",
			component:   ComponentDef{Version: "1.0", File: "bmc.bin", Strategy: "unsupported"},
			wantField:   "components.bmc.strategy",
			wantMessage: "must be 'redfish', 'ssh', or 'script'",
		},
		{
			name:        "script requires path",
			component:   ComponentDef{Version: "1.0", File: "bmc.bin", Strategy: "script"},
			wantField:   "components.bmc.script",
			wantMessage: "script path is required when strategy is 'script'",
		},
		{
			name: "script rejects unknown argument token",
			component: ComponentDef{
				Version: "1.0", File: "bmc.bin", Strategy: "script", Script: "update.sh",
				ScriptArgs: []string{"bmc_ip", "fw_file", "unknown_token"},
			},
			wantField:   "components.bmc.script_args",
			wantMessage: "invalid token 'unknown_token'",
		},
		{
			name: "script accepts credentials and firmware tokens",
			component: ComponentDef{
				Version: "1.0", File: "bmc.bin", Strategy: "script", Script: "update.sh",
				ScriptArgs: []string{
					"bmc_ip", "bmc_user", "bmc_password",
					"nvos_ip", "nvos_user", "nvos_password", "fw_file",
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pkg := FirmwarePackage{
				Version:        "bundle-1.0",
				ComponentOrder: []string{"bmc"},
				Components:     map[string]ComponentDef{"bmc": tt.component},
			}
			if tt.mutatePackage != nil {
				tt.mutatePackage(&pkg)
			}

			err := pkg.Validate()
			if tt.wantField == "" {
				require.NoError(t, err)
				return
			}

			var validationErr *ValidationError
			require.ErrorAs(t, err, &validationErr)
			assert.Equal(t, tt.wantField, validationErr.Field)
			assert.Contains(t, validationErr.Message, tt.wantMessage)
		})
	}
}
