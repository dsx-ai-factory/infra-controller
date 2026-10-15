// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"net/netip"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	ipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
)

const (
	// IPv4BlockSizeMin is the minimum value of the IPv4 BlockSize field
	IPv4BlockSizeMin = 1
	// IPv4BlockSizeMax is the maximum value of the IPv4 BlockSize field
	IPv4BlockSizeMax = 32
	// IPv6BlockSizeMin is the minimum value of the IPv6 BlockSize field
	IPv6BlockSizeMin = 1
	// IPv6BlockSizeMax is the maximum value of the IPv6 BlockSize field
	IPv6BlockSizeMax = 128

	validationErrorIPBlockRoutingType     = `IP Block routing type must be "Public" or "DatacenterOnly"`
	validationErrorIPBlockProtocolVersion = `IP Block Protocol Version must be "IPv4" or "IPv6"`
	validationErrorIPv4BlockSizeMin       = "prefixLength must be at least 1"
	validationErrorIPv4BlockSizeMax       = "prefixLength must be at most 32"
	validationErrorIPv6BlockSizeMin       = "prefixLength must be at least 1"
	validationErrorIPv6BlockSizeMax       = "prefixLength must be at most 128"
)

// APIIPBlockCreateRequest is the data structure to capture user request to create a new IPBlock
type APIIPBlockCreateRequest struct {
	// Name is the name of the IPBlock
	Name string `json:"name"`
	// Description is the description of the IPBlock
	Description *string `json:"description"`
	// SiteID is the ID of the site
	SiteID string `json:"siteId"`
	// RoutingType is the routing type of the IPBlock
	RoutingType string `json:"routingType"`
	// Prefix is the prefix of the network in CIDR notation
	Prefix string `json:"prefix"`
	// PrefixLength is the length of the prefix
	PrefixLength int `json:"prefixLength"`
	// ProtocolVersion is the version of the ip network ipv4 or ipv6
	ProtocolVersion string `json:"protocolVersion"`
}

// Validate checks the request and normalizes `Prefix` on success. The database
// compares prefixes as text, so equivalent IPv6 spellings must use the same
// compressed, lowercase form before duplicate checks and persistence.
func (ipbcr *APIIPBlockCreateRequest) Validate() error {
	err := validation.ValidateStruct(ipbcr,
		validation.Field(&ipbcr.Name,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&ipbcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ipbcr.RoutingType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(cdbm.IPBlockRoutingTypePublic, cdbm.IPBlockRoutingTypeDatacenterOnly).Error(validationErrorIPBlockRoutingType)),
		validation.Field(&ipbcr.Prefix,
			validation.Required.Error(validationErrorValueRequired),
			validationis.IP.Error(validationErrorInvalidIPAddress)),
		validation.Field(&ipbcr.PrefixLength,
			validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&ipbcr.ProtocolVersion,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(cdbm.IPBlockProtocolVersionV4, cdbm.IPBlockProtocolVersionV6).Error(validationErrorIPBlockProtocolVersion)),
	)
	if err != nil {
		return err
	}

	// Validate ipv4
	if ipbcr.ProtocolVersion == cdbm.IPBlockProtocolVersionV4 {
		// Validate PrefixLength
		err := validation.ValidateStruct(ipbcr,
			validation.Field(&ipbcr.PrefixLength,
				validation.Min(IPv4BlockSizeMin).Error(validationErrorIPv4BlockSizeMin),
				validation.Max(IPv4BlockSizeMax).Error(validationErrorIPv4BlockSizeMax)),
		)
		if err != nil {
			return err
		}

		err = validation.ValidateStruct(ipbcr,
			validation.Field(&ipbcr.Prefix,
				validationis.IPv4.Error(validationErrorInvalidIPv4Address)),
		)
		if err != nil {
			return err
		}
	}
	// validate ipv6
	if ipbcr.ProtocolVersion == cdbm.IPBlockProtocolVersionV6 {
		// Validate PrefixLength
		err := validation.ValidateStruct(ipbcr,
			validation.Field(&ipbcr.PrefixLength,
				validation.Min(IPv6BlockSizeMin).Error(validationErrorIPv6BlockSizeMin),
				validation.Max(IPv6BlockSizeMax).Error(validationErrorIPv6BlockSizeMax)),
		)
		if err != nil {
			return err
		}

		// Validate ipv6 prefix
		err = validation.ValidateStruct(ipbcr,
			validation.Field(&ipbcr.Prefix,
				validationis.IPv6.Error(validationErrorInvalidIPv6Address)),
		)
		if err != nil {
			return err
		}
	}

	prefixWithLength := netip.MustParsePrefix(fmt.Sprintf("%s/%d", ipbcr.Prefix, ipbcr.PrefixLength))
	if ipbcr.ProtocolVersion == cdbm.IPBlockProtocolVersionV4 && !prefixWithLength.Addr().Is4() {
		return validation.Errors{"prefix": errors.New(validationErrorInvalidIPv4Address)}
	}
	networkAddr := prefixWithLength.Masked().Addr()
	if prefixWithLength.Addr() != networkAddr {
		return validation.Errors{
			"prefix": fmt.Errorf("prefix should have %d right most bits zeroed to match block size, e.g. %s", ipbcr.PrefixLength, networkAddr.String()),
		}
	}

	ipbcr.Prefix = networkAddr.String()
	return nil
}

// APIIPBlockUpdateRequest is the data structure to capture user request to update an IPBlock
type APIIPBlockUpdateRequest struct {
	// Name is the name of the IPBlock
	Name *string `json:"name"`
	// Description is the description of the IPBlock
	Description *string `json:"description"`
}

// Validate ensure the values passed in request are acceptable
func (ipbur APIIPBlockUpdateRequest) Validate() error {
	fmt.Printf("%v", ipbur)
	return validation.ValidateStruct(&ipbur,
		validation.Field(&ipbur.Name,
			// length validation rule accepts empty string as valid, hence, required is needed
			validation.When(ipbur.Name != nil, validation.Required.Error(validationErrorStringLength)),
			validation.When(ipbur.Name != nil, validation.By(util.ValidateNameCharacters)),
			validation.When(ipbur.Name != nil, validation.Length(2, 256).Error(validationErrorStringLength))),
	)
}

// APIIPBlock is the data structure to capture API representation of an IPBlock
type APIIPBlock struct {
	// ID is the unique UUID v4 identifier for the IPBlock
	ID string `json:"id"`
	// Name is the name of the IPBlock
	Name string `json:"name"`
	// Description is the description of the IPBlock
	Description *string `json:"description"`
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// Site is the summary of the Site
	Site *APISiteSummary `json:"site,omitempty"`
	// InfrastructureProviderID is the ID of the InfrastructureProvider
	InfrastructureProviderID string `json:"infrastructureProviderId"`
	// InfrastructureProvider is the summary of the InfrastructureProvider
	InfrastructureProvider *APIInfrastructureProviderSummary `json:"infrastructureProvider,omitempty"`
	// TenantID is the ID of the Tenant
	TenantID *string `json:"tenantId"`
	// Tenant is the summary of the tenant
	Tenant *APITenantSummary `json:"tenant,omitempty"`
	// RoutingType is the routingType of the IPBlock
	RoutingType string `json:"routingType"`
	// Prefix is the prefix of the network in CIDR notation
	Prefix string `json:"prefix"`
	// PrefixLength is the length of the network prefix
	PrefixLength int `json:"prefixLength"`
	// ProtocolVersion is the version of the ip network IPv4 or IPv6
	ProtocolVersion string `json:"protocolVersion"`
	// Status is the status of the IPBlock
	Status string `json:"status"`
	// StatusHistory is the history of statuses for the IPBlock
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// UsageStats is the usage summary from IPAM for the IPBlock
	UsageStats *APIIPBlockUsageStats `json:"usageStats,omitempty"`
	// CreatedAt indicates the ISO datetime string for when the entity was created
	Created time.Time `json:"created"`
	// UpdatedAt indicates the ISO datetime string for when the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIIPBlock accepts a DB layer IPBlock object returns an API layer object
func NewAPIIPBlock(dbipb *cdbm.IPBlock, dbsds []cdbm.StatusDetail, dbpu *ipam.Usage) *APIIPBlock {
	apiIPBlock := APIIPBlock{
		ID:                       dbipb.ID.String(),
		Name:                     dbipb.Name,
		Description:              dbipb.Description,
		SiteID:                   dbipb.SiteID.String(),
		InfrastructureProviderID: dbipb.InfrastructureProviderID.String(),
		TenantID:                 util.GetUUIDPtrToStrPtr(dbipb.TenantID),
		RoutingType:              dbipb.RoutingType,
		Prefix:                   dbipb.Prefix,
		PrefixLength:             dbipb.PrefixLength,
		ProtocolVersion:          dbipb.ProtocolVersion,
		Status:                   dbipb.Status,
		Created:                  dbipb.Created,
		Updated:                  dbipb.Updated,
	}

	apiIPBlock.PrefixLength = dbipb.PrefixLength

	if dbipb.InfrastructureProvider != nil {
		apiIPBlock.InfrastructureProvider = NewAPIInfrastructureProviderSummary(dbipb.InfrastructureProvider)
	}
	if dbipb.Site != nil {
		apiIPBlock.Site = NewAPISiteSummary(dbipb.Site)
	}
	if dbipb.Tenant != nil {
		apiIPBlock.Tenant = NewAPITenantSummary(dbipb.Tenant)
	}
	// IPBlock usage stats
	if dbpu != nil {
		apiIPBlock.UsageStats = &APIIPBlockUsageStats{
			AvailableIPs:              dbpu.AvailableIPs,
			AcquiredIPs:               dbpu.AcquiredIPs,
			AvailablePrefixes:         dbpu.AvailablePrefixes,
			AcquiredPrefixes:          dbpu.AcquiredPrefixes,
			AvailableSmallestPrefixes: dbpu.AvailableSmallestPrefixes,
		}
	}

	apiIPBlock.StatusHistory = []APIStatusDetail{}
	for _, dbsd := range dbsds {
		apiIPBlock.StatusHistory = append(apiIPBlock.StatusHistory, NewAPIStatusDetail(dbsd))
	}

	return &apiIPBlock
}

// APIIPBlockSummary is the data structure to capture API summary of an IPBlock
type APIIPBlockSummary struct {
	// ID of the IP Block
	ID string `json:"id"`
	// Name of the IPBlock, only lowercase characters, digits, hyphens and cannot begin/end with hyphen
	Name string `json:"name"`
	// RoutingType is the routingType of the IPBlock
	RoutingType string `json:"routingType"`
	// Prefix is the prefix of the network in CIDR notation
	Prefix string `json:"prefix"`
	// PrefixLength is the length of the network prefix
	PrefixLength int `json:"prefixLength"`
	// Status is the status of the IPBlock
	Status string `json:"status"`
}

// NewAPIIPBlockSummary accepts a DB layer IPBlock object returns an API layer object
func NewAPIIPBlockSummary(dbipb *cdbm.IPBlock) *APIIPBlockSummary {
	ipb := APIIPBlockSummary{
		ID:           dbipb.ID.String(),
		Name:         dbipb.Name,
		RoutingType:  dbipb.RoutingType,
		Prefix:       dbipb.Prefix,
		PrefixLength: dbipb.PrefixLength,
		Status:       dbipb.Status,
	}

	return &ipb
}

// APIIPBlockStats is a data structure to capture information about IPBlock stats at the API layer
type APIIPBlockStats struct {
	// Total is the total number of the IPBlock object in NICo Cloud
	Total int `json:"total"`
	// Pending is the total number of pending IPBlock object in NICo Cloud
	Pending int `json:"pending"`
	// Provisioning is the total number of provisioning IPBlock object in NICo Cloud
	Provisioning int `json:"provisioning"`
	// Ready is the total number of ready IPBlock object in NICo Cloud
	Ready int `json:"ready"`
	// Deleting is the total number of deleting IPBlock object in NICo Cloud
	Deleting int `json:"deleting"`
	// Error is the total number of error IPBlock object in NICo Cloud
	Error int `json:"error"`
}

// APIIPBlockUsageStats is a data structure to capture information about IPBlock usage statsfrom IPAM at the API layer
type APIIPBlockUsageStats struct {
	// AvailableIPs is the total number of available IPs in the IPBlock
	AvailableIPs uint64 `json:"availableIPs"`
	// AcquiredIPs the number of acquired IPs from the IPBlock
	AcquiredIPs uint64 `json:"acquiredIPs"`
	// AvailablePrefixes is a list of prefixes which are available in the IPBlock
	AvailablePrefixes []string `json:"availablePrefixes"`
	// AvailableSmallestPrefixes is the count of available Prefixes with 2 countable Bits
	AvailableSmallestPrefixes uint64 `json:"availableSmallestPrefixes"`
	// AcquiredPrefixes the number of acquired prefixes from the IPBlock
	AcquiredPrefixes uint64 `json:"acquiredPrefixes"`
}
