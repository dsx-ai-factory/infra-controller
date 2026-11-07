// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcregistry

import cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

// PmcRegisterType enumerates registry backends.
type PmcRegisterType string

const (
	RegisterTypePostgres PmcRegisterType = "Postgres"
	RegisterTypeInMemory PmcRegisterType = "InMemory"
)

// Config holds the selected backend type and DB configuration.
type Config struct {
	DSType PmcRegisterType
	DSConf cdb.Config
}

// StringToDSType converts a string to a PmcRegisterType.
func StringToDSType(s string) (PmcRegisterType, bool) {
	switch s {
	case string(RegisterTypePostgres):
		return RegisterTypePostgres, true
	case string(RegisterTypeInMemory):
		return RegisterTypeInMemory, true
	}

	return "", false
}
