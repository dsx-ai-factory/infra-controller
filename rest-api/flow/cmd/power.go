// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// powerCmd is the parent command for power management subcommands.
var powerCmd = &cobra.Command{
	Use:   "power",
	Short: "Power operations",
	Long:  `Commands for power management including control, status, and statistics.`,
}

func init() {
	rootCmd.AddCommand(powerCmd)
}
