// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
)

var (
	rackCreateFile string
	rackCreateJSON string
)

// newRackCreateCmd returns a configured cobra.Command for creating a new
// expected rack from a JSON file or inline JSON string.
func newRackCreateCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "create",
		Short: "Create a new rack",
		Long: `Create a new expected rack from a JSON file or inline JSON string.

Exactly one of --file or --json must be provided.

Examples:
  # Create from inline JSON
  flow rack create --json '{
    "info": {"name": "R1", "manufacturer": "NVIDIA", "serial_number": "SN001"},
    "location": {"region": "us-east", "datacenter": "DC1", "room": "A", "position": "1"}
  }'

  # Create from file
  flow rack create --file /path/to/rack.json
`,
		Run: func(cmd *cobra.Command, args []string) {
			doCreateRack()
		},
	}

	cmd.Flags().StringVarP(
		&rackCreateFile, "file", "f", "",
		"Path to JSON file containing rack definition",
	)
	cmd.Flags().StringVarP(
		&rackCreateJSON, "json", "j", "",
		"Inline JSON string containing rack definition",
	)
	cmd.MarkFlagsOneRequired("file", "json")
	cmd.MarkFlagsMutuallyExclusive("file", "json")

	return cmd
}

func init() {
	rackCmd.AddCommand(newRackCreateCmd())
}

// doCreateRack reads the rack JSON input, parses it, and calls CreateExpectedRack
// via the gRPC client, printing the newly created rack's UUID on success.
func doCreateRack() {
	data, err := readRackJSONData(rackCreateFile, rackCreateJSON)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to read input")
	}

	rack, err := parseRackJSON(data)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to parse rack JSON")
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx, cancel := newCLIContext(30 * time.Second)
	defer cancel()

	id, err := c.CreateExpectedRack(ctx, rack)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create rack")
	}

	out, err := json.Marshal(map[string]string{"id": id.String()})
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal response")
	}
	fmt.Println(string(out))
}
