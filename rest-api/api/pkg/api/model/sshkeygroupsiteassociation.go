// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APISSHKeyGroupSiteAssociation is the data structure to capture API representation of an sshkey association
type APISSHKeyGroupSiteAssociation struct {
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site"`
	// ControllerKeySetVersion is the version of corresponding keyset on Site
	ControllerKeySetVersion *string `json:"version"`
	// Status is the status of the SSHKeyGroupSiteAssociation
	Status string `json:"status"`
	// Created indicates the ISO datetime string for when the site was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the site was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISSHKeyGroupSiteAssociation accepts a DB layer SSHKeyGroupSiteAssociation object and returns an API object
func NewAPISSHKeyGroupSiteAssociation(dbskgsa *cdbm.SSHKeyGroupSiteAssociation, ts *cdbm.TenantSite) *APISSHKeyGroupSiteAssociation {
	apiskgsa := &APISSHKeyGroupSiteAssociation{
		ControllerKeySetVersion: dbskgsa.Version,
		Status:                  dbskgsa.Status,
		Created:                 dbskgsa.Created,
		Updated:                 dbskgsa.Updated,
	}

	if dbskgsa.Site != nil {
		apiskgsa.Site = NewAPISiteSummary(dbskgsa.Site)
	}

	return apiskgsa
}
