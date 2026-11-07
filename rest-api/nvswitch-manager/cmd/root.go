// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"os"

	"github.com/spf13/cobra"
)

// rootCmd represents the base command when called without any subcommands
var rootCmd = &cobra.Command{
	Use:   "nvswitch-manager",
	Short: "NV-Switch Manager - Manage NVIDIA DGX GB200 NVLink Switch Trays",
	Long: `NV-Switch Manager is a service for managing NVIDIA DGX GB200 NVLink Switch Trays.

It provides gRPC APIs for:
  - Registering and managing NV-Switch trays
  - Firmware updates for FIRMWARE, CPLD, and NVOS components
  - Power cycle operations
  - Inventory management`,
}

// Execute adds all child commands to the root command and sets flags appropriately.
// This is called by main.main(). It only needs to happen once to the rootCmd.
func Execute() {
	err := rootCmd.Execute()
	if err != nil {
		os.Exit(1)
	}
}

func init() {
	rootCmd.Flags().BoolP("toggle", "t", false, "Help message for toggle")
}
