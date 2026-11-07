// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchmanager

import (
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/credentials"

	"github.com/uptrace/bun"
)

// DataStoreType represents the backing datastore for the manager.
type DataStoreType string

const (
	DatastoreTypeInMemory   DataStoreType = "inmemory"
	DatastoreTypePersistent DataStoreType = "persistent"
)

// Config specifies the configuration for the NVSwitchManager.
type Config struct {
	DSType         DataStoreType
	CredentialConf credentials.Config
	DB             *bun.DB // Database connection for persistent storage
}
