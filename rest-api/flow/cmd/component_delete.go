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

var (
	deleteComponentID string
)

// newDeleteCmd returns a configured cobra.Command for soft-deleting a component
// from the inventory by UUID.
func newDeleteCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "delete",
		Short: "Delete a component",
		Long: `Soft-delete a component from the inventory table.

Required:
  --id : Component UUID to delete

Examples:
  flow component delete --id "component-uuid"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doDeleteComponent()
		},
	}

	cmd.Flags().StringVar(&deleteComponentID, "id", "", "Component UUID (required)")

	_ = cmd.MarkFlagRequired("id")

	return cmd
}

func init() {
	componentCmd.AddCommand(newDeleteCmd())
}

// doDeleteComponent parses the component UUID from the flag and calls
// DeleteComponent via the gRPC client.
func doDeleteComponent() {
	compID, err := uuid.Parse(deleteComponentID)
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid component UUID")
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx := context.Background()
	if err := c.DeleteComponent(ctx, compID); err != nil {
		log.Fatal().Err(err).Msg("Failed to delete component")
	}

	fmt.Printf("Component %s deleted successfully.\n", compID)
}
