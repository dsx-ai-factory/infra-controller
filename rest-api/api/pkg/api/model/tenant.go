// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	// errMsgTenantCreateEndpointDeprecated is the error message to indicate that create endpoint is deprecated
	ErrMsgTenantCreateEndpointDeprecated = "POST '/org/:orgName/nico/tenant' endpoint has been deprecated"

	// errMsgTenantUpdateEndpointDeprecated is the error message to indicate that update endpoint is deprecated
	ErrMsgTenantUpdateEndpointDeprecated = "PATCH '/org/:orgName/nico/tenant/current' endpoint has been deprecated"

	// Time when Tenant.capabilities.targetedInstanceCreation will be removed. The
	// capabilities object remains in responses for compatibility, so the
	// deprecation notice retains a future TakeActionBy deadline.
	tenantTargetedInstanceCreationDeprecationTime = time.Date(2026, time.October, 1, 0, 0, 0, 0, time.UTC)

	tenantCapabilityDeprecations = []DeprecatedEntity{
		{
			OldValue:     "capabilities.targetedInstanceCreation",
			NewValue:     cutil.GetPtr("tenantAccount.siteCapabilities"),
			Type:         DeprecationTypeAttribute,
			TakeActionBy: tenantTargetedInstanceCreationDeprecationTime,
		},
	}
)

// APITenant is the data structure to capture API representation of a Tenant
type APITenant struct {
	// ID is the unique UUID v4 identifier for the Tenant
	ID string `json:"id"`
	// Org contains the name of the org the Tenant belongs to
	Org string `json:"org"`
	// OrgDisplayName contains the display name of the org the Tenant belongs to
	OrgDisplayName *string `json:"orgDisplayName"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
	// Capabilities is the deprecated tenant-wide compatibility object
	Capabilities *APITenantCapabilities `json:"capabilities"`
	// Deprecations is the list of deprecations for the Tenant
	Deprecations []APIDeprecation `json:"deprecations"`
}

// APITenantRoutingProfile describes the routing profiles a Tenant may select
// for VPC creation at one Site.
type APITenantRoutingProfile struct {
	// DefaultRoutingProfile is the profile Core applies when a VPC omits routingProfile.
	DefaultRoutingProfile string `json:"defaultRoutingProfile"`
	// PermittedRoutingProfiles contains the profiles this Tenant may choose during VPC creation.
	// Choosing DefaultRoutingProfile inherits it without sending an explicit override.
	PermittedRoutingProfiles []string `json:"permittedRoutingProfiles"`
}

// FromProto populates the REST response from Core's Tenant lookup. When the
// Tenant lacks the site-scoped write privilege, only its inheritable default is
// exposed as selectable.
func (atrp *APITenantRoutingProfile) FromProto(response *corev1.FindTenantResponse, allowAlternatives bool) {
	*atrp = APITenantRoutingProfile{PermittedRoutingProfiles: []string{}}
	if response == nil || response.GetTenant() == nil {
		return
	}

	atrp.DefaultRoutingProfile = NormalizeAPIVpcRoutingProfileFromSite(response.GetTenant().GetRoutingProfileType())
	if !allowAlternatives {
		if atrp.DefaultRoutingProfile != "" {
			atrp.PermittedRoutingProfiles = append(atrp.PermittedRoutingProfiles, atrp.DefaultRoutingProfile)
		}
		return
	}

	for _, profile := range response.GetPermittedRoutingProfileTypes() {
		atrp.PermittedRoutingProfiles = append(atrp.PermittedRoutingProfiles, NormalizeAPIVpcRoutingProfileFromSite(profile))
	}
}

// NewAPITenant accepts a DB layer Tenant object and the deprecated tenant-wide
// TargetedInstanceCreation compatibility value, then returns an API layer object.
func NewAPITenant(dbtn *cdbm.Tenant, targetedInstanceCreation bool) *APITenant {
	atn := APITenant{
		ID:             dbtn.ID.String(),
		Org:            dbtn.Org,
		OrgDisplayName: dbtn.OrgDisplayName,
		Capabilities:   tenantToAPITenantCapabilities(targetedInstanceCreation),
		Created:        dbtn.Created,
		Updated:        dbtn.Updated,
	}

	for _, deprecation := range tenantCapabilityDeprecations {
		atn.Deprecations = append(atn.Deprecations, NewAPIDeprecation(deprecation))
	}

	return &atn
}

// APITenantCapabilities holds the model of tenant capabilities.
type APITenantCapabilities struct {
	// TargetedInstanceCreation is present only when the compatibility aggregate is enabled
	TargetedInstanceCreation *bool `json:"targetedInstanceCreation,omitempty"`
}

func tenantToAPITenantCapabilities(targetedInstanceCreation bool) *APITenantCapabilities {
	capabilities := &APITenantCapabilities{}
	if targetedInstanceCreation {
		capabilities.TargetedInstanceCreation = cutil.GetPtr(true)
	}
	return capabilities
}

// APITenantSummary is the data structure to capture API representation of a Tenant Summary
type APITenantSummary struct {
	// Org contains the name of the org this tenant belongs to
	Org string `json:"org"`
	// OrgDisplayName contains the display name of the org the Tenant belongs to
	OrgDisplayName *string `json:"orgDisplayName"`
	// Capabilities is retained as an empty compatibility object in summaries
	Capabilities *APITenantCapabilities `json:"capabilities"`
	// Deprecations is the list of deprecations for the Tenant
	Deprecations []APIDeprecation `json:"deprecations"`
}

// NewAPITenantSummary accepts a DB layer APITenantSummary object returns an API layer object
func NewAPITenantSummary(dbtn *cdbm.Tenant) *APITenantSummary {
	atns := APITenantSummary{
		Org:            dbtn.Org,
		OrgDisplayName: dbtn.OrgDisplayName,
		Capabilities:   tenantToAPITenantCapabilities(false),
	}

	for _, deprecation := range tenantCapabilityDeprecations {
		atns.Deprecations = append(atns.Deprecations, NewAPIDeprecation(deprecation))
	}

	return &atns
}

// APITenantStats is the data structure to capture API representation of a Tenant Stats
type APITenantStats struct {
	// Instance holds aggregated instance status counts for the tenant.
	Instance APIInstanceStats `json:"instance"`
	// Vpc is the data structure to capture API representation of a Vpc Stats associated with tenant
	Vpc APIVpcStats `json:"vpc"`
	// Subnet is the data structure to capture API representation of a Subnet Stats associated with tenant
	Subnet APISubnetStats `json:"subnet"`
	// TenantAccount is the data structure to capture API representation of a TenantAccount Stats associated with tenant
	TenantAccount APITenantAccountStats `json:"tenantAccount"`
}

// NewAPITenantStats accepts stats for each object and returns an API layer object
func NewAPITenantStats(instanceStats APIInstanceStats, vpcstatsmap map[string]int, subnetstatmap map[string]int, tastatsmap map[string]int) *APITenantStats {
	ats := APITenantStats{
		Vpc: APIVpcStats{
			Total:        vpcstatsmap["total"],
			Pending:      vpcstatsmap[cdbm.VpcStatusPending],
			Provisioning: vpcstatsmap[cdbm.VpcStatusProvisioning],
			Ready:        vpcstatsmap[cdbm.VpcStatusReady],
			Error:        vpcstatsmap[cdbm.VpcStatusError],
			Deleting:     vpcstatsmap[cdbm.VpcStatusDeleting],
		},
		Instance: instanceStats,
		Subnet: APISubnetStats{
			Total:        subnetstatmap["total"],
			Pending:      subnetstatmap[cdbm.SubnetStatusPending],
			Provisioning: subnetstatmap[cdbm.SubnetStatusProvisioning],
			Ready:        subnetstatmap[cdbm.SubnetStatusReady],
			Error:        subnetstatmap[cdbm.SubnetStatusError],
			Deleting:     subnetstatmap[cdbm.SubnetStatusDeleting],
		},
		TenantAccount: APITenantAccountStats{
			Total:   tastatsmap["total"],
			Pending: tastatsmap[cdbm.TenantAccountStatusPending],
			Invited: tastatsmap[cdbm.TenantAccountStatusInvited],
			Ready:   tastatsmap[cdbm.TenantAccountStatusReady],
			Error:   tastatsmap[cdbm.TenantAccountStatusError],
		},
	}

	return &ats
}
