// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
)

var purgeComponentID string

func newComponentPurgeCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "purge",
		Short: "Permanently remove a soft-deleted component",
		Long: `Permanently remove a soft-deleted component from the database.
The component must have been soft-deleted first via "flow component delete".

Required:
  --id : Component UUID to purge

Examples:
  flow component purge --id "component-uuid"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doPurgeComponent()
		},
	}

	cmd.Flags().StringVar(&purgeComponentID, "id", "", "Component UUID (required)")
	_ = cmd.MarkFlagRequired("id")

	return cmd
}

func init() {
	componentCmd.AddCommand(newComponentPurgeCmd())
}

func doPurgeComponent() {
	compID, err := uuid.Parse(purgeComponentID)
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid component UUID")
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	if err := c.PurgeComponent(context.Background(), compID); err != nil {
		log.Fatal().Err(err).Msg("Failed to purge component")
	}

	fmt.Printf("Component %s purged successfully.\n", compID)
}
