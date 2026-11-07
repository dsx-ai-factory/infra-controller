// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
)

// APISSHKeyGroupCreateRequest is the data structure to capture instance request to create a new SSHKeyGroup
type APISSHKeyGroupCreateRequest struct {
	// Name is the name of the SSHKeyGroup
	Name string `json:"name"`
	// Description is the description of the SSHKeyGroup
	Description *string `json:"description"`
	// SiteIDs is a list of Site objects
	SiteIDs []string `json:"siteIds"`
	// SSHKeyIDs is a list of SSHKeyID objects
	SSHKeyIDs []string `json:"sshKeyIds"`
}

// Validate ensures that the values passed in request are acceptable
func (sgcr APISSHKeyGroupCreateRequest) Validate() error {
	err := validation.ValidateStruct(&sgcr,
		validation.Field(&sgcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
	)
	if err != nil {
		return err
	}
	return nil
}

// APISSHKeyGroupUpdateRequest is the data structure to capture user request to update a SSHKeyGroup
type APISSHKeyGroupUpdateRequest struct {
	// Name is the name of the SSHKeyGroup
	Name *string `json:"name"`
	// Description is the description of the SSHKeyGroup
	Description *string `json:"description"`
	// SiteIDs is a list of Site objects
	SiteIDs []string `json:"siteIds"`
	// SSHKeyIDs is a list of SSHKeyID objects
	SSHKeyIDs []string `json:"sshKeyIds"`
	// Version is the keyset version of the SSHKeyGroup
	Version *string `json:"version"`
}

// Validate ensure the values passed in request are acceptable
func (sgur APISSHKeyGroupUpdateRequest) Validate() error {
	return validation.ValidateStruct(&sgur,
		validation.Field(&sgur.Name,
			validation.When(sgur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(sgur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(sgur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&sgur.Version,
			validation.Required.Error(validationErrorValueRequired)),
	)
}

// APISSHKeyGroup is the data structure to capture API representation of a SSHKeyGroup
type APISSHKeyGroup struct {
	// ID is the unique UUID v4 identifier for the SSHKeyGroup
	ID string `json:"id"`
	// Name is the name of the SSHKeyGroup
	Name string `json:"name"`
	// Description is the description of the SSHKeyGroup
	Description *string `json:"description"`
	// Org is the organization the SSHKeyGroup belongs to
	Org string `json:"org"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Version is the keyset version for the SSHKeyGroups
	Version *string `json:"version"`
	// Status is the status of the SSHKeyGroups
	Status string `json:"status"`
	// StatusHistory is the status detail records for the SSHKeyGroups over time
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// SSHKeys is the list of sshkeys associated with the sshkey group
	SSHKeys []APISSHKey `json:"sshKeys"`
	// SiteAssociations is the list of sites associated with the sshkey group
	SiteAssociations []APISSHKeyGroupSiteAssociation `json:"siteAssociations"`
	// Created indicates the ISO datetime string for when the SSHKeyGroup was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the SSHKeyGroup was last updated
	Updated time.Time `json:"updated"`
}

// APISSHKeyGroupSummary is the data structure to capture API summary of a SSHKeyGroup
type APISSHKeyGroupSummary struct {
	// ID is the unique UUID v4 identifier for the SSHKeyGroup
	ID string `json:"id"`
	// Name of the Site, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// Description is the description of the SSHKeyGroup
	Description *string `json:"description"`
	// Version is the keyset version for the SSHKeyGroups
	Version *string `json:"version"`
	// Status is the status of the site
	Status string `json:"status"`
	// Created indicates the ISO datetime string for when the SSHKeyGroup was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the SSHKeyGroup was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISSHKeyGroupSummary accepts a DB layer SSHKeyGroup object returns an API layer object
func NewAPISSHKeyGroupSummary(skg *cdbm.SSHKeyGroup) *APISSHKeyGroupSummary {
	apiskgs := APISSHKeyGroupSummary{
		ID:          skg.ID.String(),
		Name:        skg.Name,
		Description: skg.Description,
		Version:     skg.Version,
		Status:      skg.Status,
		Created:     skg.Created,
		Updated:     skg.Updated,
	}
	return &apiskgs
}

// NewAPISSHKeyGroup accepts a DB layer SSHKeyGroup object and returns an API object
func NewAPISSHKeyGroup(skg *cdbm.SSHKeyGroup, skgsas []cdbm.SSHKeyGroupSiteAssociation, sttsmap map[uuid.UUID]*cdbm.TenantSite, sks []cdbm.SSHKeyAssociation, dbsds []cdbm.StatusDetail) *APISSHKeyGroup {
	apiskg := &APISSHKeyGroup{
		ID:          skg.ID.String(),
		Name:        skg.Name,
		Description: skg.Description,
		Org:         skg.Org,
		TenantID:    skg.TenantID.String(),
		Version:     skg.Version,
		Status:      skg.Status,
		Created:     skg.Created,
		Updated:     skg.Updated,
	}

	if skg.Tenant != nil {
		apiskg.Tenant = NewAPITenantSummary(skg.Tenant)
	}

	apiskg.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiskg.StatusHistory = append(apiskg.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	apiskg.SiteAssociations = []APISSHKeyGroupSiteAssociation{}
	for _, skgsa := range skgsas {
		ts := sttsmap[skgsa.SiteID]
		cskgsa := skgsa
		apiskg.SiteAssociations = append(apiskg.SiteAssociations, *NewAPISSHKeyGroupSiteAssociation(&cskgsa, ts))
	}

	apiskg.SSHKeys = []APISSHKey{}
	for _, sk := range sks {
		if sk.SSHKey != nil {
			apiskg.SSHKeys = append(apiskg.SSHKeys, *NewAPISSHKey(sk.SSHKey, nil))
		}
	}

	return apiskg
}
