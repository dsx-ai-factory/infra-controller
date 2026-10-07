// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

var (
	// errMsgproviderCreateEndpointDeprecated is the error message to indicate that create endpoint is deprecated
	ErrMsgproviderCreateEndpointDeprecated = "POST '/org/:orgName/nico/infrastructure-provider' endpoint has been deprecated"

	// errMsgproviderUpdateEndpointDeprecated is the error message to indicate that update endpoint is deprecated
	ErrMsgproviderUpdateEndpointDeprecated = "PATCH '/org/:orgName/nico/infrastructure-provider/current' endpoint has been deprecated"
)

// APIInfrastructureProvider is the data structure to capture API representation of an Infrastructure Provider
type APIInfrastructureProvider struct {
	// ID is the unique UUID v4 identifier for the Infrastructure Provider
	ID string `json:"id"`
	// Org contains the name of the org this Infrastructure Provider belongs to
	Org string `json:"org"`
	// OrgDisplayName contains the display name of the org this Infrastructure Provider belongs to
	OrgDisplayName *string `json:"orgDisplayName"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIInfrastructureProvider accepts a DB layer InfrastructureProvider object returns an API layer object
func NewAPIInfrastructureProvider(dbip *cdbm.InfrastructureProvider) *APIInfrastructureProvider {
	aip := APIInfrastructureProvider{
		ID:             dbip.ID.String(),
		Org:            dbip.Org,
		OrgDisplayName: dbip.OrgDisplayName,
		Created:        dbip.Created,
		Updated:        dbip.Updated,
	}

	return &aip
}

// APIInfrastructureProviderSummary is the data structure to capture API representation of an Infrastructure Provider
type APIInfrastructureProviderSummary struct {
	// Org contains the name of the org the Infrastructure Provider belongs to
	Org string `json:"org"`
	// OrgDisplayName contains the display name of the org this Infrastructure Provider belongs to
	OrgDisplayName *string `json:"orgDisplayName"`
}

// NewAPIInfrastructureProviderSummary accepts a DB layer InfrastructureProvider object returns an API layer object
func NewAPIInfrastructureProviderSummary(dbip *cdbm.InfrastructureProvider) *APIInfrastructureProviderSummary {
	aips := APIInfrastructureProviderSummary{
		Org:            dbip.Org,
		OrgDisplayName: dbip.OrgDisplayName,
	}

	return &aips
}

// APIInfrastructureProviderStats is the data structure to capture API representation of an Infrastructure Provider
type APIInfrastructureProviderStats struct {
	// Machine is the data structure to capture API representation of an Machine Stats associated with Infrastructure Provider
	Machine APIMachineStats `json:"machine"`
	// IPBlock is the data structure to capture API representation of a IPBlock Stats associated with Infrastructure Provider
	IPBlock APIIPBlockStats `json:"ipBlock"`
	// TenantAccount is the data structure to capture API representation of a TenantAccount Stats associated with Infrastructure Provider
	TenantAccount APITenantAccountStats `json:"tenantAccount"`
}

// NewAPIInfrastructureProviderStats accepts map that represents stats for the each objects and returns an API layer object
func NewAPIInfrastructureProviderStats(mcstatsmap map[string]int, ipbstatsmap map[string]int, tastatsmap map[string]int) *APIInfrastructureProviderStats {
	aips := APIInfrastructureProviderStats{
		Machine: APIMachineStats{
			Total:           mcstatsmap["total"],
			Initializing:    mcstatsmap[cdbm.MachineStatusInitializing],
			Reset:           mcstatsmap[cdbm.MachineStatusReset],
			Ready:           mcstatsmap[cdbm.MachineStatusReady],
			InUse:           mcstatsmap[cdbm.MachineStatusInUse],
			Decommissioning: mcstatsmap[cdbm.MachineStatusDecommissioning],
			Decommissioned:  mcstatsmap[cdbm.MachineStatusDecommissioned],
			Unknown:         mcstatsmap[cdbm.MachineStatusUnknown],
			Error:           mcstatsmap[cdbm.MachineStatusError],
			Maintenance:     mcstatsmap[cdbm.MachineStatusMaintenance],
		},
		IPBlock: APIIPBlockStats{
			Total:        ipbstatsmap["total"],
			Pending:      ipbstatsmap[cdbm.IPBlockStatusPending],
			Provisioning: ipbstatsmap[cdbm.IPBlockStatusProvisioning],
			Ready:        ipbstatsmap[cdbm.IPBlockStatusReady],
			Error:        ipbstatsmap[cdbm.IPBlockStatusError],
			Deleting:     ipbstatsmap[cdbm.IPBlockStatusDeleting],
		},
		TenantAccount: APITenantAccountStats{
			Total:   tastatsmap["total"],
			Pending: tastatsmap[cdbm.TenantAccountStatusPending],
			Invited: tastatsmap[cdbm.TenantAccountStatusInvited],
			Ready:   tastatsmap[cdbm.TenantAccountStatusReady],
			Error:   tastatsmap[cdbm.TenantAccountStatusError],
		},
	}

	return &aips
}
