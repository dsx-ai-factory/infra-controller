// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"fmt"
	"net"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"

	log "github.com/sirupsen/logrus"
)

// CredentialManager defines a key-value store for BMC and NVOS credentials keyed by MAC address.
type CredentialManager interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error

	// BMC credential operations
	GetBMC(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error)
	PutBMC(ctx context.Context, mac net.HardwareAddr, credentials *credential.Credential) error
	PatchBMC(ctx context.Context, mac net.HardwareAddr, credentials *credential.Credential) error
	DeleteBMC(ctx context.Context, mac net.HardwareAddr) error

	// NVOS credential operations
	GetNVOS(ctx context.Context, mac net.HardwareAddr) (*credential.Credential, error)
	PutNVOS(ctx context.Context, mac net.HardwareAddr, credentials *credential.Credential) error
	PatchNVOS(ctx context.Context, mac net.HardwareAddr, credentials *credential.Credential) error
	DeleteNVOS(ctx context.Context, mac net.HardwareAddr) error

	// List all registered MACs
	Keys(ctx context.Context) ([]net.HardwareAddr, error)
}

// New creates a new Credential Manager based on the given configuration.
func New(ctx context.Context, config *Config) (CredentialManager, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}

	switch config.DataStoreType {
	case DatastoreTypeVault:
		log.Printf("Initializing CredentialManager with vault datastore (config: %s)", config.VaultConfig)
		return config.VaultConfig.NewManager()
	case DatastoreTypeInMemory:
		log.Printf("Initializing CredentialManager with in-memory datastore")
		return NewInMemoryCredentialManager(), nil
	}

	return nil, fmt.Errorf("unsupported datastore type %s", config.DataStoreType)
}
