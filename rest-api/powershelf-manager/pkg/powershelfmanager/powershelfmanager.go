// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powershelfmanager

import (
	"context"
	"fmt"
	"net"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/firmwaremanager"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/inventorymanager"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/pmcmanager"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/pmcregistry"

	log "github.com/sirupsen/logrus"
)

// PowershelfManager coordinates registry, credential manager, firmware manager, and Redfish sessions to implement service operations.
type PowershelfManager struct {
	DataStoreType   DataStoreType
	PmcManager      *pmcmanager.PmcManager
	FirmwareManager *firmwaremanager.Manager
}

// New creates a new instance of PowershelfManager with firmware, credential, and registry backends based on the given configuration.
func New(ctx context.Context, c Config) (*PowershelfManager, error) {
	credentialManager, err := credentials.New(ctx, &c.CredentialConf)
	if err != nil {
		return nil, fmt.Errorf("failed to initialize credential manager (conf: %v): %w", c, err)
	}

	var registry pmcregistry.PmcRegistry
	switch c.DSType {
	case DatastoreTypePersistent:
		log.Printf("Initializing powershelf manager with a persistent PMC registry")
		if err := c.PmcRegistryConf.DSConf.Validate(); err != nil {
			return nil, err
		}

		registry, err = pmcregistry.New(ctx, &c.PmcRegistryConf)
		if err != nil {
			return nil, fmt.Errorf("failed to initialize persistent PMC registry (conf: %v): %w", c, err)
		}
	case DatastoreTypeInMemory:
		log.Printf("Initializing Powershelf Manager with an in-memory PMC registry")

		registry, err = pmcregistry.New(ctx, &c.PmcRegistryConf)
		if err != nil {
			return nil, fmt.Errorf("failed to initialize in-memory PMC registry (conf: %v): %w", c, err)
		}
	default:
		return nil, fmt.Errorf("unsupported PMC registry type %v", c.DSType)
	}

	pmcManager := pmcmanager.New(registry, credentialManager)

	var fwStore firmwaremanager.FirmwareUpdateStore
	switch c.DSType {
	case DatastoreTypePersistent:
		log.Printf("Initializing firmware manager with a PostgreSQL store")
		fwStore, err = firmwaremanager.NewPostgresStore(ctx, c.PmcRegistryConf.DSConf)
		if err != nil {
			return nil, fmt.Errorf("failed to initialize firmware postgres store (conf: %v): %w", c, err)
		}
	case DatastoreTypeInMemory:
		log.Printf("Initializing firmware manager with an in-memory store (updates will not persist across restarts)")
		fwStore = firmwaremanager.NewInMemoryStore()
	default:
		return nil, fmt.Errorf("unsupported datastore type for firmware manager: %v", c.DSType)
	}

	firmwareManager, err := firmwaremanager.New(fwStore, pmcManager, false, c.FirmwareDir)
	if err != nil {
		return nil, fmt.Errorf("failed to initialize firmware manager (conf: %v): %w", c, err)
	}

	return &PowershelfManager{
		DataStoreType:   c.DSType,
		PmcManager:      pmcManager,
		FirmwareManager: firmwareManager,
	}, nil
}

// Start initializes the registry and credential manager.
func (pm *PowershelfManager) Start(ctx context.Context) error {
	if err := pm.PmcManager.Start(ctx); err != nil {
		return err
	}

	return inventorymanager.Start(pm.PmcManager)
}

// Stop shuts down the registry and credential manager.
func (pm *PowershelfManager) Stop(ctx context.Context) error {
	if err := inventorymanager.Stop(); err != nil {
		return err
	}

	return pm.PmcManager.Stop(ctx)
}

// GetPmc resolves a PMC by MAC from the registry and attaches its credential from the credential manager.
func (pm *PowershelfManager) GetPmc(ctx context.Context, mac net.HardwareAddr) (*pmc.PMC, error) {
	return pm.PmcManager.GetPmc(ctx, mac)
}

// GetAllPowershelves returns PowerShelf views for all registered PMCs.
func (pm *PowershelfManager) GetPowershelves(ctx context.Context, macs []net.HardwareAddr) ([]*powershelf.PowerShelf, error) {
	return inventorymanager.GetPowershelves(macs), nil
}

// GetAllPowershelves returns PowerShelf views for all registered PMCs.
func (pm *PowershelfManager) GetAllPowershelves(ctx context.Context) ([]*powershelf.PowerShelf, error) {
	return inventorymanager.GetAllPowershelves(), nil
}

// RegisterPmc persists PMC identity in the registry and stores credentials keyed by MAC in the credential manager.
func (pm *PowershelfManager) RegisterPmc(ctx context.Context, pmc *pmc.PMC) error {
	return pm.PmcManager.Register(ctx, pmc)
}

func (pm *PowershelfManager) ListAvailableFirmware(ctx context.Context, mac net.HardwareAddr) ([]firmwaremanager.FirmwareUpgrade, error) {
	pmc, err := pm.GetPmc(ctx, mac)
	if err != nil {
		return nil, fmt.Errorf("failed to get query PMC (%s): %w", mac.String(), err)
	}

	return pm.FirmwareManager.ListAvailableFirmware(ctx, pmc)

}

// UpgradeFirmware performs (or simulates) a firmware upgrade. Returns the underlying HTTP response from the device on success.
func (pm *PowershelfManager) UpgradeFirmware(ctx context.Context, mac net.HardwareAddr, component powershelf.Component, targetFwVersion string) error {
	pmc, err := pm.GetPmc(ctx, mac)
	if err != nil {
		return fmt.Errorf("failed to get query PMC (%s): %w", mac.String(), err)
	}

	return pm.FirmwareManager.Upgrade(ctx, pmc, component, targetFwVersion)
}

// GetFirmwareUpdateStatus returns the status of a firmware update for the specified PMC and component.
func (pm *PowershelfManager) GetFirmwareUpdateStatus(ctx context.Context, mac net.HardwareAddr, component powershelf.Component) (*powershelf.FirmwareUpdate, error) {
	return pm.FirmwareManager.GetFirmwareUpdate(ctx, mac, component)
}

func (pm *PowershelfManager) powerControl(ctx context.Context, mac net.HardwareAddr, on bool) error {
	return pm.PmcManager.PowerControl(ctx, mac, on)
}

// Power ON the rack
func (pm *PowershelfManager) PowerOn(ctx context.Context, mac net.HardwareAddr) error {
	return pm.powerControl(ctx, mac, true)
}

// Power OFF the rack
func (pm *PowershelfManager) PowerOff(ctx context.Context, mac net.HardwareAddr) error {
	return pm.powerControl(ctx, mac, false)
}

// PowerControlDirect performs a power action using pre-built connection details,
// bypassing registry and credential manager lookups.
func (pm *PowershelfManager) PowerControlDirect(ctx context.Context, pmc *pmc.PMC, on bool) error {
	return pm.PmcManager.PowerControlDirect(ctx, pmc, on)
}
