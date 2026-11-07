// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// rackCmd is the parent command for rack inventory subcommands.
var rackCmd = &cobra.Command{
	Use:   "rack",
	Short: "Rack operations",
	Long:  `Commands for managing racks in the inventory.`,
}

func init() {
	rootCmd.AddCommand(rackCmd)
}
