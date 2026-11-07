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

var purgeRackID string

func newRackPurgeCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "purge",
		Short: "Permanently remove a soft-deleted rack",
		Long: `Permanently remove a soft-deleted rack and all its components from the database.
The rack must have been soft-deleted first via "flow rack delete".

Required:
  --id : Rack UUID to purge

Examples:
  flow rack purge --id "rack-uuid"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doPurgeRack()
		},
	}

	cmd.Flags().StringVar(&purgeRackID, "id", "", "Rack UUID (required)")
	_ = cmd.MarkFlagRequired("id")

	return cmd
}

func init() {
	rackCmd.AddCommand(newRackPurgeCmd())
}

func doPurgeRack() {
	rackID, err := uuid.Parse(purgeRackID)
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid rack UUID")
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	if err := c.PurgeRack(context.Background(), rackID); err != nil {
		log.Fatal().Err(err).Msg("Failed to purge rack")
	}

	fmt.Printf("Rack %s purged successfully.\n", rackID)
}
