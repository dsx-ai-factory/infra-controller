// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/vpcprefix"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	ipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	validationErrorIPBlockIDRequired = "IPBlockID is required in request"
)

// APIVpcPrefixCreateRequest is the data structure to capture user request to create a new VpcPrefix
type APIVpcPrefixCreateRequest struct {
	// Name is the name of the VpcPrefix
	Name string `json:"name"`
	// VpcID is the ID of the vpc containing the VpcPrefix
	VpcID string `json:"vpcId"`
	// IPBlockID is the derived ipBlockId for the tenant from an allocation
	IPBlockID *string `json:"ipBlockId"`
	// PrefixLength is the length of the prefix
	PrefixLength int `json:"prefixLength"`
}

// ValidatePrefixLength checks PrefixLength against the maximum resolved from
// the IP Block family and VPC address mode. Callers run Validate first because
// those values are not part of the request body.
func (vpcr *APIVpcPrefixCreateRequest) ValidatePrefixLength(maxPrefixLength int) error {
	if vpcr.PrefixLength > maxPrefixLength {
		return validation.Errors{
			"prefixLength": fmt.Errorf("prefixLength must be at most %d for this IP Block and VPC", maxPrefixLength),
		}
	}

	return nil
}

// Validate ensure the values passed in request are acceptable
func (vpcr *APIVpcPrefixCreateRequest) Validate() error {
	err := validation.ValidateStruct(vpcr,
		validation.Field(&vpcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&vpcr.VpcID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&vpcr.IPBlockID,
			validation.Required.Error(validationErrorIPBlockIDRequired),
			validation.When(vpcr.IPBlockID != nil, validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&vpcr.PrefixLength,
			validation.Required.Error(validationErrorValueRequired),
			validation.Min(vpcprefix.PrefixLengthMinimum).Error(fmt.Sprintf("prefixLength must be at least %d", vpcprefix.PrefixLengthMinimum)),
			validation.Max(vpcprefix.PrefixLengthMaximum).Error(fmt.Sprintf("prefixLength must be at most %d", vpcprefix.PrefixLengthMaximum))),
	)

	if err != nil {
		return err
	}

	return nil
}

// ToProto builds the workflow request that asks a Site to create a new
// VpcPrefix for this API request. `vp` is the just-persisted DB record;
// its `ToProto(vpc)` is the source of the canonical wire fields
// (Id, VpcId, Config.Prefix, Metadata.Name). The parent `vpc` is needed
// to translate to the Site-facing VPC ID (`Vpc.GetSiteID`).
//
// The method trusts that the request has already passed `Validate` and
// `ValidatePrefixLength`; the handler supplies the resolved IP Block family
// and VPC SLAAC setting before reaching this conversion.
//
// Precondition: `vpc` must be non-nil; a nil `vpc` produces a
// `VpcPrefixCreationRequest` with an unset `VpcId`, which the Site
// agent will reject. The current handler always provides a hydrated
// parent VPC.
func (vpcr *APIVpcPrefixCreateRequest) ToProto(vp *cdbm.VpcPrefix, vpc *cdbm.Vpc) *corev1.VpcPrefixCreationRequest {
	vpProto := vp.ToProto(vpc)
	return &corev1.VpcPrefixCreationRequest{
		Id:       vpProto.Id,
		VpcId:    vpProto.VpcId,
		Config:   vpProto.Config,
		Metadata: vpProto.Metadata,
	}
}

// APIVpcPrefixUpdateRequest is the data structure to capture user request to update a VpcPrefix
type APIVpcPrefixUpdateRequest struct {
	// Name is the name of the VpcPrefix
	Name *string `json:"name"`
	// IPBlockID is the derived ipBlockId for the tenant from an allocation
	IPBlockID *string `json:"ipBlockId"`
	// PrefixLength is the length of the prefix
	PrefixLength *int `json:"prefixLength"`
}

// Validate ensure the values passed in request are acceptable
func (vpur *APIVpcPrefixUpdateRequest) Validate() error {
	err := validation.ValidateStruct(vpur,
		validation.Field(&vpur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(vpur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(vpur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)

	if vpur.IPBlockID != nil || vpur.PrefixLength != nil {
		return validation.Errors{
			"prefixLength": errors.New("prefix length modification is not supported at this time"),
		}
	}

	if err != nil {
		return err
	}

	return nil
}

// ToProto builds the workflow request that pushes this Update's
// merged-into-DB state to a Site. The persisted `vp` is the source of
// the wire fields because the handler has already merged the request's
// (sparse) update fields into the entity by the time this is called;
// sending the post-merge state matches the pre-existing handler
// behaviour. Currently only `Metadata.Name` flows over (`Prefix` is
// immutable for VpcPrefix and rejected by Validate).
func (vpur *APIVpcPrefixUpdateRequest) ToProto(vp *cdbm.VpcPrefix) *corev1.VpcPrefixUpdateRequest {
	vpProto := vp.ToProto(nil)
	return &corev1.VpcPrefixUpdateRequest{
		Id:       vpProto.Id,
		Metadata: vpProto.Metadata,
	}
}

// APIVpcPrefix is the data structure to capture API representation of a VpcPrefix
type APIVpcPrefix struct {
	// ID is the unique UUID v4 identifier for the VpcPrefix
	ID string `json:"id"`
	// Name is the name of the VpcPrefix
	Name string `json:"name"`
	// SiteID is the ID of the Site containing the VpcPrefix
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// VpcID is the ID of the Vpc containing the VpcPrefix
	VpcID string `json:"vpcId"`
	// Vpc is the summary of the VPC
	Vpc *APIVpcSummary `json:"vpc,omitempty"`
	// IPBlockID is the derived IPBlockId for the tenant from an allocation
	IPBlockID *string `json:"ipBlockId"`
	// IPBlock is the summary of the IPBlock
	IPBlock *APIIPBlockSummary `json:"ipBlock,omitempty"`
	// Prefix includes both IP address and the length of the network, for example: 192.168.1.0/24
	Prefix *string `json:"prefix"`
	// PrefixLength is the length of the network prefix
	PrefixLength int `json:"prefixLength"`
	// Status is the status of the VpcPrefix
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the VpcPrefix
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// UsageStats reports IPv4 usage from VpcPrefixDAO.GetPrefixUsage (in-memory IPAM over /31s from interface IPs) when requested via includeUsageStats
	UsageStats *APIIPBlockUsageStats `json:"usageStats,omitempty"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIVpcPrefix accepts a DB layer objects and returns an API layer object
func NewAPIVpcPrefix(dbvp *cdbm.VpcPrefix, dbsds []cdbm.StatusDetail, dbpu *ipam.Usage) *APIVpcPrefix {
	apiVpcPrefix := APIVpcPrefix{
		ID:           dbvp.ID.String(),
		Name:         dbvp.Name,
		SiteID:       dbvp.SiteID.String(),
		VpcID:        dbvp.VpcID.String(),
		IPBlockID:    util.GetUUIDPtrToStrPtr(dbvp.IPBlockID),
		Prefix:       &dbvp.Prefix,
		PrefixLength: dbvp.PrefixLength,
		Status:       dbvp.Status,
		Created:      dbvp.Created,
		Updated:      dbvp.Updated,
	}

	if dbpu != nil {
		apiVpcPrefix.UsageStats = &APIIPBlockUsageStats{
			AvailableIPs:              dbpu.AvailableIPs,
			AcquiredIPs:               dbpu.AcquiredIPs,
			AvailablePrefixes:         dbpu.AvailablePrefixes,
			AcquiredPrefixes:          dbpu.AcquiredPrefixes,
			AvailableSmallestPrefixes: dbpu.AvailableSmallestPrefixes,
		}
	}

	apiVpcPrefix.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiVpcPrefix.StatusHistory = append(apiVpcPrefix.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	if dbvp.Site != nil {
		apiVpcPrefix.Site = NewAPISiteSummary(dbvp.Site)
	}

	if dbvp.Vpc != nil {
		apiVpcPrefix.Vpc = NewAPIVpcSummary(dbvp.Vpc)
	}

	if dbvp.IPBlock != nil {
		apiVpcPrefix.IPBlock = NewAPIIPBlockSummary(dbvp.IPBlock)
	}

	return &apiVpcPrefix
}

// APIVpcPrefixSummary is the data structure to capture API summary of a VpcPrefix
type APIVpcPrefixSummary struct {
	// ID is the unique UUID v4 identifier for the VpcPrefix
	ID string `json:"id"`
	// Name of the VpcPrefix, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// Prefix is the prefix of the network in CIDR notation
	Prefix *string `json:"prefix"`
	// PrefixLength is the length of the network prefix of this VpcPrefix
	PrefixLength int `json:"prefixLength"`
}

// NewAPIVpcPrefixSummary accepts a DB layer VpcPrefix object returns an API layer object
func NewAPIVpcPrefixSummary(dbvp *cdbm.VpcPrefix) *APIVpcPrefixSummary {
	apiVpcPrefixSummary := APIVpcPrefixSummary{
		ID:           dbvp.ID.String(),
		Name:         dbvp.Name,
		Prefix:       &dbvp.Prefix,
		PrefixLength: dbvp.PrefixLength,
	}
	return &apiVpcPrefixSummary
}
