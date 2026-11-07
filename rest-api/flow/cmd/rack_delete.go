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

var deleteRackID string

func newRackDeleteCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "delete",
		Short: "Soft-delete a rack",
		Long: `Soft-delete a rack and all its components.

Required:
  --id : Rack UUID to delete

Examples:
  flow rack delete --id "rack-uuid"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doDeleteRack()
		},
	}

	cmd.Flags().StringVar(&deleteRackID, "id", "", "Rack UUID (required)")
	_ = cmd.MarkFlagRequired("id")

	return cmd
}

func init() {
	rackCmd.AddCommand(newRackDeleteCmd())
}

func doDeleteRack() {
	rackID, err := uuid.Parse(deleteRackID)
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid rack UUID")
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	if err := c.DeleteRack(context.Background(), rackID); err != nil {
		log.Fatal().Err(err).Msg("Failed to delete rack")
	}

	fmt.Printf("Rack %s deleted successfully.\n", rackID)
}
