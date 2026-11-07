// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIOperatingSystemSiteAssociation is the data structure to capture API representation of an sshkey association
type APIOperatingSystemSiteAssociation struct {
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site"`
	// Version is the version of corresponding image on Site
	Version *string `json:"version"`
	// Status is the status of the OperatingSystemSiteAssociation
	Status string `json:"status"`
	// Created indicates the ISO datetime string for when the site was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the site was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIOperatingSystemSiteAssociation accepts a DB layer OperatingSystemSiteAssociation object and returns an API object
func NewAPIOperatingSystemSiteAssociation(dbossa *cdbm.OperatingSystemSiteAssociation, ts *cdbm.TenantSite) *APIOperatingSystemSiteAssociation {
	apiossa := &APIOperatingSystemSiteAssociation{
		Version: dbossa.Version,
		Status:  dbossa.Status,
		Created: dbossa.Created,
		Updated: dbossa.Updated,
	}

	if dbossa.Site != nil {
		apiossa.Site = NewAPISiteSummary(dbossa.Site)
	}

	return apiossa
}
