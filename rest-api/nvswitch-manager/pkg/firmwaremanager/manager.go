// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchmanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
)

// Config holds configuration for the FirmwareManager.
type Config struct {
	// PackagesDir is the directory containing firmware package YAML definitions
	PackagesDir string

	// FirmwareDir is the directory containing firmware files
	FirmwareDir string

	// NumWorkers is the number of concurrent update workers
	NumWorkers int

	// SchedulerInterval is how often the scheduler queries for pending updates
	SchedulerInterval time.Duration
}

// FirmwareManager orchestrates firmware updates for NV-Switches.
type FirmwareManager struct {
	config     Config
	packages   *packages.Registry
	store      UpdateStore
	nsmgr      *nvswitchmanager.NVSwitchManager
	workerPool *WorkerPool

	// switchLocks provides per-switch mutex to prevent concurrent QueueUpdate calls
	// for the same switch. This ensures the "check for active update + save new updates"
	// sequence is atomic at the application level.
	//
	// NOTE: This only works for single-instance deployments. For multi-node NSM deployments,
	// we will need to add distributed locking (e.g., PostgreSQL advisory locks, Redis locks,
	// or a partial unique index on the firmware_update table) to prevent race conditions
	// across multiple NSM instances.
	switchLocks sync.Map // map[uuid.UUID]*sync.Mutex
}

// getSwitchLock returns the mutex for a given switch, creating one if it doesn't exist.
func (m *FirmwareManager) getSwitchLock(switchUUID uuid.UUID) *sync.Mutex {
	lock, _ := m.switchLocks.LoadOrStore(switchUUID, &sync.Mutex{})
	return lock.(*sync.Mutex)
}

// New creates a new FirmwareManager.
func New(
	config Config,
	store UpdateStore,
	nsmgr *nvswitchmanager.NVSwitchManager,
) (*FirmwareManager, error) {
	// Create and load package registry
	pkgRegistry := packages.NewRegistry(config.FirmwareDir)
	if err := pkgRegistry.LoadFromDirectory(config.PackagesDir); err != nil {
		return nil, fmt.Errorf("failed to load firmware packages: %w", err)
	}

	log.Infof("Loaded %d firmware packages", pkgRegistry.Count())

	// Create worker pool with scheduler
	workerPool := NewWorkerPool(
		config.NumWorkers,
		config.SchedulerInterval,
		store,
		nsmgr,
		pkgRegistry,
	)

	return &FirmwareManager{
		config:     config,
		packages:   pkgRegistry,
		store:      store,
		nsmgr:      nsmgr,
		workerPool: workerPool,
	}, nil
}

// Start initializes and starts the firmware manager.
func (m *FirmwareManager) Start(ctx context.Context) error {
	log.Info("Starting firmware manager")

	// Start workers
	m.workerPool.Start()

	return nil
}

// Stop shuts down the firmware manager.
func (m *FirmwareManager) Stop() {
	log.Info("Stopping firmware manager")
	m.workerPool.Stop()
}

// QueueUpdate queues firmware updates for one or more components.
// If components is empty, all components in the bundle are updated in sequence.
// Returns the list of queued updates in execution order.
func (m *FirmwareManager) QueueUpdate(
	ctx context.Context,
	switchUUID uuid.UUID,
	bundleVersion string,
	components []nvswitch.Component,
) ([]*FirmwareUpdate, error) {
	// Acquire per-switch lock to prevent concurrent QueueUpdate calls for the same switch.
	// This ensures the "check for active update + save new updates" sequence is atomic.
	lock := m.getSwitchLock(switchUUID)
	lock.Lock()
	defer lock.Unlock()

	// Validate bundle exists
	pkg, err := m.packages.Get(bundleVersion)
	if err != nil {
		return nil, fmt.Errorf("invalid bundle version: %w", err)
	}

	// Validate switch exists
	tray, err := m.nsmgr.Get(ctx, switchUUID)
	if err != nil {
		return nil, fmt.Errorf("switch not found: %w", err)
	}

	// Determine which components to update
	var componentNames []string
	if len(components) == 0 {
		componentNames = pkg.GetOrderedComponents()
		if len(componentNames) == 0 {
			return nil, fmt.Errorf("no components found in bundle %s", bundleVersion)
		}
		// Default to BMC and BIOS — these only require BMC access (Redfish)
		// componentNames = []string{
		//	strings.ToLower(string(nvswitch.BMC)),
		//	strings.ToLower(string(nvswitch.BIOS)),
		//}
	} else {
		// Convert and validate specified components
		for _, c := range components {
			name := strings.ToLower(string(c))
			if !pkg.HasComponent(name) {
				return nil, fmt.Errorf("component %s not found in bundle %s", c, bundleVersion)
			}
			componentNames = append(componentNames, name)
		}
	}

	// Check for any active update on this switch (tray-level locking)
	existing, err := m.store.GetAnyActiveForSwitch(ctx, switchUUID)
	if err != nil {
		return nil, fmt.Errorf("failed to check for existing updates: %w", err)
	}
	if existing != nil {
		return nil, fmt.Errorf("update already in progress for switch %s: %s (component: %s, state: %s)",
			switchUUID, existing.ID, existing.Component, existing.State)
	}

	// Generate bundle update ID if we're updating multiple components
	var bundleUpdateID *uuid.UUID
	if len(componentNames) > 1 {
		id := uuid.New()
		bundleUpdateID = &id
	}

	// Create update records with sequencing
	var updates []*FirmwareUpdate
	var prevID *uuid.UUID

	for i, name := range componentNames {
		component := nvswitch.Component(strings.ToUpper(name))
		compDef := pkg.GetComponent(name)

		strategy := Strategy(compDef.Strategy)
		if !strategy.IsValid() {
			return nil, fmt.Errorf("invalid strategy for %s: %s", name, compDef.Strategy)
		}

		update := NewFirmwareUpdate(
			switchUUID,
			component,
			bundleVersion,
			strategy,
			compDef.Version,
		)

		// Set sequencing
		update.WithSequencing(bundleUpdateID, i+1, prevID)

		// Try to get current version
		if currentVersion, err := m.getCurrentVersion(ctx, tray, component, strategy, pkg); err == nil {
			update.VersionFrom = currentVersion
		}

		// Save to database
		if err := m.store.Save(ctx, update); err != nil {
			return nil, fmt.Errorf("failed to queue update for %s: %w", component, err)
		}

		log.Infof("Queued firmware update: id=%s, switch=%s, component=%s, strategy=%s, version=%s->%s, seq=%d",
			update.ID, switchUUID, component, strategy, update.VersionFrom, update.VersionTo, update.SequenceOrder)

		updates = append(updates, update)
		prevID = &update.ID
	}

	return updates, nil
}

// getCurrentVersion gets the current firmware version using the appropriate strategy.
func (m *FirmwareManager) getCurrentVersion(
	ctx context.Context,
	tray *nvswitch.NVSwitchTray,
	component nvswitch.Component,
	strategy Strategy,
	pkg *packages.FirmwarePackage,
) (string, error) {
	var s UpdateStrategy
	switch strategy {
	case StrategyRedfish:
		s = NewRedfishStrategy(pkg.StrategyConfig.Redfish)
	case StrategySSH:
		s = NewSSHStrategy(pkg.StrategyConfig.SSH)
	case StrategyScript:
		s = NewScriptStrategy(pkg.StrategyConfig.Script)
	default:
		return "", fmt.Errorf("unknown strategy: %s", strategy)
	}
	return s.GetCurrentVersion(ctx, tray, component)
}

// GetUpdate retrieves a firmware update by ID.
func (m *FirmwareManager) GetUpdate(ctx context.Context, updateID uuid.UUID) (*FirmwareUpdate, error) {
	return m.store.Get(ctx, updateID)
}

// GetUpdatesForSwitch returns the firmware updates for the most recent
// bundle of a switch, filtering out stale historical records.
func (m *FirmwareManager) GetUpdatesForSwitch(ctx context.Context, switchUUID uuid.UUID) ([]*FirmwareUpdate, error) {
	return m.store.GetLatestBundleBySwitch(ctx, switchUUID)
}

// GetAllUpdates returns all firmware updates.
func (m *FirmwareManager) GetAllUpdates(ctx context.Context) ([]*FirmwareUpdate, error) {
	return m.store.GetAll(ctx)
}

// CancelUpdate attempts to cancel an in-progress update.
// If the update is part of a bundle, all remaining (QUEUED) updates in the bundle are also cancelled.
func (m *FirmwareManager) CancelUpdate(ctx context.Context, updateID uuid.UUID) error {
	update, err := m.store.Get(ctx, updateID)
	if err != nil {
		return err
	}

	if update.State.IsTerminal() {
		return fmt.Errorf("update already in terminal state: %s", update.State)
	}

	// Try to cancel if actively running
	if m.workerPool.CancelJob(updateID) {
		log.Infof("Cancelled active job: %s", updateID)
	}

	// Update state in database
	update.ErrorMessage = "cancelled by user"
	update.SetState(StateCancelled)
	if err := m.store.Save(ctx, update); err != nil {
		return err
	}

	// If this update is part of a bundle, cancel remaining updates in the bundle
	if update.BundleUpdateID != nil {
		cancelled, err := m.store.CancelRemainingInBundle(ctx, *update.BundleUpdateID, update.SequenceOrder, update.Component)
		if err != nil {
			log.Warnf("Failed to cancel remaining bundle updates: %v", err)
		} else if cancelled > 0 {
			log.Infof("Cancelled %d remaining updates in bundle %s", cancelled, update.BundleUpdateID)
		}
	}

	return nil
}

// ListBundles returns all available firmware bundle versions.
func (m *FirmwareManager) ListBundles() []string {
	return m.packages.List()
}

// GetBundle returns a firmware package by version.
func (m *FirmwareManager) GetBundle(version string) (*packages.FirmwarePackage, error) {
	return m.packages.Get(version)
}

// Stats returns current worker pool statistics.
func (m *FirmwareManager) Stats() FirmwareManagerStats {
	return FirmwareManagerStats{
		ActiveJobs:    m.workerPool.ActiveJobCount(),
		LoadedBundles: m.packages.Count(),
		WorkerCount:   m.config.NumWorkers,
	}
}

// FirmwareManagerStats holds runtime statistics.
type FirmwareManagerStats struct {
	ActiveJobs    int
	LoadedBundles int
	WorkerCount   int
}

// ReloadPackages reloads firmware packages from the packages directory.
func (m *FirmwareManager) ReloadPackages() error {
	return m.packages.LoadFromDirectory(m.config.PackagesDir)
}
