// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"os"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/pmcregistry"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"
)

// Config captures runtime settings for running the gRPC service, including the public port,
// the datastore mode (Persistent or InMemory), and concrete configurations for the PMC registry and credential manager backends.
type Config struct {
	Port          int
	DataStoreType powershelfmanager.DataStoreType
	VaultConf     credentials.VaultConfig
	DBConf        cdb.Config
	FirmwareDir   string
}

// toCredentialManagerConf converts the public service Config into a pmcregistry.Config,
// mapping datastore selection to a concrete registry backend.
func (c *Config) toCredentialManagerConf() (*credentials.Config, error) {
	var dataStoreType credentials.DataStoreType
	var vaultConfig *credentials.VaultConfig
	switch c.DataStoreType {
	case powershelfmanager.DatastoreTypePersistent:
		dataStoreType = credentials.DatastoreTypeVault
		vaultConfig = &credentials.VaultConfig{Address: c.VaultConf.Address, Token: c.VaultConf.Token}
	case powershelfmanager.DatastoreTypeInMemory:
		dataStoreType = credentials.DatastoreTypeInMemory
	}
	return &credentials.Config{
		DataStoreType: dataStoreType,
		VaultConfig:   vaultConfig,
	}, nil
}

// toDataStoreConf converts the public service Config into a pmcregistry.Config,
// mapping datastore selection to a concrete credential backend.
func (c *Config) toDataStoreConf() (*pmcregistry.Config, error) {
	var pmcRegistryType pmcregistry.PmcRegisterType
	switch c.DataStoreType {
	case powershelfmanager.DatastoreTypePersistent:
		pmcRegistryType = pmcregistry.RegisterTypePostgres
	case powershelfmanager.DatastoreTypeInMemory:
		pmcRegistryType = pmcregistry.RegisterTypeInMemory
	}
	return &pmcregistry.Config{
		DSType: pmcRegistryType,
		DSConf: c.DBConf,
	}, nil
}

// ToPsmConf converts the public service Config into a powershelfmanager.Config,
// mapping datastore selection to concrete registry and credential backends.
func (c *Config) ToPsmConf() (*powershelfmanager.Config, error) {
	credentialManagerConf, err := c.toCredentialManagerConf()
	if err != nil {
		return nil, err
	}

	dataStoreConf, err := c.toDataStoreConf()
	if err != nil {
		return nil, err
	}

	psmConf := powershelfmanager.Config{
		DSType:          c.DataStoreType,
		CredentialConf:  *credentialManagerConf,
		PmcRegistryConf: *dataStoreConf,
		FirmwareDir:     c.FirmwareDir,
	}

	return &psmConf, nil
}

// BuildDBConfigFromEnv builds cdb.Config from environment variables.
// Delegates to cdb.ConfigFromEnv() and returns a pointer for backward compatibility.
func BuildDBConfigFromEnv() (*cdb.Config, error) {
	c, err := cdb.ConfigFromEnv()
	if err != nil {
		return nil, err
	}
	return &c, nil
}

// BuildVaultConfigFromEnv builds credentials.VaultConfig from environment variables (VAULT_ADDR, VAULT_TOKEN).
func BuildVaultConfigFromEnv() (*credentials.VaultConfig, error) {
	return &credentials.VaultConfig{
		Address: os.Getenv("VAULT_ADDR"),
		Token:   os.Getenv("VAULT_TOKEN"),
	}, nil
}
