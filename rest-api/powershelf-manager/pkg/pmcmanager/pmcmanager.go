// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcmanager

import (
	"context"
	"errors"
	"fmt"
	"net"
	"time"

	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/pmcregistry"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/redfish"
)

const redfishTimeout = time.Minute * 1

type PmcManager struct {
	registry          pmcregistry.PmcRegistry
	credentialManager credentials.CredentialManager
}

func New(registry pmcregistry.PmcRegistry, credentialManager credentials.CredentialManager) *PmcManager {
	return &PmcManager{
		registry:          registry,
		credentialManager: credentialManager,
	}
}

func (pm *PmcManager) Start(ctx context.Context) error {
	err := pm.registry.Start(ctx)
	if err != nil {
		return err
	}

	return pm.credentialManager.Start(ctx)
}

func (pm *PmcManager) Stop(ctx context.Context) error {
	err := pm.registry.Stop(ctx)
	if err != nil {
		return err
	}

	return pm.credentialManager.Stop(ctx)
}

func (pm *PmcManager) Register(ctx context.Context, pmc *pmc.PMC) error {
	if err := pm.registry.RegisterPmc(ctx, pmc); err != nil {
		return fmt.Errorf("failed to register PMC (%s): %w", pmc.GetMac().String(), err)
	}

	if cred := pmc.GetCredential(); cred != nil {
		return pm.credentialManager.Put(ctx, pmc.GetMac(), cred)
	}
	return nil
}

// GetPmc resolves a PMC by MAC from the registry and attaches its credential from the credential manager.
func (pm *PmcManager) GetPmc(ctx context.Context, mac net.HardwareAddr) (*pmc.PMC, error) {
	pmc, err := pm.registry.GetPmc(ctx, mac)
	if err != nil {
		return nil, fmt.Errorf("failed to get PMC (%s) from registry: %w", mac.String(), err)
	}

	creds, err := pm.credentialManager.Get(context.Background(), mac)
	if err != nil {
		return nil, fmt.Errorf("failed to get credentials for PMC (%s): %w", mac.String(), err)
	}

	pmc.SetCredential(creds)

	return pmc, nil
}

func (pm *PmcManager) GetAllPmcs(ctx context.Context) ([]*pmc.PMC, error) {
	pmcs, err := pm.registry.GetAllPmcs(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to get PMCs from registry: %w", err)
	}

	for _, pmc := range pmcs {
		creds, err := pm.credentialManager.Get(context.Background(), pmc.MAC)
		if err != nil {
			return nil, fmt.Errorf("failed to get credentials for PMC (%s): %w", pmc.MAC.String(), err)
		}
		pmc.SetCredential(creds)
	}

	return pmcs, nil
}

func (pm *PmcManager) RedfishTx(ctx context.Context, pmc *pmc.PMC, tx func(client *redfish.RedfishClient) error) error {
	if pmc == nil {
		return errors.New("cannot query redfish with a null PMC")
	}

	ctx, cancel := context.WithTimeout(ctx, redfishTimeout)
	defer cancel()

	client, err := redfish.New(ctx, pmc, true)
	if err != nil {
		return err
	}
	defer client.Logout()

	return tx(client)
}

func (pm *PmcManager) PowerControl(ctx context.Context, mac net.HardwareAddr, on bool) error {
	pmc, err := pm.GetPmc(ctx, mac)
	if err != nil {
		return err
	}
	return pm.powerControlPmc(ctx, pmc, on)
}

// PowerControlDirect performs a power action on a PMC using pre-built connection
// details, bypassing registry and credential manager lookups.
func (pm *PmcManager) PowerControlDirect(ctx context.Context, pmc *pmc.PMC, on bool) error {
	return pm.powerControlPmc(ctx, pmc, on)
}

func (pm *PmcManager) powerControlPmc(ctx context.Context, pmc *pmc.PMC, on bool) error {
	action := "off"
	if on {
		action = "on"
	}
	log.Infof("Power %s initiated for %s", action, pmc.IP)

	tx := func(client *redfish.RedfishClient) error {
		if on {
			_, err := client.PowerOn()
			return err
		}
		_, err := client.PowerOff()
		return err
	}
	return pm.RedfishTx(ctx, pmc, tx)
}

func (pm *PmcManager) QueryPowerShelf(ctx context.Context, pmc *pmc.PMC) (*powershelf.PowerShelf, error) {
	if pmc == nil {
		return nil, errors.New("cannot query redfish with a null PMC")
	}

	client, err := redfish.New(ctx, pmc, true)
	if err != nil {
		return nil, err
	}
	defer client.Logout()

	return client.QueryPowerShelf()
}
