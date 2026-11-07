// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitch

import (
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"

	"github.com/google/uuid"
	gofish "github.com/stmcginnis/gofish/redfish"
)

// NVSwitchTray represents a complete NV-Switch tray with its two subsystems: BMC and NVOS.
// The UUID is service-generated and serves as the primary identifier for all operations.
type NVSwitchTray struct {
	UUID   uuid.UUID     `json:"uuid"`    // Service-generated unique identifier
	Vendor vendor.Vendor `json:"vendor"`  // Hardware vendor (e.g., NVIDIA)
	RackID string        `json:"rack_id"` // Optional rack identifier for grouping switches

	// Subsystems
	BMC  *bmc.BMC   `json:"bmc"`  // Board Management Controller subsystem
	NVOS *nvos.NVOS `json:"nvos"` // NV-Switch Operating System subsystem

	// Enriched data from Redfish (populated during inventory queries)
	Chassis         *gofish.Chassis `json:"-"` // Redfish chassis info
	Manager         *gofish.Manager `json:"-"` // Redfish manager info
	SerialNumber    string          `json:"serial_number"`
	FirmwareVersion string          `json:"firmware_version"` // BMC firmware version
	NVOSVersion     string          `json:"nvos_version"`     // NVOS version
	CPLDVersion     string          `json:"cpld_version"`     // CPLD firmware version
}

// NewUUID generates a new UUID for an NVSwitchTray.
func NewUUID() uuid.UUID {
	return uuid.New()
}

// Component represents the updatable components of an NV-Switch tray.
type Component string

const (
	// BMC is the BMC firmware updated via Redfish
	BMC Component = "BMC"
	// CPLD is updated via NVOS SSH
	CPLD Component = "CPLD"
	// BIOS is the BIOS firmware updated via Redfish
	BIOS Component = "BIOS"
	// NVOS is the operating system updated via SSH
	NVOS Component = "NVOS"
)

// IsValid returns true if the component is a known valid component.
func (c Component) IsValid() bool {
	switch c {
	case BMC, CPLD, BIOS, NVOS:
		return true
	default:
		return false
	}
}

// FirmwareState represents the overall state of a firmware operation.
type FirmwareState string

const (
	FirmwareStateQueued        FirmwareState = "Queued"
	FirmwareStateInProgress    FirmwareState = "InProgress"
	FirmwareStateWaitingReboot FirmwareState = "WaitingReboot"
	FirmwareStateVerifying     FirmwareState = "Verifying"
	FirmwareStateCompleted     FirmwareState = "Completed"
	FirmwareStateFailed        FirmwareState = "Failed"
)

// IsTerminal returns true if the state is a terminal state (completed or failed).
func (s FirmwareState) IsTerminal() bool {
	return s == FirmwareStateCompleted || s == FirmwareStateFailed
}

// FirmwareUpdate tracks the state of a firmware update operation.
type FirmwareUpdate struct {
	SwitchUUID      uuid.UUID     `json:"switch_uuid"` // NVSwitchTray UUID
	Component       Component     `json:"component"`
	VersionFrom     string        `json:"version_from"`
	VersionTo       string        `json:"version_to"`
	State           FirmwareState `json:"state"`
	PercentComplete int           `json:"percent_complete"`
	JobID           string        `json:"job_id"`
	ErrorMessage    string        `json:"error_message,omitempty"`
}
