// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

const (
	defaultWindowDuration = 24 * time.Hour
)

var (
	firmwareUpgradeCmd = &cobra.Command{
		Use:   "upgrade",
		Short: "Upgrade firmware for machines",
		Long: `Upgrade firmware for machines via nico API.
		
Specify exactly ONE of the following options:
  --rack-ids      : Comma-separated list of rack UUIDs
  --rack-names    : Comma-separated list of rack names
  --component-ids : Comma-separated list of component IDs (e.g. machine_id from NICo)

If racks are specified, component IDs will be retrieved from the database.
If --start and --end are not specified, defaults to now + 24 hours.

Time formats supported:
  - 2025-01-02T03:04:05+0000 (with timezone offset, no colon)
  - 2025-01-02T03:04:05 (no timezone, treated as local time)

Examples:
  # Upgrade by rack IDs
  flow firmware upgrade --rack-ids "uuid1,uuid2" --type compute

  # Upgrade by rack names
  flow firmware upgrade --rack-names "rack-name-1,rack-name-2" --type compute

  # Upgrade by component IDs
  flow firmware upgrade --component-ids "machine1,machine2"

  # Upgrade with explicit time window
  flow firmware upgrade --rack-names "rack-name-1" --type compute --start "2025-01-02T03:04:05" --end "2025-01-02T06:04:05"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doFirmwareUpgrade()
		},
	}

	firmwareUpgradeRackIDs       string
	firmwareUpgradeRackNames     string
	firmwareUpgradeComponentIDs  string
	firmwareUpgradeComponentType string
	firmwareUpgradeStartTime     string
	firmwareUpgradeEndTime       string
)

func init() {
	firmwareCmd.AddCommand(firmwareUpgradeCmd)

	firmwareUpgradeCmd.Flags().StringVar(&firmwareUpgradeRackIDs, "rack-ids", "", "Comma-separated list of rack UUIDs")
	firmwareUpgradeCmd.Flags().StringVar(&firmwareUpgradeRackNames, "rack-names", "", "Comma-separated list of rack names")
	firmwareUpgradeCmd.Flags().StringVar(&firmwareUpgradeComponentIDs, "component-ids", "", "Comma-separated list of component IDs")
	firmwareUpgradeCmd.Flags().StringVarP(&firmwareUpgradeComponentType, "type", "t", "", "Component type: compute, nvswitch, powershelf (required for rack-ids/rack-names)")
	firmwareUpgradeCmd.Flags().StringVarP(&firmwareUpgradeStartTime, "start", "s", "", "Start time (default: now)")
	firmwareUpgradeCmd.Flags().StringVarP(&firmwareUpgradeEndTime, "end", "e", "", "End time (default: start + 24h)")
}

// parseTimeString parses time string in the following formats:
// - 2025-01-02T03:04:05+0000 (with timezone offset, no colon)
// - 2025-01-02T03:04:05 (no timezone, treated as local time)
func parseTimeString(s string) (time.Time, error) {
	// Try format with timezone offset (no colon)
	t, err := time.Parse("2006-01-02T15:04:05-0700", s)
	if err == nil {
		return t, nil
	}

	// Try format without timezone (local time)
	t, err = time.ParseInLocation("2006-01-02T15:04:05", s, time.Local)
	if err == nil {
		return t, nil
	}

	return time.Time{}, fmt.Errorf("unable to parse time '%s': expected format 2025-01-02T03:04:05 or 2025-01-02T03:04:05+0000", s)
}

// doFirmwareUpgrade validates the CLI inputs, resolves the upgrade time window,
// and calls the appropriate UpgradeFirmware client method based on whether the
// caller specified rack IDs, rack names, or component IDs.
func doFirmwareUpgrade() {
	// Validate inputs - only one of the three options can be specified
	hasRackIDs := firmwareUpgradeRackIDs != ""
	hasRackNames := firmwareUpgradeRackNames != ""
	hasComponentIDs := firmwareUpgradeComponentIDs != ""

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
	componentType := parseComponentTypeToTypes(firmwareUpgradeComponentType)
	if (hasRackIDs || hasRackNames) && componentType == types.ComponentTypeUnknown {
		log.Fatal().Msg("--type is required when using --rack-ids or --rack-names (compute, nvswitch, powershelf)")
	}

	// Parse time strings with defaults
	var startTime, endTime time.Time
	var err error

	if firmwareUpgradeStartTime == "" {
		// Default: now
		startTime = time.Now()
	} else {
		startTime, err = parseTimeString(firmwareUpgradeStartTime)
		if err != nil {
			log.Fatal().Msgf("Invalid start time: %v", err)
		}
	}

	if firmwareUpgradeEndTime == "" {
		// Default: start + 24 hours
		endTime = startTime.Add(defaultWindowDuration)
	} else {
		endTime, err = parseTimeString(firmwareUpgradeEndTime)
		if err != nil {
			log.Fatal().Msgf("Invalid end time: %v", err)
		}
	}

	if endTime.Before(startTime) {
		log.Fatal().Msg("End time must be after start time")
	}

	ctx := context.Background()

	// Create Flow client
	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Msgf("Failed to create Flow client: %v", err)
	}
	defer flowClient.Close()

	// Execute based on the specified option
	var result *client.UpgradeFirmwareResult

	switch {
	case hasComponentIDs:
		componentIDs := parseCommaSeparatedList(firmwareUpgradeComponentIDs)
		if len(componentIDs) == 0 {
			log.Fatal().Msg("No valid component IDs provided")
		}
		log.Info().
			Strs("component_ids", componentIDs).
			Time("start_time", startTime).
			Time("end_time", endTime).
			Msg("Upgrading firmware by component IDs")
		result, err = flowClient.UpgradeFirmwareByMachineIDs(ctx, componentIDs, &startTime, &endTime)

	case hasRackIDs:
		rackIDs := parseUUIDList(firmwareUpgradeRackIDs)
		if len(rackIDs) == 0 {
			log.Fatal().Msg("No valid rack IDs provided")
		}
		log.Info().
			Int("rack_count", len(rackIDs)).
			Str("component_type", firmwareUpgradeComponentType).
			Time("start_time", startTime).
			Time("end_time", endTime).
			Msg("Upgrading firmware by rack IDs")
		result, err = flowClient.UpgradeFirmwareByRackIDs(ctx, rackIDs, componentType, &startTime, &endTime)

	case hasRackNames:
		rackNames := parseCommaSeparatedList(firmwareUpgradeRackNames)
		if len(rackNames) == 0 {
			log.Fatal().Msg("No valid rack names provided")
		}
		log.Info().
			Strs("rack_names", rackNames).
			Str("component_type", firmwareUpgradeComponentType).
			Time("start_time", startTime).
			Time("end_time", endTime).
			Msg("Upgrading firmware by rack names")
		result, err = flowClient.UpgradeFirmwareByRackNames(ctx, rackNames, componentType, &startTime, &endTime)
	}

	if err != nil {
		log.Fatal().Err(err).Msg("Failed to upgrade firmware")
	}

	taskIDStrs := make([]string, 0, len(result.TaskIDs))
	for _, id := range result.TaskIDs {
		taskIDStrs = append(taskIDStrs, id.String())
	}

	log.Info().
		Strs("task_ids", taskIDStrs).
		Int("task_count", len(result.TaskIDs)).
		Msg("Firmware upgrade tasks submitted successfully")
}

// parseCommaSeparatedList parses a comma-separated string into a list of trimmed, non-empty strings
func parseCommaSeparatedList(s string) []string {
	var result []string
	for _, item := range strings.Split(s, ",") {
		item = strings.TrimSpace(item)
		if item != "" {
			result = append(result, item)
		}
	}
	return result
}

// parseUUIDList parses a comma-separated string of UUIDs
func parseUUIDList(s string) []uuid.UUID {
	var result []uuid.UUID
	for _, idStr := range strings.Split(s, ",") {
		idStr = strings.TrimSpace(idStr)
		if idStr == "" {
			continue
		}
		id, err := uuid.Parse(idStr)
		if err != nil {
			log.Fatal().Msgf("Invalid UUID: %s", idStr)
		}
		result = append(result, id)
	}
	return result
}
