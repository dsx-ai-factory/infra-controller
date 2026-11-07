// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"github.com/spf13/cobra"
)

// firmwareCmd is the parent command for firmware management subcommands.
var firmwareCmd = &cobra.Command{
	Use:   "firmware",
	Short: "Firmware operations",
	Long:  `Commands for firmware management including version checking, updates, and scheduling.`,
}

func init() {
	rootCmd.AddCommand(firmwareCmd)
}
