// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// type APIServiceAccount is the data structure to capture API representation of a Service Account
type APIServiceAccount struct {
	// Enabled is a flag to indicate if the Service Account is enabled
	Enabled bool `json:"enabled"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider
	InfrastructureProviderID *string `json:"infrastructureProviderId"`
	// ID is the unique UUID v4 identifier for the Service Account
	TenantID *string `json:"tenantId"`
}

// NewAPIServiceAccount accepts a DB layer ServiceAccount object and returns an API object
func NewAPIServiceAccount(serviceAccountEnabled bool, dbProvider *cdbm.InfrastructureProvider, dbTenant *cdbm.Tenant) *APIServiceAccount {
	apiServiceAccount := APIServiceAccount{
		Enabled: serviceAccountEnabled,
	}

	if dbProvider != nil {
		apiServiceAccount.InfrastructureProviderID = cutil.GetPtr(dbProvider.ID.String())
	}
	if dbTenant != nil {
		apiServiceAccount.TenantID = cutil.GetPtr(dbTenant.ID.String())
	}

	return &apiServiceAccount
}
