// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchregistry

import (
	"context"
	"fmt"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

// InMemoryRegistry is an in-memory implementation of Registry.
type InMemoryRegistry struct {
	mu       sync.RWMutex
	switches map[uuid.UUID]*nvswitch.NVSwitchTray
	byBMCMAC map[string]uuid.UUID // BMC MAC -> UUID lookup
}

// NewInMemoryRegistry creates a new in-memory registry.
func NewInMemoryRegistry() *InMemoryRegistry {
	return &InMemoryRegistry{
		switches: make(map[uuid.UUID]*nvswitch.NVSwitchTray),
		byBMCMAC: make(map[string]uuid.UUID),
	}
}

// Start initializes the registry.
func (r *InMemoryRegistry) Start(ctx context.Context) error {
	return nil
}

// Stop cleans up the registry.
func (r *InMemoryRegistry) Stop(ctx context.Context) error {
	return nil
}

// Register creates or updates an NV-Switch tray.
func (r *InMemoryRegistry) Register(ctx context.Context, tray *nvswitch.NVSwitchTray) (uuid.UUID, bool, error) {
	if tray == nil {
		return uuid.Nil, false, fmt.Errorf("tray is nil")
	}
	if tray.BMC == nil {
		return uuid.Nil, false, fmt.Errorf("tray.BMC is nil")
	}
	if tray.NVOS == nil {
		return uuid.Nil, false, fmt.Errorf("tray.NVOS is nil")
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	bmcMAC := tray.BMC.MAC.String()

	// Check if this BMC MAC is already registered
	if existingUUID, exists := r.byBMCMAC[bmcMAC]; exists {
		// Update existing entry
		tray.UUID = existingUUID
		r.switches[existingUUID] = tray
		return existingUUID, false, nil
	}

	// Generate new UUID if not set
	if tray.UUID == uuid.Nil {
		tray.UUID = uuid.New()
	}

	r.switches[tray.UUID] = tray
	r.byBMCMAC[bmcMAC] = tray.UUID

	// Also index by NVOS MAC if available
	if tray.NVOS != nil {
		r.byBMCMAC[tray.NVOS.MAC.String()] = tray.UUID
	}

	return tray.UUID, true, nil
}

// Get retrieves an NV-Switch by UUID.
func (r *InMemoryRegistry) Get(ctx context.Context, id uuid.UUID) (*nvswitch.NVSwitchTray, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	tray, exists := r.switches[id]
	if !exists {
		return nil, fmt.Errorf("NV-Switch with UUID %s not found", id)
	}

	return tray, nil
}

// GetByBMCMAC retrieves an NV-Switch by BMC MAC address.
func (r *InMemoryRegistry) GetByBMCMAC(ctx context.Context, bmcMAC string) (*nvswitch.NVSwitchTray, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	id, exists := r.byBMCMAC[bmcMAC]
	if !exists {
		return nil, fmt.Errorf("NV-Switch with BMC MAC %s not found", bmcMAC)
	}

	return r.switches[id], nil
}

// List returns all registered NV-Switches.
func (r *InMemoryRegistry) List(ctx context.Context) ([]*nvswitch.NVSwitchTray, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	result := make([]*nvswitch.NVSwitchTray, 0, len(r.switches))
	for _, tray := range r.switches {
		result = append(result, tray)
	}

	return result, nil
}

// Delete removes an NV-Switch by UUID.
func (r *InMemoryRegistry) Delete(ctx context.Context, id uuid.UUID) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	tray, exists := r.switches[id]
	if !exists {
		return nil // Already deleted
	}

	// Remove from MAC index
	if tray.BMC != nil {
		delete(r.byBMCMAC, tray.BMC.MAC.String())
	}
	if tray.NVOS != nil {
		delete(r.byBMCMAC, tray.NVOS.MAC.String())
	}

	delete(r.switches, id)
	return nil
}
