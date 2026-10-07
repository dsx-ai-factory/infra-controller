// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"strings"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// rackInput is the JSON input structure for rack commands.
type rackInput struct {
	Info struct {
		ID           string `json:"id"`
		Name         string `json:"name"`
		Manufacturer string `json:"manufacturer"`
		Model        string `json:"model"`
		SerialNumber string `json:"serial_number"`
		Description  string `json:"description"`
	} `json:"info"`
	Location struct {
		Region     string `json:"region"`
		Datacenter string `json:"datacenter"`
		Room       string `json:"room"`
		Position   string `json:"position"`
	} `json:"location"`
	NVLinkDomainIDs []string             `json:"nvl_domain_ids"`
	Components      []rackComponentInput `json:"components"`
}

// rackComponentInput is the JSON input structure for a single component within a rack definition.
type rackComponentInput struct {
	Type            string `json:"type"`
	FirmwareVersion string `json:"firmware_version"`
	ComponentID     string `json:"component_id"`
	Info            struct {
		ID           string `json:"id"`
		Name         string `json:"name"`
		Manufacturer string `json:"manufacturer"`
		Model        string `json:"model"`
		SerialNumber string `json:"serial_number"`
		Description  string `json:"description"`
	} `json:"info"`
	Position struct {
		SlotID    int `json:"slot_id"`
		TrayIndex int `json:"tray_index"`
		HostID    int `json:"host_id"`
	} `json:"position"`
	BMCs []rackBMCInput `json:"bmcs"`
}

// rackBMCInput is the JSON input structure for a BMC entry within a component definition.
type rackBMCInput struct {
	Type string `json:"type"`
	MAC  string `json:"mac"`
	IP   string `json:"ip"`
}

// readRackJSONData returns JSON bytes from a file path or an inline string.
// Cobra enforces that exactly one is non-empty before this is called.
func readRackJSONData(file, jsonStr string) ([]byte, error) {
	if file != "" {
		return os.ReadFile(file)
	}
	return []byte(jsonStr), nil
}

// parseRackJSON parses JSON bytes into a types.Rack.
func parseRackJSON(data []byte) (*types.Rack, error) {
	var input rackInput
	if err := json.Unmarshal(data, &input); err != nil {
		return nil, fmt.Errorf("failed to parse JSON: %w", err)
	}

	rack := types.Rack{
		Info: types.DeviceInfo{
			Name:         input.Info.Name,
			Manufacturer: input.Info.Manufacturer,
			Model:        input.Info.Model,
			SerialNumber: input.Info.SerialNumber,
			Description:  input.Info.Description,
		},
		Location: types.Location{
			Region:     input.Location.Region,
			Datacenter: input.Location.Datacenter,
			Room:       input.Location.Room,
			Position:   input.Location.Position,
		},
	}

	if input.Info.ID != "" {
		id, err := uuid.Parse(input.Info.ID)
		if err != nil {
			return nil, fmt.Errorf(
				"invalid rack UUID %q: %w", input.Info.ID, err,
			)
		}
		rack.Info.ID = id
	} else {
		rack.Info.ID = uuid.New()
	}

	rack.NVLDomainIDs = make([]uuid.UUID, 0, len(input.NVLinkDomainIDs))
	for _, domainID := range input.NVLinkDomainIDs {
		id, err := uuid.Parse(domainID)
		if err != nil {
			return nil, fmt.Errorf(
				"invalid NVLink domain UUID %q: %w", domainID, err,
			)
		}
		rack.NVLDomainIDs = append(rack.NVLDomainIDs, id)
	}

	rack.Components = make([]types.Component, 0, len(input.Components))
	for _, ci := range input.Components {
		comp, err := parseRackComponentInput(ci)
		if err != nil {
			return nil, err
		}
		rack.Components = append(rack.Components, comp)
	}

	return &rack, nil
}

// parseRackComponentInput converts a rackComponentInput to a types.Component,
// validating the component type and BMC fields in the process.
func parseRackComponentInput(
	ci rackComponentInput,
) (types.Component, error) {
	typ := parseComponentTypeToTypes(ci.Type)
	if typ == types.ComponentTypeUnknown {
		return types.Component{}, fmt.Errorf("invalid component type %q", ci.Type)
	}

	comp := types.Component{
		Type:            typ,
		FirmwareVersion: ci.FirmwareVersion,
		ComponentID:     ci.ComponentID,
		Info: types.DeviceInfo{
			Name:         ci.Info.Name,
			Manufacturer: ci.Info.Manufacturer,
			Model:        ci.Info.Model,
			SerialNumber: ci.Info.SerialNumber,
			Description:  ci.Info.Description,
		},
		Position: types.InRackPosition{
			SlotID:    ci.Position.SlotID,
			TrayIndex: ci.Position.TrayIndex,
			HostID:    ci.Position.HostID,
		},
	}

	if ci.Info.ID != "" {
		id, err := uuid.Parse(ci.Info.ID)
		if err != nil {
			return types.Component{}, fmt.Errorf(
				"invalid component UUID %q: %w", ci.Info.ID, err,
			)
		}
		comp.Info.ID = id
	} else {
		comp.Info.ID = uuid.New()
	}

	comp.BMCs = make([]types.BMC, 0, len(ci.BMCs))
	for _, bi := range ci.BMCs {
		bmc, err := parseRackBMCInput(bi)
		if err != nil {
			return types.Component{}, err
		}
		comp.BMCs = append(comp.BMCs, bmc)
	}

	return comp, nil
}

// parseRackBMCInput converts a rackBMCInput to a types.BMC, validating
// the MAC address and IP address fields if present.
func parseRackBMCInput(bi rackBMCInput) (types.BMC, error) {
	var bmc types.BMC

	bmc.Type = parseBMCTypeToTypes(bi.Type)
	if bmc.Type == types.BMCTypeUnknown {
		return types.BMC{}, fmt.Errorf("invalid BMC type %q", bi.Type)
	}

	if bi.MAC != "" {
		var err error
		bmc.MAC, err = net.ParseMAC(bi.MAC)
		if err != nil {
			return types.BMC{}, fmt.Errorf(
				"invalid BMC MAC address %q: %w", bi.MAC, err,
			)
		}
	}

	if bi.IP != "" {
		bmc.IP = net.ParseIP(bi.IP)
		if bmc.IP == nil {
			return types.BMC{}, fmt.Errorf(
				"invalid BMC IP address %q", bi.IP,
			)
		}
	}

	return bmc, nil
}

// parseBMCTypeToTypes converts a BMC type string (e.g. "host", "dpu") to
// types.BMCType. An empty string is treated as "host". An unrecognised value
// returns types.BMCTypeUnknown.
func parseBMCTypeToTypes(s string) types.BMCType {
	switch strings.ToLower(s) {
	case "host", "":
		return types.BMCTypeHost
	case "dpu":
		return types.BMCTypeDPU
	default:
		return types.BMCTypeUnknown
	}
}
