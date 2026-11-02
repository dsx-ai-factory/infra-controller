// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"net"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/cobra"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/client"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

var (
	addRackID          string
	addName            string
	addType            string
	addManufacturer    string
	addSerialNumber    string
	addModel           string
	addFirmwareVersion string
	addSlotID          int
	addTrayIndex       int
	addHostID          int
	addDescription     string
	addBmcMAC          string
	addBmcIP           string
	addBmcType         string
)

const unknownComponentPosition = -1

// newAddCmd returns a configured cobra.Command for adding a new component
// to an existing rack in the inventory.
func newAddCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "add",
		Short: "Add a component to an existing rack",
		Long: `Add a new component to the inventory table. The component may optionally
be attached to an existing rack via --rack-id; when omitted the component is
ingested without a rack assignment and can be moved into a rack later via
"component update --rack-id".

Required:
  --name             : Component name
  --type             : Component type (compute, nvswitch, powershelf, torswitch, ums, cdu)
  --manufacturer     : Manufacturer
  --serial-number    : Serial number

Optional:
  --rack-id          : Rack UUID to add the component to
  --model            : Model name
  --firmware-version : Firmware version
  --slot-id          : Slot ID (position)
  --tray-index       : Tray index (position)
  --host-id          : Host ID (position)
  --description      : Description (JSON string)
  --bmc-mac          : BMC MAC address
  --bmc-ip           : BMC IP address
  --bmc-type         : BMC type (host, dpu). Default: host

Examples:
  # Add a compute node attached to a rack
  flow component add --rack-id "uuid" --name "node-01" --type compute \
    --manufacturer "NVIDIA" --serial-number "SN123" --slot-id 1 --tray-index 0 --host-id 1

  # Ingest an unattached component (no rack)
  flow component add --name "node-01" --type compute \
    --manufacturer "NVIDIA" --serial-number "SN123"

  # Add a powershelf with BMC
  flow component add --rack-id "uuid" --name "ps-01" --type powershelf \
    --manufacturer "NVIDIA" --serial-number "PS123" --bmc-mac "aa:bb:cc:dd:ee:ff" --bmc-ip "10.0.0.1"
`,
		Run: func(cmd *cobra.Command, args []string) {
			doAddComponent(cmd)
		},
	}

	cmd.Flags().StringVar(&addRackID, "rack-id", "", "Rack UUID (optional; leave empty to ingest without a rack assignment)")
	cmd.Flags().StringVar(&addName, "name", "", "Component name (required)")
	cmd.Flags().StringVarP(&addType, "type", "t", "", "Component type: compute, nvswitch, powershelf, torswitch, ums, cdu (required)")
	cmd.Flags().StringVar(&addManufacturer, "manufacturer", "", "Manufacturer (required)")
	cmd.Flags().StringVar(&addSerialNumber, "serial-number", "", "Serial number (required)")
	cmd.Flags().StringVar(&addModel, "model", "", "Model name")
	cmd.Flags().StringVar(&addFirmwareVersion, "firmware-version", "", "Firmware version")
	cmd.Flags().IntVar(&addSlotID, "slot-id", 0, "Slot ID")
	cmd.Flags().IntVar(&addTrayIndex, "tray-index", 0, "Tray index")
	cmd.Flags().IntVar(&addHostID, "host-id", 0, "Host ID")
	cmd.Flags().StringVar(&addDescription, "description", "", "Description (JSON string)")
	cmd.Flags().StringVar(&addBmcMAC, "bmc-mac", "", "BMC MAC address")
	cmd.Flags().StringVar(&addBmcIP, "bmc-ip", "", "BMC IP address")
	cmd.Flags().StringVar(&addBmcType, "bmc-type", "host", "BMC type: host, dpu")

	_ = cmd.MarkFlagRequired("name")
	_ = cmd.MarkFlagRequired("type")
	_ = cmd.MarkFlagRequired("manufacturer")
	_ = cmd.MarkFlagRequired("serial-number")

	return cmd
}

func init() {
	componentCmd.AddCommand(newAddCmd())
}

// doAddComponent builds a types.Component from the CLI flags and calls
// AddComponent via the gRPC client, printing the created component as JSON.
func doAddComponent(cmd *cobra.Command) {
	// --rack-id is optional; parse only when provided. uuid.Nil signals
	// "ingest without a rack assignment" to the server.
	var rackID uuid.UUID
	if addRackID != "" {
		parsed, err := uuid.Parse(addRackID)
		if err != nil {
			log.Fatal().Err(err).Msg("Invalid rack UUID")
		}
		rackID = parsed
	}

	compType := parseComponentTypeToTypes(addType)
	if compType == types.ComponentTypeUnknown {
		log.Fatal().Str("type", addType).Msg("Invalid component type")
	}

	comp := &types.Component{
		Type: compType,
		Info: types.DeviceInfo{
			ID:           uuid.New(),
			Name:         addName,
			Manufacturer: addManufacturer,
			SerialNumber: addSerialNumber,
			Model:        addModel,
			Description:  addDescription,
		},
		FirmwareVersion: addFirmwareVersion,
		Position:        componentPositionFromFlags(cmd),
		RackID:          rackID,
	}

	// Add BMC if provided
	if addBmcMAC != "" {
		bmcType := types.BMCTypeHost
		if addBmcType == "dpu" {
			bmcType = types.BMCTypeDPU
		}

		bmc := types.BMC{
			Type: bmcType,
		}
		mac, err := net.ParseMAC(addBmcMAC)
		if err != nil {
			log.Fatal().Err(err).Msg("Invalid BMC MAC address")
		}
		bmc.MAC = mac
		if addBmcIP != "" {
			bmc.IP = net.ParseIP(addBmcIP)
		}
		comp.BMCs = []types.BMC{bmc}
	}

	c, err := client.New(newGlobalClientConfig())
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to create client")
	}
	defer c.Close()

	ctx := context.Background()
	created, err := c.AddComponent(ctx, comp)
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to add component")
	}

	data, err := json.MarshalIndent(created, "", "  ")
	if err != nil {
		log.Fatal().Err(err).Msg("Failed to marshal JSON")
	}
	fmt.Println(string(data))
}

func componentPositionFromFlags(cmd *cobra.Command) types.InRackPosition {
	position := types.InRackPosition{
		SlotID:    unknownComponentPosition,
		TrayIndex: unknownComponentPosition,
		HostID:    unknownComponentPosition,
	}
	if cmd.Flags().Changed("slot-id") {
		position.SlotID = addSlotID
	}
	if cmd.Flags().Changed("tray-index") {
		position.TrayIndex = addTrayIndex
	}
	if cmd.Flags().Changed("host-id") {
		position.HostID = addHostID
	}
	return position
}
