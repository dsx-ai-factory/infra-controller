// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package store provides the storage layer for inventory management.
// It defines the InventoryStore interface for persisting and retrieving
// inventory data (racks, components, NVL domains).
package store

import (
	"context"
	"time"

	"github.com/google/uuid"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// ComponentDrift represents a drift detected between expected (local DB) and actual (source system) data.
type ComponentDrift struct {
	ID          uuid.UUID
	ComponentID *uuid.UUID  // NULL for missing_in_expected
	ExternalID  *string     // Component ID from the component manager service; NULL for missing_in_actual
	DriftType   string      // "missing_in_expected", "missing_in_actual", "mismatch"
	Diffs       []FieldDiff // Field-level differences (for mismatch type)
	CheckedAt   time.Time
}

// FieldDiff represents a single field difference between expected and actual values.
type FieldDiff struct {
	FieldName     string
	ExpectedValue string
	ActualValue   string
}

// Store defines the interface for inventory data persistence.
// It provides operations for managing racks (with components) and NVL domains.
type Store interface {
	// Lifecycle
	Start(ctx context.Context) error
	Stop(ctx context.Context) error

	// Rack operations
	CreateExpectedRack(ctx context.Context, rack *rack.Rack) (uuid.UUID, error)
	GetRackByID(ctx context.Context, id uuid.UUID, withComponents bool) (*rack.Rack, error)
	GetRackByExternalID(ctx context.Context, externalID string, withComponents bool) (*rack.Rack, error)
	GetRacksByIDs(ctx context.Context, ids []uuid.UUID, withComponents bool) ([]*rack.Rack, error)
	GetRacksByIDsIncludingDeleted(ctx context.Context, ids []uuid.UUID, withComponents bool) ([]*rack.Rack, error)
	GetRackBySerial(ctx context.Context, manufacturer string, serial string, withComponents bool) (*rack.Rack, error)
	GetRackByIdentifier(ctx context.Context, identifier identifier.Identifier, withComponents bool) (*rack.Rack, error)
	PatchRack(ctx context.Context, rack *rack.Rack) (string, error)
	DeleteRack(ctx context.Context, id uuid.UUID) error
	PurgeRack(ctx context.Context, id uuid.UUID) error
	GetListOfRacks(ctx context.Context, info dbquery.StringQueryInfo, manufacturerFilter *dbquery.StringQueryInfo, modelFilter *dbquery.StringQueryInfo, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy, withComponents bool) ([]*rack.Rack, int32, error)

	// Component operations
	GetComponentByID(ctx context.Context, id uuid.UUID) (*component.Component, error)
	GetComponentBySerial(ctx context.Context, manufacturer string, serial string, withRack bool) (*component.Component, error)
	GetComponentByBMCMAC(ctx context.Context, macAddress string) (*component.Component, error)
	GetComponentsByExternalIDs(ctx context.Context, externalIDs []string) ([]*component.Component, error)
	GetListOfComponents(ctx context.Context, info dbquery.StringQueryInfo, manufacturerFilter *dbquery.StringQueryInfo, modelFilter *dbquery.StringQueryInfo, componentTypes []devicetypes.ComponentType, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy) ([]*component.Component, int32, error)
	AddComponent(ctx context.Context, comp *component.Component) (uuid.UUID, error)
	PatchComponent(ctx context.Context, comp *component.Component) error
	DeleteComponent(ctx context.Context, id uuid.UUID) error
	PurgeComponent(ctx context.Context, id uuid.UUID) error

	// Component drift operations
	GetDriftsByComponentIDs(ctx context.Context, componentIDs []uuid.UUID) ([]ComponentDrift, error)
	GetAllDrifts(ctx context.Context) ([]ComponentDrift, error)

	// NVL Domain operations
	CreateNVLDomain(ctx context.Context, nvlDomain *nvldomain.NVLDomain) (uuid.UUID, error)
	AttachRacksToNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier, rackIDs []identifier.Identifier) error
	DetachRacksFromNVLDomain(ctx context.Context, rackIDs []identifier.Identifier) error
	GetListOfNVLDomains(ctx context.Context, info dbquery.StringQueryInfo, pagination *dbquery.Pagination) ([]*nvldomain.NVLDomain, int32, error)
	GetRacksForNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier) ([]*rack.Rack, error)
}
