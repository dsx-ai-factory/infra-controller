// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIFabric is the data structure to capture API representation of a Fabric
type APIFabric struct {
	// ID is the guid identifier for the Fabric
	ID string `json:"id"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// Status represents the status of the machine
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the Fabric
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIFabric accepts a DB layer Fabric object and returns an API object
func NewAPIFabric(dbf *cdbm.Fabric, dbsds []cdbm.StatusDetail) *APIFabric {
	apif := &APIFabric{
		ID:                       dbf.ID,
		InfrastructureProviderID: dbf.InfrastructureProviderID.String(),
		SiteID:                   dbf.SiteID.String(),
		Status:                   dbf.Status,
		Created:                  dbf.Created,
		Updated:                  dbf.Updated,
	}

	if dbf.InfrastructureProvider != nil {
		apif.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbf.InfrastructureProvider)
	}

	if dbf.Site != nil {
		apif.Site = NewAPISiteSummary(dbf.Site)
	}
	apif.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apif.StatusHistory = append(apif.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return apif
}

// APIFabricSummary is the data structure to capture API summary of a Fabric
type APIFabricSummary struct {
	// ID of the Fabric
	ID string `json:"id"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Status is the status of the Fabric
	Status string `json:"status"`
}

// NewAPIFabricSummary accepts a DB layer Fabric object returns an API layer object
func NewAPIFabricSummary(dbfb *cdbm.Fabric) *APIFabricSummary {
	afb := APIFabricSummary{
		ID:     dbfb.ID,
		SiteID: dbfb.SiteID.String(),
		Status: dbfb.Status,
	}
	return &afb
}
