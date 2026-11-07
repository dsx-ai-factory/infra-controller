// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"strings"

	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var (
	powerControlCmd = &cobra.Command{
		Use:   "control",
		Short: "Control power state of components in racks",
		Long: `Control power state of components via nico API.
		
Specify exactly ONE of the following options:
  --rack-ids      : Comma-separated list of rack UUIDs
  --rack-names    : Comma-separated list of rack names
  --component-ids : Comma-separated list of component IDs (e.g. machine_id from NICo)

Component types (required for rack-ids/rack-names):
  --type compute     : Compute nodes
  --type nvswitch   : NVSwitches
  --type powershelf  : Power shelves

Power operations:
  Power On:
    --op on           : Power on
    --op force-on     : Force power on
  Power Off:
    --op off          : Graceful shutdown
    --op force-off    : Force power off
  Restart (OS level):
    --op restart      : Graceful restart
    --op force-restart: Force restart
  Reset (hardware level):
    --op warm-reset   : Warm reset (keep memory)
    --op cold-reset   : Cold reset (full power cycle)

Examples:
  # Power on compute nodes by rack names
  flow power control --rack-names "rack-name-1,rack-name-2" --type compute --op on

  # Graceful shutdown
  flow power control --rack-names "rack-name-1" --type compute --op off

  # Force power off
  flow power control --rack-names "rack-name-1" --type compute --op force-off

  # Cold reset (hardware level)
  flow power control --rack-names "rack-name-1" --type compute --op cold-reset

  # Power control by component IDs (no --type needed)
  flow power control --component-ids "machine1,machine2" --op restart
`,
		Run: func(cmd *cobra.Command, args []string) {
			doPowerControl()
		},
	}

	powerControlRackIDs       string
	powerControlRackNames     string
	powerControlComponentIDs  string
	powerControlComponentType string
	powerControlOp            string
)

func init() {
	powerCmd.AddCommand(powerControlCmd)

	powerControlCmd.Flags().StringVar(&powerControlRackIDs, "rack-ids", "", "Comma-separated list of rack UUIDs")
	powerControlCmd.Flags().StringVar(&powerControlRackNames, "rack-names", "", "Comma-separated list of rack names")
	powerControlCmd.Flags().StringVar(&powerControlComponentIDs, "component-ids", "", "Comma-separated list of component IDs")
	powerControlCmd.Flags().StringVarP(&powerControlComponentType, "type", "t", "", "Component type: compute, nvswitch, powershelf (required for rack-ids/rack-names)")
	powerControlCmd.Flags().StringVar(&powerControlOp, "op", "", "Power operation: on, off, force-off, reset, force-reset, ac-powercycle")

	powerControlCmd.MarkFlagRequired("op") //nolint
}

// parsePowerOpToTypes converts a power operation string (e.g. "on", "force-off",
// "cold-reset") to types.PowerControlOp. Returns an empty string for unrecognised values.
func parsePowerOpToTypes(op string) types.PowerControlOp {
	switch strings.ToLower(op) {
	// Power On
	case "on":
		return types.PowerControlOpOn
	case "force-on", "forceon":
		return types.PowerControlOpForceOn
	// Power Off
	case "off":
		return types.PowerControlOpOff
	case "force-off", "forceoff":
		return types.PowerControlOpForceOff
	// Restart (OS level)
	case "restart":
		return types.PowerControlOpRestart
	case "force-restart", "forcerestart":
		return types.PowerControlOpForceRestart
	// Reset (hardware level)
	case "warm-reset", "warmreset":
		return types.PowerControlOpWarmReset
	case "cold-reset", "coldreset":
		return types.PowerControlOpColdReset
	default:
		return ""
	}
}

// doPowerControl validates the CLI inputs and calls the appropriate
// PowerControl client method based on whether the caller specified rack IDs,
// rack names, or component IDs.
func doPowerControl() {
	// Validate inputs - only one of the options can be specified
	hasRackIDs := powerControlRackIDs != ""
	hasRackNames := powerControlRackNames != ""
	hasComponentIDs := powerControlComponentIDs != ""

	optionCount := 0
	if hasRackIDs {
		optionCount++
	}
	if hasRackNames {
		optionCount++
	}
	if hasComponentIDs {
		optionCount++
	}

	if optionCount == 0 {
		log.Fatal().Msg("One of --rack-ids, --rack-names, or --component-ids must be specified")
	}

	if optionCount > 1 {
		log.Fatal().Msg("Only one of --rack-ids, --rack-names, or --component-ids can be specified")
	}

	// Parse and validate component type (required for rack-ids/rack-names)
	componentType := parseComponentTypeToTypes(powerControlComponentType)
	if (hasRackIDs || hasRackNames) && componentType == types.ComponentTypeUnknown {
		log.Fatal().Msg("--type is required when using --rack-ids or --rack-names (compute, nvswitch, powershelf)")
	}

	// Parse power operation
	op := parsePowerOpToTypes(powerControlOp)
	if op == "" {
		log.Fatal().Msgf("Invalid power operation: %s. Valid options: on, force-on, off, force-off, restart, force-restart, warm-reset, cold-reset", powerControlOp)
	}

	ctx := context.Background()

	// Create Flow client
	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Msgf("Failed to create Flow client: %v", err)
	}
	defer flowClient.Close()

	// Execute based on the specified option
	var result *client.PowerControlResult

	switch {
	case hasRackIDs:
		rackIDs := parseUUIDList(powerControlRackIDs)
		if len(rackIDs) == 0 {
			log.Fatal().Msg("No valid rack IDs provided")
		}
		log.Info().
			Int("rack_count", len(rackIDs)).
			Str("component_type", powerControlComponentType).
			Str("operation", powerControlOp).
			Msg("Executing power control by rack IDs")
		result, err = flowClient.PowerControlByRackIDs(ctx, rackIDs, componentType, op)

	case hasRackNames:
		rackNames := parseCommaSeparatedList(powerControlRackNames)
		if len(rackNames) == 0 {
			log.Fatal().Msg("No valid rack names provided")
		}
		log.Info().
			Strs("rack_names", rackNames).
			Str("component_type", powerControlComponentType).
			Str("operation", powerControlOp).
			Msg("Executing power control by rack names")
		result, err = flowClient.PowerControlByRackNames(ctx, rackNames, componentType, op)

	case hasComponentIDs:
		componentIDs := parseCommaSeparatedList(powerControlComponentIDs)
		if len(componentIDs) == 0 {
			log.Fatal().Msg("No valid component IDs provided")
		}
		log.Info().
			Strs("component_ids", componentIDs).
			Str("operation", powerControlOp).
			Msg("Executing power control by component IDs")
		result, err = flowClient.PowerControlByMachineIDs(ctx, componentIDs, op)
	}

	if err != nil {
		log.Fatal().Err(err).Msg("Failed to execute power control")
	}

	// Log results
	taskIDStrs := make([]string, 0, len(result.TaskIDs))
	for _, id := range result.TaskIDs {
		taskIDStrs = append(taskIDStrs, id.String())
	}

	log.Info().
		Strs("task_ids", taskIDStrs).
		Int("task_count", len(result.TaskIDs)).
		Msg("Power control tasks submitted successfully")
}
