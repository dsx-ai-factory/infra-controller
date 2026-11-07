// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// dbCmd is the parent command for database management subcommands.
var dbCmd = &cobra.Command{
	Use:   "db",
	Short: "Database operations",
	Long:  `Commands for database management such as migrations.`,
}

func init() {
	rootCmd.AddCommand(dbCmd)
}
