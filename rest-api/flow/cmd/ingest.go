// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"

	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
)

var (
	ingestCmd = &cobra.Command{
		Use:   "ingest",
		Short: "Ingest rack components to component manager services",
		Long: `Inject expected component configurations to their respective component manager services.

Components are routed to the appropriate service based on type. Today every
component type is dispatched through Core (NICo); Core's per-tray-type backends
(NSM, PSM, RMS, ...) handle the underlying hardware registration.

Specify racks by ID or name:
  --rack-ids   : Comma-separated list of rack UUIDs
  --rack-names : Comma-separated list of rack names

Examples:
  # Ingest all components in racks by name
  flow ingest --rack-names "rack-1,rack-2"

  # Ingest by rack IDs
  flow ingest --rack-ids "uuid1,uuid2"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doIngest()
		},
	}

	ingestRackIDs     string
	ingestRackNames   string
	ingestDescription string
)

func init() {
	rootCmd.AddCommand(ingestCmd)

	ingestCmd.Flags().StringVar(&ingestRackIDs, "rack-ids", "", "Comma-separated list of rack UUIDs")
	ingestCmd.Flags().StringVar(&ingestRackNames, "rack-names", "", "Comma-separated list of rack names")
	ingestCmd.Flags().StringVar(&ingestDescription, "description", "", "Task description")
}

// doIngest validates the CLI inputs and calls IngestRackByRackIDs or
// IngestRackByRackNames via the gRPC client, logging the submitted task IDs.
func doIngest() {
	hasRackIDs := ingestRackIDs != ""
	hasRackNames := ingestRackNames != ""

	if !hasRackIDs && !hasRackNames {
		log.Fatal().Msg("One of --rack-ids or --rack-names must be specified")
	}
	if hasRackIDs && hasRackNames {
		log.Fatal().Msg("Only one of --rack-ids or --rack-names can be specified")
	}

	ctx := context.Background()

	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Msgf("Failed to create Flow client: %v", err)
	}
	defer flowClient.Close()

	var result *client.IngestRackResult

	switch {
	case hasRackIDs:
		rackIDs := parseUUIDList(ingestRackIDs)
		if len(rackIDs) == 0 {
			log.Fatal().Msg("No valid rack IDs provided")
		}
		log.Info().
			Int("rack_count", len(rackIDs)).
			Msg("Submitting ingestion task by rack IDs")
		result, err = flowClient.IngestRackByRackIDs(ctx, rackIDs, ingestDescription)

	case hasRackNames:
		rackNames := parseCommaSeparatedList(ingestRackNames)
		if len(rackNames) == 0 {
			log.Fatal().Msg("No valid rack names provided")
		}
		log.Info().
			Strs("rack_names", rackNames).
			Msg("Submitting ingestion task by rack names")
		result, err = flowClient.IngestRackByRackNames(ctx, rackNames, ingestDescription)
	}

	if err != nil {
		log.Fatal().Err(err).Msg("Failed to submit ingestion task")
	}

	taskIDStrs := make([]string, 0, len(result.TaskIDs))
	for _, id := range result.TaskIDs {
		taskIDStrs = append(taskIDStrs, id.String())
	}

	log.Info().
		Strs("task_ids", taskIDStrs).
		Int("task_count", len(result.TaskIDs)).
		Msg("Ingestion tasks submitted successfully")
}
