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
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var (
	diffCmd = &cobra.Command{
		Use:   "diff",
		Short: "Compare expected (local DB) vs actual (source system) components",
		Long: `Compare expected components from local database against actual components from source systems.

Each component type queries Core (NICo); Core fans out to the per-tray-type
backend (NSM, PSM, RMS, ...) under the hood. Currently only supports Compute
component type.

Specify exactly ONE of the following options:
  --rack-ids      : Comma-separated list of rack UUIDs
  --rack-names    : Comma-separated list of rack names
  --component-ids : Comma-separated list of component IDs

Component types (required):
  --type compute     : Compute nodes (currently the only supported type)

Output formats:
  --output json      : JSON format (default)
  --output table     : Table format

Examples:
  # Compare compute components from racks by name
  flow component diff --rack-names "rack-1,rack-2" --type compute

  # Compare components from rack by ID
  flow component diff --rack-ids "uuid-1,uuid-2" --type compute

  # Compare by component IDs
  flow component diff --component-ids "machine-1,machine-2" --type compute

  # Output as table
  flow component diff --rack-names "rack-1" --type compute --output table
`,
		Run: func(cmd *cobra.Command, args []string) {
			doDiffComponents()
		},
	}

	diffRackIDs       string
	diffRackNames     string
	diffComponentIDs  string
	diffComponentType string
	diffOutput        string
)

func init() {
	componentCmd.AddCommand(diffCmd)

	diffCmd.Flags().StringVar(&diffRackIDs, "rack-ids", "", "Comma-separated list of rack UUIDs")
	diffCmd.Flags().StringVar(&diffRackNames, "rack-names", "", "Comma-separated list of rack names")
	diffCmd.Flags().StringVar(&diffComponentIDs, "component-ids", "", "Comma-separated list of component IDs")
	diffCmd.Flags().StringVarP(&diffComponentType, "type", "t", "", "Component type (required): compute")
	diffCmd.Flags().StringVarP(&diffOutput, "output", "o", "json", "Output format: json, table")
}

// doDiffComponents validates the CLI inputs, calls the appropriate
// ValidateComponents client method, and prints the result in the requested
// output format.
func doDiffComponents() {
	// Validate input - exactly one of rack-ids, rack-names, or component-ids must be provided
	optionCount := 0
	if diffRackIDs != "" {
		optionCount++
	}
	if diffRackNames != "" {
		optionCount++
	}
	if diffComponentIDs != "" {
		optionCount++
	}

	if optionCount == 0 {
		log.Fatal().Msg("One of --rack-ids, --rack-names, or --component-ids must be specified")
	}
	if optionCount > 1 {
		log.Fatal().Msg("Only one of --rack-ids, --rack-names, or --component-ids can be specified")
	}

	// Component type is required
	if diffComponentType == "" {
		log.Fatal().Msg("--type is required (currently only 'compute' is supported)")
	}

	// Parse and validate component type
	componentType := parseComponentTypeToTypes(diffComponentType)
	if componentType != types.ComponentTypeCompute {
		log.Fatal().Msg("Only 'compute' component type is supported for diff")
	}

	// Create client
	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx := context.Background()
	var result *client.ValidateComponentsResult

	// Call the appropriate client method based on input
	if diffRackIDs != "" {
		rackIDStrs := strings.Split(diffRackIDs, ",")
		rackIDs := make([]uuid.UUID, 0, len(rackIDStrs))
		for _, idStr := range rackIDStrs {
			id, err := uuid.Parse(strings.TrimSpace(idStr))
			if err != nil {
				log.Fatal().Err(err).Str("id", idStr).Msg("Invalid rack UUID")
			}
			rackIDs = append(rackIDs, id)
		}
		result, err = c.ValidateComponentsByRackIDs(ctx, rackIDs, componentType)
	} else if diffRackNames != "" {
		rackNames := strings.Split(diffRackNames, ",")
		for i := range rackNames {
			rackNames[i] = strings.TrimSpace(rackNames[i])
		}
		result, err = c.ValidateComponentsByRackNames(ctx, rackNames, componentType)
	} else if diffComponentIDs != "" {
		componentIDs := strings.Split(diffComponentIDs, ",")
		for i := range componentIDs {
			componentIDs[i] = strings.TrimSpace(componentIDs[i])
		}
		result, err = c.ValidateComponentsByComponentIDs(ctx, componentIDs, componentType)
	}

	if err != nil {
		log.Fatal().Err(err).Msg("Failed to compare components")
	}

	// Output results
	switch diffOutput {
	case "json":
		outputDiffJSON(result)
	case "table":
		outputDiffTable(result)
	default:
		log.Fatal().Str("format", diffOutput).Msg("Unknown output format")
	}
}

// outputDiffJSON prints the ValidateComponentsResult as indented JSON to stdout.
func outputDiffJSON(result *client.ValidateComponentsResult) {
	output := struct {
		TotalDiffs      int                    `json:"total_diffs"`
		MissingCount    int                    `json:"missing_count"`
		UnexpectedCount int                    `json:"unexpected_count"`
		MismatchCount   int                    `json:"mismatch_count"`
		MatchCount      int                    `json:"match_count"`
		Diffs           []*types.ComponentDiff `json:"diffs"`
	}{
		TotalDiffs:      result.TotalDiffs,
		MissingCount:    result.MissingCount,
		UnexpectedCount: result.UnexpectedCount,
		MismatchCount:   result.MismatchCount,
		MatchCount:      result.MatchCount,
		Diffs:           result.Diffs,
	}

	data, err := json.MarshalIndent(output, "", "  ")
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal JSON")
	}
	fmt.Println(string(data))
}

// outputDiffTable prints the ValidateComponentsResult as a human-readable
// table with a summary header and per-diff rows to stdout.
func outputDiffTable(result *client.ValidateComponentsResult) {
	// Summary
	fmt.Println("Summary:")
	fmt.Printf("  Total compared: %d\n", result.TotalDiffs+result.MatchCount)
	fmt.Printf("  - Match: %d\n", result.MatchCount)
	fmt.Printf("  - Missing (expected but not in source): %d\n", result.MissingCount)
	fmt.Printf("  - Unexpected (in source but not expected): %d\n", result.UnexpectedCount)
	fmt.Printf("  - Mismatch (field differences): %d\n", result.MismatchCount)
	fmt.Println()

	if len(result.Diffs) == 0 {
		fmt.Println("No differences found.")
		return
	}

	// Differences table
	fmt.Println("Differences:")
	fmt.Println(strings.Repeat("-", 130))
	fmt.Printf("%-15s %-38s %-30s %s\n", "TYPE", "UUID", "COMPONENT_ID", "DETAILS")
	fmt.Println(strings.Repeat("-", 130))

	for _, diff := range result.Diffs {
		diffType := ""
		details := ""

		switch diff.Type {
		case types.DiffTypeMissing:
			diffType = "Missing"
			details = "Expected but not found in source system"
		case types.DiffTypeUnexpected:
			diffType = "Unexpected"
			details = "Found in source system but not expected"
		case types.DiffTypeMismatch:
			diffType = "Mismatch"
			var fieldStrs []string
			for _, fd := range diff.FieldDiffs {
				fieldStrs = append(fieldStrs, fmt.Sprintf("%s: %s → %s",
					fd.FieldName, fd.ExpectedValue, fd.ActualValue))
			}
			details = strings.Join(fieldStrs, ", ")
		}

		uuidStr := ""
		if diff.ID != uuid.Nil {
			uuidStr = diff.ID.String()
		}
		fmt.Printf("%-15s %-38s %-30s %s\n", diffType, uuidStr, diff.ComponentID, details)
	}
	fmt.Println(strings.Repeat("-", 130))
}
