// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"net"
	"time"

	"github.com/google/uuid"
)

// DeviceInfo contains device identification information.
type DeviceInfo struct {
	ID           uuid.UUID
	Name         string
	Manufacturer string
	Model        string
	SerialNumber string
	Description  string
}

// Location represents physical location of a rack.
type Location struct {
	Region     string
	Datacenter string
	Room       string
	Position   string
}

// InRackPosition represents a component's position within a rack.
type InRackPosition struct {
	SlotID    int
	TrayIndex int
	HostID    int
}

// BMC represents BMC (Baseboard Management Controller) information.
type BMC struct {
	Type BMCType
	MAC  net.HardwareAddr
	IP   net.IP
}

// Component represents a rack component (compute node, switch, etc.).
type Component struct {
	Type            ComponentType
	Info            DeviceInfo
	FirmwareVersion string
	Position        InRackPosition
	BMCs            []BMC
	ComponentID     string // Component ID from the component manager service
	RackID          uuid.UUID
	NVLDomainID     uuid.UUID
	PowerState      string
}

// Rack represents a physical rack containing components.
type Rack struct {
	Info         DeviceInfo
	Location     Location
	Components   []Component
	NVLDomainIDs []uuid.UUID
	// OperationStatus is Flow's aggregate operability phase for the rack.
	OperationStatus Phase
}

// NVLDomain represents an NVL domain (a group of related racks).
type NVLDomain struct {
	ID   uuid.UUID
	Name string
}

// Identifier can identify a resource by ID or Name.
type Identifier struct {
	ID   uuid.UUID
	Name string
}

// Pagination for list requests.
type Pagination struct {
	Offset int
	Limit  int
}

// StringQueryInfo for filtering list requests.
type StringQueryInfo struct {
	Patterns   []string
	IsWildcard bool
	UseOR      bool
}

// Task represents an async operation task.
type Task struct {
	ID           uuid.UUID
	Operation    string
	RackID       uuid.UUID
	ComponentIDs []uuid.UUID
	Description  string
	ExecutorType TaskExecutorType
	ExecutionID  string
	Status       TaskStatus
	Message      string
}

// ComponentDiff represents a difference found during validation.
type ComponentDiff struct {
	Type        DiffType
	ID          uuid.UUID // Flow internal component UUID
	ComponentID string    // Component ID assigned by the component manager service
	Expected    *Component
	Actual      *Component
	FieldDiffs  []FieldDiff
}

// FieldDiff represents a single field difference.
type FieldDiff struct {
	FieldName     string
	ExpectedValue string
	ActualValue   string
}

// OperationRule represents a configurable rule for executing operations.
type OperationRule struct {
	ID                 uuid.UUID
	Name               string
	Description        string
	OperationType      OperationType
	OperationCode      string // e.g., "power_on", "upgrade"
	RuleDefinitionJSON string // JSON-encoded rule definition
	IsDefault          bool
	CreatedAt          time.Time
	UpdatedAt          time.Time
}

// RackRuleAssociation represents an association between a rack and an operation rule.
type RackRuleAssociation struct {
	RackID        uuid.UUID
	OperationType OperationType
	OperationCode string // e.g., "power_on", "upgrade"
	RuleID        uuid.UUID
	CreatedAt     time.Time
	UpdatedAt     time.Time
}
