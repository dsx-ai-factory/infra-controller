// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// inventoryCmd is the parent command for rack inventory management subcommands.
var inventoryCmd = &cobra.Command{
	Use:   "inventory",
	Short: "Inventory management",
	Long:  `Commands for managing rack inventory.`,
}

func init() {
	rootCmd.AddCommand(inventoryCmd)
}
