// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APISSHKeyAssociation is the data structure to capture API representation of an sshkey association
type APISSHKeyAssociation struct {
	// ID is the unique UUID v4 identifier for the security policy
	ID string `json:"id"`
	// SSHKeyID is the ID of the associated SSHKey
	SSHKeyID string `json:"sshKeyId"`
	// SSHKeyGroupID is the ID of the SSHKeyGroup
	SSHKeyGroupID string `json:"entityId"`
	// Created indicates the ISO datetime string for when the site was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the site was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISSHKeyAssociation accepts a DB layer SSHKeyAssociation object and returns an API object
func NewAPISSHKeyAssociation(ska *cdbm.SSHKeyAssociation) *APISSHKeyAssociation {
	apiska := &APISSHKeyAssociation{
		ID:            ska.ID.String(),
		SSHKeyID:      ska.SSHKeyID.String(),
		SSHKeyGroupID: ska.SSHKeyGroupID.String(),
		Created:       ska.Created,
		Updated:       ska.Updated,
	}

	return apiska
}
