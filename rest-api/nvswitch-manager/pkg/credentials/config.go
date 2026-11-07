// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"errors"
	"fmt"
)

// DataStoreType selects credential store backend.
type DataStoreType string

const (
	DatastoreTypeVault    DataStoreType = "Vault"
	DatastoreTypeInMemory DataStoreType = "InMemory"
)

// Config holds the selected backend and provider config (Vault).
type Config struct {
	DataStoreType DataStoreType
	VaultConfig   *VaultConfig
}

func (c *Config) String() string {
	return fmt.Sprintf("DataStoreType: %s; VaultConfig: %v", c.DataStoreType, c.VaultConfig)
}

// Validate checks if the Config fields are set correctly.
func (c *Config) Validate() error {
	switch c.DataStoreType {
	case DatastoreTypeVault:
		if c.VaultConfig == nil {
			return errors.New("vault config needs to be specified when using Vault as the credential manager datastore")
		}

		return c.VaultConfig.Validate()
	}
	return nil
}
