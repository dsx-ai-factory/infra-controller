// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var (
	updateComponentID     string
	updateFirmwareVersion string
	updateSlotID          int
	updateTrayIndex       int
	updateHostID          int
	updateDescription     string
	updateRackID          string
	updateBMCs            string
)

// newUpdateCmd returns a configured cobra.Command for patching a component's
// fields (firmware version, position, description, or rack assignment).
func newUpdateCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "update",
		Short: "Update a component's fields",
		Long: `Update a component's patchable fields in the inventory table.

Required:
  --id              : Component UUID (required)

Patchable fields (at least one required):
  --firmware-version : New firmware version
  --slot-id          : New slot ID (position)
  --tray-index       : New tray index (position)
  --host-id          : New host ID (position)
  --description      : New description (JSON string)
  --rack-id          : Re-assign to a different rack (UUID)
  --bmcs             : BMCs as JSON array, e.g. [{"type":"HOST","mac":"aa:bb:cc:dd:ee:ff","ip":"10.0.0.1"}]

Examples:
  # Update firmware version
  flow component update --id "uuid" --firmware-version "2.0.0"

  # Update position fields
  flow component update --id "uuid" --slot-id 3 --tray-index 1 --host-id 5

  # Re-assign to a different rack
  flow component update --id "uuid" --rack-id "new-rack-uuid"

  # Update BMC information
  flow component update --id "uuid" --bmcs '[{"type":"HOST","mac":"aa:bb:cc:dd:ee:ff","ip":"10.0.0.1"}]'

  # Update multiple fields at once
  flow component update --id "uuid" --firmware-version "2.0.0" --slot-id 3
`,
		Run: func(cmd *cobra.Command, args []string) {
			doUpdateComponent(cmd)
		},
	}

	cmd.Flags().StringVar(&updateComponentID, "id", "", "Component UUID (required)")
	cmd.Flags().StringVar(&updateFirmwareVersion, "firmware-version", "", "New firmware version")
	cmd.Flags().IntVar(&updateSlotID, "slot-id", 0, "New slot ID")
	cmd.Flags().IntVar(&updateTrayIndex, "tray-index", 0, "New tray index")
	cmd.Flags().IntVar(&updateHostID, "host-id", 0, "New host ID")
	cmd.Flags().StringVar(&updateDescription, "description", "", "New description (JSON string)")
	cmd.Flags().StringVar(&updateRackID, "rack-id", "", "Re-assign to a different rack (UUID)")
	cmd.Flags().StringVar(&updateBMCs, "bmcs", "", `BMCs as JSON array, e.g. [{"type":"HOST","mac":"aa:bb:cc:dd:ee:ff","ip":"10.0.0.1"}]`)

	_ = cmd.MarkFlagRequired("id")

	return cmd
}

func init() {
	componentCmd.AddCommand(newUpdateCmd())
}

// doUpdateComponent builds a PatchComponentOpts from the changed flags and
// calls PatchComponent via the gRPC client, printing the updated component as JSON.
func doUpdateComponent(cmd *cobra.Command) {
	// Parse component ID
	compID, err := uuid.Parse(updateComponentID)
	if err != nil {
		log.Fatal().Err(err).Msg("Invalid component UUID")
	}

	// Build patch options
	opts := client.PatchComponentOpts{}
	hasUpdate := false

	if updateFirmwareVersion != "" {
		opts.FirmwareVersion = &updateFirmwareVersion
		hasUpdate = true
	}

	if cmd.Flags().Changed("slot-id") {
		slotID := int32(updateSlotID)
		opts.SlotID = &slotID
		hasUpdate = true
	}

	if cmd.Flags().Changed("tray-index") {
		trayIdx := int32(updateTrayIndex)
		opts.TrayIndex = &trayIdx
		hasUpdate = true
	}

	if cmd.Flags().Changed("host-id") {
		hostID := int32(updateHostID)
		opts.HostID = &hostID
		hasUpdate = true
	}

	if updateDescription != "" {
		opts.Description = &updateDescription
		hasUpdate = true
	}

	if updateRackID != "" {
		rackID, err := uuid.Parse(updateRackID)
		if err != nil {
			log.Fatal().Err(err).Msg("Invalid rack UUID")
		}
		opts.RackID = &rackID
		hasUpdate = true
	}

	if updateBMCs != "" {
		bmcs, err := parseBMCsFromJSON(updateBMCs)
		if err != nil {
			log.Fatal().Err(err).Msg("Invalid --bmcs input")
		}
		opts.BMCs = bmcs
		hasUpdate = true
	}

	if !hasUpdate {
		log.Fatal().Msg("At least one field to update must be specified")
	}

	// Create client
	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx := context.Background()
	updated, err := c.PatchComponent(ctx, compID, opts)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to update component")
	}

	// Output the updated component as JSON
	data, err := json.MarshalIndent(updated, "", "  ")
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal JSON")
	}
	fmt.Println(string(data))
}

// parseBMCsFromJSON parses a JSON array of BMC objects, validates each entry's
// type (HOST or DPU), MAC address, and IP address, and returns typed BMC values.
func parseBMCsFromJSON(input string) ([]types.BMC, error) {
	var rawBMCs []struct {
		Type string `json:"type"`
		MAC  string `json:"mac"`
		IP   string `json:"ip"`
	}
	if err := json.Unmarshal([]byte(input), &rawBMCs); err != nil {
		return nil, fmt.Errorf("invalid JSON: %w", err)
	}

	bmcs := make([]types.BMC, 0, len(rawBMCs))
	for _, rb := range rawBMCs {
		bt := types.BMCType(strings.ToUpper(rb.Type))
		switch bt {
		case types.BMCTypeHost, types.BMCTypeDPU:
		default:
			return nil, fmt.Errorf("invalid BMC type %q (allowed: HOST, DPU)", rb.Type)
		}

		b := types.BMC{Type: bt}
		if rb.MAC != "" {
			mac, err := net.ParseMAC(rb.MAC)
			if err != nil {
				return nil, fmt.Errorf("invalid MAC address %q: %w", rb.MAC, err)
			}
			b.MAC = mac
		}
		if rb.IP != "" {
			b.IP = net.ParseIP(rb.IP)
			if b.IP == nil {
				return nil, fmt.Errorf("invalid IP address %q", rb.IP)
			}
		}
		bmcs = append(bmcs, b)
	}
	return bmcs, nil
}
