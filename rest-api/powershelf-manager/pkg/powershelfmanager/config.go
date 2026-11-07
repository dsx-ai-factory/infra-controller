// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powershelfmanager

import (
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/pmcregistry"
)

// DataStoreType selects between Persistent (Postgres+Vault) and InMemory backends.
type DataStoreType string

const (
	DatastoreTypePersistent DataStoreType = "Persistent"
	DatastoreTypeInMemory   DataStoreType = "InMemory"
)

// Config contains the orchestrator’s datastore mode and concrete backends for the PMC registry and the credential manager.
type Config struct {
	DSType          DataStoreType
	PmcRegistryConf pmcregistry.Config
	CredentialConf  credentials.Config
	FirmwareDir     string
}

// StringToDSType converts a string to a DataStoreType, returning false if unsupported.
func StringToDSType(s string) (DataStoreType, bool) {
	switch s {
	case string(DatastoreTypePersistent):
		return DatastoreTypePersistent, true
	case string(DatastoreTypeInMemory):
		return DatastoreTypeInMemory, true
	}

	return "", false
}
