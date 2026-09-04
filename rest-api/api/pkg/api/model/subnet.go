// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// SubnetBlockSizeMin is the minimum IPv4 prefix length accepted by REST Subnet creation.
	SubnetBlockSizeMin = 8
	// SubnetBlockSizeMax is the maximum IPv4 prefix length accepted by REST Subnet creation.
	SubnetBlockSizeMax = 30

	validationErrorIPv4BlockIDRequired    = "REST Subnet creation requires ipv4BlockId"
	validationErrorIPv6SubnetNotSupported = "ipv6BlockId is not supported for Subnets in Ethernet virtualizer VPCs; FNN VPCs use VPC Prefixes"
	validationErrorSubnetBlockSizeMin     = "prefixLength must be at least 8"
	validationErrorSubnetBlockSizeMax     = "prefixLength must be at most 30"
)

// APISubnetCreateRequest captures a request to create an IPv4 Subnet in an Ethernet virtualizer VPC.
// FNN VPCs use the separate VPC Prefix resource.
type APISubnetCreateRequest struct {
	// Name is the name of the Subnet
	Name string `json:"name"`
	// Description is the description of the Subnet
	Description *string `json:"description"`
	// VpcID is the ID of the vpc containing the Subnet
	VpcID string `json:"vpcId"`
	// IPv4BlockID identifies a Ready, derived IPv4 block for the tenant from an allocation.
	IPv4BlockID *string `json:"ipv4BlockId"`
	// IPv6BlockID is retained so legacy ipv6BlockId input can be rejected explicitly.
	IPv6BlockID *string `json:"ipv6BlockId"`
	// SubdomainID identifies the REST Domain to use for DNS records in this Subnet.
	SubdomainID *string `json:"subdomainId"`
	// PrefixLength is the length of the IPv4 prefix.
	PrefixLength int `json:"prefixLength"`
	// ControllerDomainID is the Site-facing ID resolved from SubdomainID by the handler.
	ControllerDomainID *uuid.UUID `json:"-"`
}

// Validate ensure the values passed in request are acceptable
func (scr *APISubnetCreateRequest) Validate() error {
	err := validation.ValidateStruct(scr,
		validation.Field(&scr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&scr.Description,
			validation.When(scr.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
		validation.Field(&scr.VpcID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&scr.IPv4BlockID,
			validation.Required.Error(validationErrorIPv4BlockIDRequired),
			validation.When(scr.IPv4BlockID != nil, validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&scr.IPv6BlockID,
			// Subnets in Ethernet virtualizer VPCs support IPv4 only.
			validation.Nil.Error(validationErrorIPv6SubnetNotSupported)),
		validation.Field(&scr.SubdomainID,
			validation.When(scr.SubdomainID != nil, validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&scr.PrefixLength,
			validation.Required.Error(validationErrorValueRequired),
			validation.Min(SubnetBlockSizeMin).Error(validationErrorSubnetBlockSizeMin),
			validation.Max(SubnetBlockSizeMax).Error(validationErrorSubnetBlockSizeMax)),
	)

	if err != nil {
		return err
	}

	return nil
}

// ToProto builds the workflow request that asks a Site to create a new
// Subnet under the given parent VPC. `subnet` is the just-persisted DB
// record; its `ToProto()` is the source of the canonical wire fields
// (Id/Name/Mtu/SubdomainId/Prefixes). `vpc` provides the Site-facing
// parent VPC ID. `reservedIPCount` is a deployment-policy value applied
// to each prefix that does not live on the entity, so it is threaded
// through here.
//
// The method trusts that the request has already been Validated and
// that the handler has performed any cross-context checks Validate
// cannot see.
func (scr *APISubnetCreateRequest) ToProto(subnet *cdbm.Subnet, vpc *cdbm.Vpc, reservedIPCount int32) *corev1.NetworkSegmentCreationRequest {
	subnetProto := subnet.ToProto()
	cfg := subnetProto.GetConfig()
	// `prefixes` aliases `cfg.Prefixes`; the slice + its `NetworkPrefix`
	// elements are freshly allocated by `subnet.ToProto()` on every call, so
	// mutating `ReserveFirst` here is safe -- nothing else holds a reference
	// to those values.
	prefixes := cfg.GetPrefixes()
	for _, p := range prefixes {
		p.ReserveFirst = reservedIPCount
	}
	var subdomainID *corev1.DomainId
	var mtu *int32
	if cfg != nil {
		mtu = cfg.Mtu
	}
	if scr.ControllerDomainID != nil {
		subdomainID = &corev1.DomainId{Value: scr.ControllerDomainID.String()}
	}
	return &corev1.NetworkSegmentCreationRequest{
		Id:          subnetProto.Id,
		Name:        subnetProto.GetMetadata().GetName(),
		SubdomainId: subdomainID,
		VpcId:       &corev1.VpcId{Value: vpc.GetSiteID().String()},
		Mtu:         mtu,
		Prefixes:    prefixes,
	}
}

// APISubnetAttachVpcRequest captures a request to reassign a Subnet to another VPC.
type APISubnetAttachVpcRequest struct {
	// VpcID identifies the target REST VPC.
	VpcID string `json:"vpcId"`
	// AllowReplace permits replacing an attachment to a different VPC.
	AllowReplace bool `json:"allowReplace,omitempty"`
	// ControllerNetworkSegmentID is the Site-facing Subnet ID resolved by the handler.
	ControllerNetworkSegmentID uuid.UUID `json:"-"`
	// ControllerVpcID is the Site-facing target VPC ID resolved by the handler.
	ControllerVpcID uuid.UUID `json:"-"`
}

// Validate ensures the target REST VPC ID is present and valid.
func (savr *APISubnetAttachVpcRequest) Validate() error {
	return validation.ValidateStruct(savr,
		validation.Field(&savr.VpcID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
}

// ToProto builds the Site-facing request after the handler has resolved both controller IDs.
func (savr *APISubnetAttachVpcRequest) ToProto() *corev1.AttachNetworkSegmentToVpcRequest {
	return &corev1.AttachNetworkSegmentToVpcRequest{
		NetworkSegmentId: &corev1.NetworkSegmentId{Value: savr.ControllerNetworkSegmentID.String()},
		VpcId:            &corev1.VpcId{Value: savr.ControllerVpcID.String()},
		AllowReplace:     savr.AllowReplace,
	}
}

// APISubnetUpdateRequest is the data structure to capture user request to update a Subnet
type APISubnetUpdateRequest struct {
	// Name is the name of the Subnet
	Name *string `json:"name"`
	// Description is the description of the Subnet
	Description *string `json:"description"`
}

// Validate ensure the values passed in request are acceptable
func (sur *APISubnetUpdateRequest) Validate() error {
	return validation.ValidateStruct(sur,
		validation.Field(&sur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(sur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(sur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
		validation.Field(&sur.Description,
			validation.When(sur.Description != nil,
				validation.Length(0, 1024).Error(validationErrorDescriptionStringLength)),
		),
	)
}

// APISubnet is the data structure to capture API representation of a Subnet
type APISubnet struct {
	// ID is the unique UUID v4 identifier for the Subnet
	ID string `json:"id"`
	// Name is the name of the Subnet
	Name string `json:"name"`
	// Description is the description of the Subnet
	Description *string `json:"description"`
	// SiteID is the ID of the Site containing the Subnet
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// VpcID is the ID of the Vpc containing the Subnet
	VpcID string `json:"vpcId"`
	// Vpc is the summary of the VPC
	Vpc *APIVpcSummary `json:"vpc,omitempty"`
	// SubdomainID is the local REST Domain ID used by this Subnet.
	SubdomainID *string `json:"subdomainId"`
	// Controller network Segment ID is the ID of the Site Controller Network Segment corresponding to the Subnet
	ControllerNetworkSegmentID *string `json:"controllerNetworkSegmentId"`
	// IPv4Prefix is the prefix of the network in CIDR notation
	IPv4Prefix *string `json:"ipv4Prefix"`
	// IPv4BlockID is the derived IPv4BlockId for the tenant from an allocation
	IPv4BlockID *string `json:"ipv4BlockId"`
	// IPv4Block is the summary of the IPv4Block
	IPv4Block *APIIPBlockSummary `json:"ipv4Block,omitempty"`
	// IPv4Gateway is the address of the IPv4 gateway in the Subnet
	IPv4Gateway *string `json:"ipv4Gateway"`
	// IPv6Prefix is the prefix of the network in CIDR notation
	IPv6Prefix *string `json:"ipv6Prefix"`
	// IPv6BlockID is the derived IPv6BlockId for the tenant from an allocation
	IPv6BlockID *string `json:"ipv6BlockId"`
	// IPv4Block is the summary of the IPv4Block
	IPv6Block *APIIPBlockSummary `json:"ipv6Block,omitempty"`
	// IPv6Gateway is the address of the IPv6 gateway in the Subnet
	IPv6Gateway *string `json:"ipv6Gateway"`
	// PrefixLength is the length of the network prefix
	PrefixLength int `json:"prefixLength"`
	// RoutingType is the routing type of the Subnet
	RoutingType *string `json:"routingType"`
	// Status is the status of the Subnet
	Status string `json:"status"`
	// MTU is the maximum transmission unit of the Subnet
	MTU *int `json:"mtu"`
	// StatusHistory is the history of statuses for the Subnet
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// UsageStats reports IPv4 usage from SubnetDAO.GetPrefixUsage (in-memory IPAM over interface IPs) when requested via includeUsageStats
	UsageStats *APIIPBlockUsageStats `json:"usageStats,omitempty"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISubnet accepts a DB layer objects and returns an API layer object
func NewAPISubnet(dbs *cdbm.Subnet, dbsds []cdbm.StatusDetail, dbpu *cipam.Usage) *APISubnet {
	apiSubnet := APISubnet{
		ID:           dbs.ID.String(),
		Name:         dbs.Name,
		Description:  dbs.Description,
		SiteID:       dbs.SiteID.String(),
		VpcID:        dbs.VpcID.String(),
		IPv4Prefix:   dbs.IPv4Prefix,
		IPv4Gateway:  dbs.IPv4Gateway,
		IPv4BlockID:  util.GetUUIDPtrToStrPtr(dbs.IPv4BlockID),
		IPv6Prefix:   dbs.IPv6Prefix,
		IPv6Gateway:  dbs.IPv6Gateway,
		IPv6BlockID:  util.GetUUIDPtrToStrPtr(dbs.IPv6BlockID),
		PrefixLength: dbs.PrefixLength,
		RoutingType:  dbs.RoutingType,
		Status:       dbs.Status,
		Created:      dbs.Created,
		Updated:      dbs.Updated,
		MTU:          dbs.MTU,
	}

	if dbpu != nil {
		apiSubnet.UsageStats = &APIIPBlockUsageStats{
			AvailableIPs:              dbpu.AvailableIPs,
			AcquiredIPs:               dbpu.AcquiredIPs,
			AvailablePrefixes:         dbpu.AvailablePrefixes,
			AcquiredPrefixes:          dbpu.AcquiredPrefixes,
			AvailableSmallestPrefixes: dbpu.AvailableSmallestPrefixes,
		}
	}

	if dbs.ControllerNetworkSegmentID != nil {
		apiSubnet.ControllerNetworkSegmentID = util.GetUUIDPtrToStrPtr(dbs.ControllerNetworkSegmentID)
	}
	apiSubnet.SubdomainID = util.GetUUIDPtrToStrPtr(dbs.DomainID)

	apiSubnet.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiSubnet.StatusHistory = append(apiSubnet.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	if dbs.Site != nil {
		apiSubnet.Site = NewAPISiteSummary(dbs.Site)
	}

	if dbs.Vpc != nil {
		apiSubnet.Vpc = NewAPIVpcSummary(dbs.Vpc)
	}

	if dbs.IPv4Block != nil {
		apiSubnet.IPv4Block = NewAPIIPBlockSummary(dbs.IPv4Block)
	}

	if dbs.IPv6Block != nil {
		apiSubnet.IPv6Block = NewAPIIPBlockSummary(dbs.IPv6Block)
	}

	return &apiSubnet
}

// APISubnetStats is a data structure to capture information about Subnet stats at the API layer
type APISubnetStats struct {
	// Total is the total number of the Subnet object in NICo Cloud
	Total int `json:"total"`
	// Pending is the total number of pending Subnet object in NICo Cloud
	Pending int `json:"pending"`
	// Provisioning is the total number of provisioning Subnet object in NICo Cloud
	Provisioning int `json:"provisioning"`
	// Ready is the total number of ready Subnet object in NICo Cloud
	Ready int `json:"ready"`
	// Deleting is the total number of deleting Subnet object in NICo Cloud
	Deleting int `json:"deleting"`
	// Error is the total number of error Subnet object in NICo Cloud
	Error int `json:"error"`
}

// APISubnetSummary is the data structure to capture API summary of a Subnet
type APISubnetSummary struct {
	// ID is the unique UUID v4 identifier for the Subnet
	ID string `json:"id"`
	// Name of the Subnet, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// Controller network Segment ID is the ID of the Site Controller Network Segment corresponding to the Subnet
	ControllerNetworkSegmentID *string `json:"controllerNetworkSegmentId"`
	// IPv4Prefix is the prefix of the network in CIDR notation
	IPv4Prefix *string `json:"ipv4Prefix"`
	// IPv6Prefix is the prefix of the network in CIDR notation
	IPv6Prefix *string `json:"ipv6Prefix"`
	// PrefixLength is the length of the network prefix of this Subnet
	PrefixLength int `json:"prefixLength"`
	// RoutingType is the routing type of the Subnet
	RoutingType *string `json:"routingType"`
	// Status is the status of the Subnet
	Status string `json:"status"`
}

// NewAPISubnetSummary accepts a DB layer Subnet object returns an API layer object
func NewAPISubnetSummary(dbs *cdbm.Subnet) *APISubnetSummary {
	apiSubnetSummary := APISubnetSummary{
		ID:           dbs.ID.String(),
		Name:         dbs.Name,
		IPv4Prefix:   dbs.IPv4Prefix,
		IPv6Prefix:   dbs.IPv6Prefix,
		PrefixLength: dbs.PrefixLength,
		RoutingType:  dbs.RoutingType,
		Status:       dbs.Status,
	}

	if dbs.ControllerNetworkSegmentID != nil {
		apiSubnetSummary.ControllerNetworkSegmentID = util.GetUUIDPtrToStrPtr(dbs.ControllerNetworkSegmentID)
	}

	return &apiSubnetSummary
}
