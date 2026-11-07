// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"strings"

	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// componentCmd is the parent command for component operation subcommands.
var componentCmd = &cobra.Command{
	Use:   "component",
	Short: "Component operations",
	Long:  `Commands for querying and comparing components (expected vs actual).`,
}

func init() {
	rootCmd.AddCommand(componentCmd)
}

// parseComponentTypeToTypes converts string to types.ComponentType
func parseComponentTypeToTypes(s string) types.ComponentType {
	switch strings.ToLower(s) {
	case "compute":
		return types.ComponentTypeCompute
	case "nvswitch", "nvl-switch":
		return types.ComponentTypeNVSwitch
	case "powershelf", "power-shelf":
		return types.ComponentTypePowerShelf
	case "torswitch", "tor-switch":
		return types.ComponentTypeTORSwitch
	case "ums":
		return types.ComponentTypeUMS
	case "cdu":
		return types.ComponentTypeCDU
	default:
		return types.ComponentTypeUnknown
	}
}
