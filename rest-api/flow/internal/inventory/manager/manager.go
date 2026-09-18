// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package manager provides the business logic layer for inventory management.
// It wraps the InventoryStore and provides a place for validation, caching,
// audit logging, and other business rules.
package manager

import (
	"context"
	"sync"

	"github.com/google/uuid"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// Re-export for convenience
type ComponentDrift = inventorystore.ComponentDrift
type FieldDiff = inventorystore.FieldDiff

// Manager defines the interface for inventory management business logic.
// It wraps InventoryStore and provides a consistent API for the service layer.
type Manager interface {
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
	GetDriftsByComponentIDs(ctx context.Context, componentIDs []uuid.UUID) ([]inventorystore.ComponentDrift, error)
	GetAllDrifts(ctx context.Context) ([]inventorystore.ComponentDrift, error)

	// NVL Domain operations
	CreateNVLDomain(ctx context.Context, nvlDomain *nvldomain.NVLDomain) (uuid.UUID, error)
	AttachRacksToNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier, rackIDs []identifier.Identifier) error
	DetachRacksFromNVLDomain(ctx context.Context, rackIDs []identifier.Identifier) error
	GetListOfNVLDomains(ctx context.Context, info dbquery.StringQueryInfo, pagination *dbquery.Pagination) ([]*nvldomain.NVLDomain, int32, error)
	GetRacksForNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier) ([]*rack.Rack, error)
}

// ManagerImpl implements the Manager interface.
// It currently delegates all operations to the underlying store,
// but provides a place to add business logic in the future.
type ManagerImpl struct {
	store     inventorystore.Store
	startOnce sync.Once
	stopOnce  sync.Once
}

// New creates a new inventory manager with the given store.
func New(store inventorystore.Store) *ManagerImpl {
	return &ManagerImpl{
		store: store,
	}
}

// Start starts the manager.
func (m *ManagerImpl) Start(ctx context.Context) error {
	var err error
	m.startOnce.Do(func() {
		err = m.store.Start(ctx)
	})
	return err
}

// Stop stops the manager.
func (m *ManagerImpl) Stop(ctx context.Context) error {
	var err error
	m.stopOnce.Do(func() {
		err = m.store.Stop(ctx)
	})
	return err
}

// CreateExpectedRack creates an expected rack configuration.
// Future: Add validation, audit logging, etc.
func (m *ManagerImpl) CreateExpectedRack(ctx context.Context, rack *rack.Rack) (uuid.UUID, error) {
	return m.store.CreateExpectedRack(ctx, rack)
}

// GetRackByID retrieves a rack by its UUID.
func (m *ManagerImpl) GetRackByID(ctx context.Context, id uuid.UUID, withComponents bool) (*rack.Rack, error) {
	return m.store.GetRackByID(ctx, id, withComponents)
}

// GetRackByExternalID retrieves a rack by its external ID.
func (m *ManagerImpl) GetRackByExternalID(ctx context.Context, externalID string, withComponents bool) (*rack.Rack, error) {
	return m.store.GetRackByExternalID(ctx, externalID, withComponents)
}

// GetRacksByIDs retrieves multiple racks by their UUIDs.
func (m *ManagerImpl) GetRacksByIDs(ctx context.Context, ids []uuid.UUID, withComponents bool) ([]*rack.Rack, error) {
	return m.store.GetRacksByIDs(ctx, ids, withComponents)
}

// GetRacksByIDsIncludingDeleted retrieves multiple racks by UUID, including
// soft-deleted rows.
func (m *ManagerImpl) GetRacksByIDsIncludingDeleted(ctx context.Context, ids []uuid.UUID, withComponents bool) ([]*rack.Rack, error) {
	return m.store.GetRacksByIDsIncludingDeleted(ctx, ids, withComponents)
}

// GetRackBySerial retrieves a rack by its serial number and manufacturer.
func (m *ManagerImpl) GetRackBySerial(ctx context.Context, manufacturer string, serial string, withComponents bool) (*rack.Rack, error) {
	return m.store.GetRackBySerial(ctx, manufacturer, serial, withComponents)
}

// GetRackByIdentifier retrieves a rack by its identifier (ID or name).
func (m *ManagerImpl) GetRackByIdentifier(ctx context.Context, identifier identifier.Identifier, withComponents bool) (*rack.Rack, error) {
	return m.store.GetRackByIdentifier(ctx, identifier, withComponents)
}

// PatchRack updates an existing rack.
func (m *ManagerImpl) PatchRack(ctx context.Context, rack *rack.Rack) (string, error) {
	return m.store.PatchRack(ctx, rack)
}

// GetListOfRacks lists racks matching the given criteria.
func (m *ManagerImpl) GetListOfRacks(ctx context.Context, info dbquery.StringQueryInfo, manufacturerFilter *dbquery.StringQueryInfo, modelFilter *dbquery.StringQueryInfo, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy, withComponents bool) ([]*rack.Rack, int32, error) {
	return m.store.GetListOfRacks(ctx, info, manufacturerFilter, modelFilter, pagination, orderBy, withComponents)
}

// GetListOfComponents lists components matching the given criteria.
func (m *ManagerImpl) GetListOfComponents(ctx context.Context, info dbquery.StringQueryInfo, manufacturerFilter *dbquery.StringQueryInfo, modelFilter *dbquery.StringQueryInfo, componentTypes []devicetypes.ComponentType, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy) ([]*component.Component, int32, error) {
	return m.store.GetListOfComponents(ctx, info, manufacturerFilter, modelFilter, componentTypes, pagination, orderBy)
}

// GetComponentByID retrieves a component by its UUID.
func (m *ManagerImpl) GetComponentByID(ctx context.Context, id uuid.UUID) (*component.Component, error) {
	return m.store.GetComponentByID(ctx, id)
}

// GetComponentBySerial retrieves a component by its serial number and manufacturer.
func (m *ManagerImpl) GetComponentBySerial(ctx context.Context, manufacturer string, serial string, withRack bool) (*component.Component, error) {
	return m.store.GetComponentBySerial(ctx, manufacturer, serial, withRack)
}

// GetComponentByBMCMAC retrieves a component by its BMC MAC address.
func (m *ManagerImpl) GetComponentByBMCMAC(ctx context.Context, macAddress string) (*component.Component, error) {
	return m.store.GetComponentByBMCMAC(ctx, macAddress)
}

// GetComponentsByExternalIDs retrieves components by their external IDs.
func (m *ManagerImpl) GetComponentsByExternalIDs(ctx context.Context, externalIDs []string) ([]*component.Component, error) {
	return m.store.GetComponentsByExternalIDs(ctx, externalIDs)
}

// CreateNVLDomain creates a new NVL domain.
func (m *ManagerImpl) CreateNVLDomain(ctx context.Context, nvlDomain *nvldomain.NVLDomain) (uuid.UUID, error) {
	return m.store.CreateNVLDomain(ctx, nvlDomain)
}

// AttachRacksToNVLDomain attaches racks to an NVL domain.
func (m *ManagerImpl) AttachRacksToNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier, rackIDs []identifier.Identifier) error {
	return m.store.AttachRacksToNVLDomain(ctx, nvlDomainID, rackIDs)
}

// DetachRacksFromNVLDomain detaches racks from their NVL domain.
func (m *ManagerImpl) DetachRacksFromNVLDomain(ctx context.Context, rackIDs []identifier.Identifier) error {
	return m.store.DetachRacksFromNVLDomain(ctx, rackIDs)
}

// GetListOfNVLDomains lists NVL domains matching the given criteria.
func (m *ManagerImpl) GetListOfNVLDomains(ctx context.Context, info dbquery.StringQueryInfo, pagination *dbquery.Pagination) ([]*nvldomain.NVLDomain, int32, error) {
	return m.store.GetListOfNVLDomains(ctx, info, pagination)
}

// GetRacksForNVLDomain retrieves all racks belonging to an NVL domain.
func (m *ManagerImpl) GetRacksForNVLDomain(ctx context.Context, nvlDomainID identifier.Identifier) ([]*rack.Rack, error) {
	return m.store.GetRacksForNVLDomain(ctx, nvlDomainID)
}

// AddComponent creates a single component in the database and returns its UUID.
func (m *ManagerImpl) AddComponent(ctx context.Context, comp *component.Component) (uuid.UUID, error) {
	return m.store.AddComponent(ctx, comp)
}

// PatchComponent updates a single component's fields in the database.
func (m *ManagerImpl) PatchComponent(ctx context.Context, comp *component.Component) error {
	return m.store.PatchComponent(ctx, comp)
}

// DeleteRack soft-deletes a rack and all its components.
func (m *ManagerImpl) DeleteRack(ctx context.Context, id uuid.UUID) error {
	return m.store.DeleteRack(ctx, id)
}

// PurgeRack permanently removes a soft-deleted rack and its components.
func (m *ManagerImpl) PurgeRack(ctx context.Context, id uuid.UUID) error {
	return m.store.PurgeRack(ctx, id)
}

// DeleteComponent soft-deletes a component by UUID.
func (m *ManagerImpl) DeleteComponent(ctx context.Context, id uuid.UUID) error {
	return m.store.DeleteComponent(ctx, id)
}

// PurgeComponent permanently removes a soft-deleted component.
func (m *ManagerImpl) PurgeComponent(ctx context.Context, id uuid.UUID) error {
	return m.store.PurgeComponent(ctx, id)
}

// GetDriftsByComponentIDs retrieves drift records for the given component UUIDs.
func (m *ManagerImpl) GetDriftsByComponentIDs(ctx context.Context, componentIDs []uuid.UUID) ([]inventorystore.ComponentDrift, error) {
	return m.store.GetDriftsByComponentIDs(ctx, componentIDs)
}

// GetAllDrifts retrieves all drift records.
func (m *ManagerImpl) GetAllDrifts(ctx context.Context) ([]inventorystore.ComponentDrift, error) {
	return m.store.GetAllDrifts(ctx)
}
