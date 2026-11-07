// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
)

var (
	expectedCmd = &cobra.Command{
		Use:   "expected",
		Short: "Get expected components from local database",
		Long: `Get expected components from local database.
		
Specify exactly ONE of the following options:
  --rack-ids      : Comma-separated list of rack UUIDs
  --rack-names    : Comma-separated list of rack names
  --component-ids : Comma-separated list of component IDs (e.g. machine_id from NICo)

Component types (required for rack-ids/rack-names):
  --type compute     : Compute nodes
  --type nvswitch   : NVSwitches  
  --type powershelf  : Power shelves
  --type torswitch   : ToR switches
  --type ums         : UMS
  --type cdu         : CDU

Output formats:
  --output json      : JSON format (default)
  --output table     : Table format

Examples:
  # Get all compute components from racks by name
  flow component expected --rack-names "rack-1,rack-2" --type compute

  # Get NVSwitches from rack by ID
  flow component expected --rack-ids "uuid-1,uuid-2" --type nvswitch

  # Get components by component IDs
  flow component expected --component-ids "machine-1,machine-2"

  # Output as table
  flow component expected --rack-names "rack-1" --type compute --output table
`,
		Run: func(cmd *cobra.Command, args []string) {
			doGetExpectedComponents()
		},
	}

	expectedRackIDs       string
	expectedRackNames     string
	expectedComponentIDs  string
	expectedComponentType string
	expectedOutput        string
)

func init() {
	componentCmd.AddCommand(expectedCmd)

	expectedCmd.Flags().StringVar(&expectedRackIDs, "rack-ids", "", "Comma-separated list of rack UUIDs")
	expectedCmd.Flags().StringVar(&expectedRackNames, "rack-names", "", "Comma-separated list of rack names")
	expectedCmd.Flags().StringVar(&expectedComponentIDs, "component-ids", "", "Comma-separated list of component IDs")
	expectedCmd.Flags().StringVarP(&expectedComponentType, "type", "t", "", "Component type: compute, nvswitch, powershelf, torswitch, ums, cdu")
	expectedCmd.Flags().StringVarP(&expectedOutput, "output", "o", "json", "Output format: json, table")
}

// doGetExpectedComponents validates the CLI inputs, calls the appropriate
// GetExpectedComponents client method, and prints the result in the requested
// output format.
func doGetExpectedComponents() {
	// Validate input - exactly one of rack-ids, rack-names, or component-ids must be provided
	optionCount := 0
	if expectedRackIDs != "" {
		optionCount++
	}
	if expectedRackNames != "" {
		optionCount++
	}
	if expectedComponentIDs != "" {
		optionCount++
	}

	if optionCount == 0 {
		log.Fatal().Msg("One of --rack-ids, --rack-names, or --component-ids must be specified")
	}
	if optionCount > 1 {
		log.Fatal().Msg("Only one of --rack-ids, --rack-names, or --component-ids can be specified")
	}

	// Validate component type for rack-based queries
	if (expectedRackIDs != "" || expectedRackNames != "") && expectedComponentType == "" {
		log.Fatal().Msg("--type is required when using --rack-ids or --rack-names")
	}

	// Parse component type
	componentType := parseComponentTypeToTypes(expectedComponentType)

	// Create client
	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx := context.Background()
	var result *client.GetExpectedComponentsResult

	// Call the appropriate client method based on input
	if expectedRackIDs != "" {
		rackIDStrs := strings.Split(expectedRackIDs, ",")
		rackIDs := make([]uuid.UUID, 0, len(rackIDStrs))
		for _, idStr := range rackIDStrs {
			id, err := uuid.Parse(strings.TrimSpace(idStr))
			if err != nil {
				log.Fatal().Err(err).Str("id", idStr).Msg("Invalid rack UUID")
			}
			rackIDs = append(rackIDs, id)
		}
		result, err = c.GetExpectedComponentsByRackIDs(ctx, rackIDs, componentType)
	} else if expectedRackNames != "" {
		rackNames := strings.Split(expectedRackNames, ",")
		for i := range rackNames {
			rackNames[i] = strings.TrimSpace(rackNames[i])
		}
		result, err = c.GetExpectedComponentsByRackNames(ctx, rackNames, componentType)
	} else if expectedComponentIDs != "" {
		componentIDs := strings.Split(expectedComponentIDs, ",")
		for i := range componentIDs {
			componentIDs[i] = strings.TrimSpace(componentIDs[i])
		}
		result, err = c.GetExpectedComponentsByComponentIDs(ctx, componentIDs, componentType)
	}

	if err != nil {
		log.Fatal().Err(err).Msg("Failed to get components")
	}

	// Output results
	switch expectedOutput {
	case "json":
		outputExpectedJSON(result)
	case "table":
		outputExpectedTable(result)
	default:
		log.Fatal().Str("format", expectedOutput).Msg("Unknown output format")
	}
}

// outputExpectedJSON prints the GetExpectedComponentsResult as indented JSON to stdout.
func outputExpectedJSON(result *client.GetExpectedComponentsResult) {
	output := struct {
		Total      int         `json:"total"`
		Components interface{} `json:"components"`
	}{
		Total:      result.Total,
		Components: result.Components,
	}

	data, err := json.MarshalIndent(output, "", "  ")
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal JSON")
	}
	fmt.Println(string(data))
}

// outputExpectedTable prints the GetExpectedComponentsResult as a human-readable
// table to stdout.
func outputExpectedTable(result *client.GetExpectedComponentsResult) {
	fmt.Printf("Total: %d components\n", result.Total)
	fmt.Println(strings.Repeat("-", 100))
	fmt.Printf("%-40s %-15s %-30s %-15s\n", "ID", "TYPE", "NAME", "MACHINE_ID")
	fmt.Println(strings.Repeat("-", 100))

	for _, comp := range result.Components {
		name := comp.Info.Name
		id := comp.Info.ID.String()
		fmt.Printf("%-40s %-15s %-30s %-15s\n",
			id,
			string(comp.Type),
			name,
			comp.ComponentID,
		)
	}
}
