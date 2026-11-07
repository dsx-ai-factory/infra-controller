// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"regexp"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

const (
	SSHKeyValidatorRegex = `^ssh-(rsa|ecdsa|ed25519) AAAA[0-9A-Za-z+/]+[=]{0,3}(\s+.+)?$`
	SSHKeyInvalid        = "SSH key is invalid, must be an SSH key of type: RSA, ECDSA or ED25519"
)

// APISSHKeyCreateRequest is the data structure to capture instance request to create a new SSHKey
type APISSHKeyCreateRequest struct {
	// Name is the name of the SSHKey
	Name string `json:"name"`
	// PublicKey is the public key
	PublicKey string `json:"publicKey"`
	// SSHKeyGroupID is the ID of the SSHKey Group
	SSHKeyGroupID *string `json:"sshKeyGroupId"`
}

// Validate ensures that the values passed in request are acceptable
func (skcr APISSHKeyCreateRequest) Validate() error {
	err := validation.ValidateStruct(&skcr,
		validation.Field(&skcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&skcr.PublicKey,
			validation.Required.Error(validationErrorStringLength),
			validation.Match(regexp.MustCompile(SSHKeyValidatorRegex)).Error(SSHKeyInvalid)),
		validation.Field(&skcr.SSHKeyGroupID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}
	return nil
}

// APISSHKeyUpdateRequest is the data structure to capture user request to update a SSHKey
type APISSHKeyUpdateRequest struct {
	// Name is the name of the SSHKey
	Name *string `json:"name"`
}

// Validate ensure the values passed in request are acceptable
func (sgur APISSHKeyUpdateRequest) Validate() error {
	return validation.ValidateStruct(&sgur,
		validation.Field(&sgur.Name,
			validation.When(sgur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(sgur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(sgur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)
}

// APISSHKey is the data structure to capture API representation of a SSHKey
type APISSHKey struct {
	// ID is the unique UUID v4 identifier for the SSHKey
	ID string `json:"id"`
	// Name is the name of the SSHKey
	Name string `json:"name"`
	// Org is the organization the SSHKey belongs to
	Org string `json:"org"`
	// TenantID is the ID of the Tenant
	TenantID string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// Fingerprint is the fingerprint of the public key
	Fingerprint string `json:"fingerprint"`
	// Created indicates the ISO datetime string for when the SSHKey was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the SSHKey was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISSHKey accepts a DB layer SSHKey object and returns an API object
func NewAPISSHKey(sk *cdbm.SSHKey, skas []cdbm.SSHKeyAssociation) *APISSHKey {
	apisk := &APISSHKey{
		ID:       sk.ID.String(),
		Name:     sk.Name,
		Org:      sk.Org,
		TenantID: sk.TenantID.String(),
		Created:  sk.Created,
		Updated:  sk.Updated,
	}

	if sk.Fingerprint != nil {
		apisk.Fingerprint = *sk.Fingerprint
	}

	if sk.Tenant != nil {
		apisk.Tenant = NewAPITenantSummary(sk.Tenant)
	}

	return apisk
}
