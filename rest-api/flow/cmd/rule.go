// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// ruleCmd is the parent command for operation rule management subcommands.
var ruleCmd = &cobra.Command{
	Use:   "rule",
	Short: "Operation rule management",
	Long:  `Commands for managing operation rules that control workflow execution behavior.`,
}

func init() {
	rootCmd.AddCommand(ruleCmd)
}
