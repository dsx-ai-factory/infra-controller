// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"errors"
	"os"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchmanager"
)

// Config captures runtime settings for running the gRPC service.
type Config struct {
	Port          int
	DataStoreType nvswitchmanager.DataStoreType
	VaultConf     credentials.VaultConfig
	DBConf        db.Config
	FirmwareConf  FirmwareConfig
}

// FirmwareConfig contains firmware manager configuration.
type FirmwareConfig struct {
	PackagesDir       string        // Directory containing firmware package YAML definitions
	FirmwareDir       string        // Directory containing firmware files
	NumWorkers        int           // Number of concurrent update workers
	SchedulerInterval time.Duration // How often the scheduler queries for pending updates
}

// ToFirmwareManagerConfig converts FirmwareConfig to firmwaremanager.Config.
func (c *FirmwareConfig) ToFirmwareManagerConfig() firmwaremanager.Config {
	return firmwaremanager.Config{
		PackagesDir:       c.PackagesDir,
		FirmwareDir:       c.FirmwareDir,
		NumWorkers:        c.NumWorkers,
		SchedulerInterval: c.SchedulerInterval,
	}
}

// toCredentialManagerConf converts the service Config into a credentials.Config.
func (c *Config) toCredentialManagerConf() (*credentials.Config, error) {
	var dataStoreType credentials.DataStoreType
	var vaultConfig *credentials.VaultConfig
	switch c.DataStoreType {
	case nvswitchmanager.DatastoreTypePersistent:
		dataStoreType = credentials.DatastoreTypeVault
		vaultConfig = &credentials.VaultConfig{Address: c.VaultConf.Address, Token: c.VaultConf.Token}
	case nvswitchmanager.DatastoreTypeInMemory:
		dataStoreType = credentials.DatastoreTypeInMemory
	}
	return &credentials.Config{
		DataStoreType: dataStoreType,
		VaultConfig:   vaultConfig,
	}, nil
}

// ToNsmConf converts the service Config into a nvswitchmanager.Config.
// The DB connection must be set separately by the caller.
func (c *Config) ToNsmConf() (*nvswitchmanager.Config, error) {
	credentialManagerConf, err := c.toCredentialManagerConf()
	if err != nil {
		return nil, err
	}

	return &nvswitchmanager.Config{
		DSType:         c.DataStoreType,
		CredentialConf: *credentialManagerConf,
		// DB must be set by caller when using persistent storage
	}, nil
}

// BuildDBConfigFromEnv builds db.Config from environment variables.
func BuildDBConfigFromEnv() (*db.Config, error) {
	port, err := strconv.Atoi(os.Getenv("DB_PORT"))
	if err != nil {
		return nil, errors.New("fail to retrieve port")
	}

	dbConf := db.Config{
		Host:       os.Getenv("DB_ADDR"),
		Port:       port,
		Credential: credential.NewFromEnv("DB_USER", "DB_PASSWORD"),
		DBName:     os.Getenv("DB_DATABASE"),
	}

	return &dbConf, nil
}

// BuildVaultConfigFromEnv builds credentials.VaultConfig from environment variables.
func BuildVaultConfigFromEnv() (*credentials.VaultConfig, error) {
	return &credentials.VaultConfig{
		Address: os.Getenv("VAULT_ADDR"),
		Token:   os.Getenv("VAULT_TOKEN"),
	}, nil
}
