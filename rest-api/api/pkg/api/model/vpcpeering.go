// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APIVpcPeeringCreateRequest captures the request data for creating a new VPC peering
type APIVpcPeeringCreateRequest struct {
	// The order of VPCs is not important, the VPC peering is bidirectional.
	// Vpc1ID is the ID of one VPC in the peering
	Vpc1ID string `json:"vpc1Id"`
	// Vpc2ID is the ID of the other VPC in the peering
	Vpc2ID string `json:"vpc2Id"`
	// SiteID is the ID of the Site where the peering exists
	SiteID string `json:"siteId"`
}

// Validate ensures the values passed in create request are acceptable
func (vpcr *APIVpcPeeringCreateRequest) Validate() error {
	err := validation.ValidateStruct(vpcr,
		validation.Field(&vpcr.Vpc1ID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&vpcr.Vpc2ID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&vpcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}

	// Validate that the VPCs are different
	if vpcr.Vpc1ID == vpcr.Vpc2ID {
		return validation.Errors{
			"vpc2Id": errors.New("Cannot be the same value as `vpc1Id`"),
		}
	}
	return nil
}

// ToProto builds the workflow request that asks a Site to create a new
// VPC peering for this API request. `vp` is the just-persisted DB
// record; its `ToProto()` is the source of the canonical wire fields
// (peering ID, VpcId, PeerVpcId).
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see (Site existence, VPC existence, RBAC). There is no
// request-only data layered on top of the entity for the peering
// create flow: the API request just carries the same VPC IDs that end
// up on the entity, so this is a thin wrapper around `vp.ToProto()`.
func (vpcr *APIVpcPeeringCreateRequest) ToProto(vp *cdbm.VpcPeering) *corev1.VpcPeeringCreationRequest {
	vpProto := vp.ToProto()
	return &corev1.VpcPeeringCreationRequest{
		Id:        vpProto.Id,
		VpcId:     vpProto.VpcId,
		PeerVpcId: vpProto.PeerVpcId,
	}
}

// APIVpcPeering represents a VPC Peering connection
type APIVpcPeering struct {
	// ID is the unique UUID v4 identifier of the VPC Peering
	ID string `json:"id"`
	// Vpc1ID is the ID of the first VPC in the Peering
	Vpc1ID string `json:"vpc1Id"`
	// Vpc1 is the summary of the first VPC in the Peering
	Vpc1 *APIVpcPeeringVpcSummary `json:"vpc1,omitempty"`
	// Vpc2ID is the ID of the second VPC in the peering
	Vpc2ID string `json:"vpc2Id"`
	// Vpc2 is the summary of the second VPC in the Peering
	Vpc2 *APIVpcPeeringVpcSummary `json:"vpc2,omitempty"`
	// SiteID is the ID of the Site where the Peering resides
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// TenantID is the ID of the Tenant that created the VPC peering
	TenantID *string `json:"tenantId,omitempty"`
	// Tenant is the summary of the tenant that created the VPC peering
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// IsMultiTenant indicates if this is a multi-tenant peering
	IsMultiTenant bool `json:"isMultiTenant"`
	// Status is the status of the VPC peering
	Status string `json:"status"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the VPC peering was last updated
	Updated time.Time `json:"updated"`
}

// APIVpcPeeringVpcSummary is summarizes a VPC in context of a VPC Peering
type APIVpcPeeringVpcSummary struct {
	// ID is the unique UUID v4 identifier of the VPC
	ID string `json:"id"`
	// Name of the Vpc
	Name string `json:"name"`
	// TenantID is the ID of the Tenant that owns the VPC
	TenantID string `json:"tenantId"`
	// Tenant describes details of the Tenant that owns the VPC
	Tenant *APIVpcPeeringTenantSummary `json:"tenant"`
	// Network virtualization type describe the VPC's virtualization type
	NetworkVirtualizationType *string `json:"networkVirtualizationType"`
	// Status is the status of the VPC
	Status string `json:"status"`
}

// APIVpcPeeringTenantSummary is summarizes a Tenant in context of a VPC Peering
type APIVpcPeeringTenantSummary struct {
	// ID of the Tenant
	ID string `json:"id"`
	// Org contains the org ID of the Tenant
	Org string `json:"org"`
	// OrgDisplayName contains the display name of Tenant's org
	OrgDisplayName *string `json:"orgDisplayName"`
}

// NewAPIVpcPeeringTenantSummary creates a new APIVpcPeeringTenantSummary from a database Tenant model
func NewAPIVpcPeeringTenantSummary(dbTenant *cdbm.Tenant) *APIVpcPeeringTenantSummary {
	return &APIVpcPeeringTenantSummary{
		ID:             dbTenant.ID.String(),
		Org:            dbTenant.Org,
		OrgDisplayName: dbTenant.OrgDisplayName,
	}
}

// NewAPIVpcPeeringSummary creates a new APIVpcPeeringSummary from a database VPC peering model
func NewAPIVpcPeeringVpcSummary(dbVpc *cdbm.Vpc, dbTenant *cdbm.Tenant) *APIVpcPeeringVpcSummary {
	if dbVpc == nil {
		return nil
	}

	apiVpcPeeringVpcSummary := APIVpcPeeringVpcSummary{
		ID:                        dbVpc.ID.String(),
		Name:                      dbVpc.Name,
		TenantID:                  dbVpc.TenantID.String(),
		NetworkVirtualizationType: dbVpc.NetworkVirtualizationType,
		Status:                    dbVpc.Status,
	}

	if dbTenant != nil {
		apiVpcPeeringVpcSummary.Tenant = NewAPIVpcPeeringTenantSummary(dbTenant)
	}

	return &apiVpcPeeringVpcSummary
}

// NewAPIVpcPeering creates a new APIVpcPeering from a database VPC peering model
func NewAPIVpcPeering(dbVpcPeering cdbm.VpcPeering, dbMappedPeeringTenants map[uuid.UUID]*cdbm.Tenant) APIVpcPeering {
	apiVpcPeering := APIVpcPeering{
		ID:            dbVpcPeering.ID.String(),
		Vpc1ID:        dbVpcPeering.Vpc1ID.String(),
		Vpc2ID:        dbVpcPeering.Vpc2ID.String(),
		SiteID:        dbVpcPeering.SiteID.String(),
		IsMultiTenant: dbVpcPeering.IsMultiTenant,
		Status:        dbVpcPeering.Status,
		Created:       dbVpcPeering.Created,
		Updated:       dbVpcPeering.Updated,
	}

	// Expand relations if available.
	if dbVpcPeering.Vpc1 != nil {
		apiVpcPeering.Vpc1 = NewAPIVpcPeeringVpcSummary(dbVpcPeering.Vpc1, nil)
		peerTenant1, _ := dbMappedPeeringTenants[dbVpcPeering.Vpc1.TenantID]
		apiVpcPeering.Vpc1 = NewAPIVpcPeeringVpcSummary(dbVpcPeering.Vpc1, peerTenant1)
	}

	if dbVpcPeering.Vpc2 != nil {
		apiVpcPeering.Vpc2 = NewAPIVpcPeeringVpcSummary(dbVpcPeering.Vpc2, nil)
		peerTenant2, _ := dbMappedPeeringTenants[dbVpcPeering.Vpc2.TenantID]
		apiVpcPeering.Vpc2 = NewAPIVpcPeeringVpcSummary(dbVpcPeering.Vpc2, peerTenant2)
	}

	if dbVpcPeering.Site != nil {
		apiVpcPeering.Site = NewAPISiteSummary(dbVpcPeering.Site)
	}

	if dbVpcPeering.Tenant != nil {
		apiVpcPeering.TenantID = util.GetUUIDPtrToStrPtr(dbVpcPeering.TenantID)
		apiVpcPeering.Tenant = NewAPITenantSummary(dbVpcPeering.Tenant)
	}
	return apiVpcPeering
}
