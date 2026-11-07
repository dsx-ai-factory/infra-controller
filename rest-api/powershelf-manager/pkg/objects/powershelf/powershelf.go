// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powershelf

import (
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powersupply"

	gofish "github.com/stmcginnis/gofish/redfish"
)

// PowerShelf is a snapshot of a powershelf. Consists of the power-shelf's PMC and its Redfish-exposed chassis, manager, and power supplies.
type PowerShelf struct {
	PMC           *pmc.PMC
	Chassis       *gofish.Chassis
	Manager       *gofish.Manager
	PowerSupplies []*powersupply.PowerSupply
}

type Component string

const (
	PMC Component = "PMC"
	PSU Component = "PSU"
)

// FirmwareState represents the overall state of a firmware operation.
type FirmwareState string

const (
	FirmwareStateQueued    FirmwareState = "Queued"
	FirmwareStateVerifying FirmwareState = "Verifying"
	FirmwareStateCompleted FirmwareState = "Completed"
	FirmwareStateFailed    FirmwareState = "Failed"
)

type FirmwareUpdate struct {
	PmcMacAddress string
	Component     Component
	VersionFrom   string
	VersionTo     string
	State         FirmwareState
	JobID         string
	ErrorMessage  string
}
