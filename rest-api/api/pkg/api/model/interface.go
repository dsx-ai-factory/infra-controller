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

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// IPFamily identifies an address family for a VPC-selected Interface.
type IPFamily string

const (
	// IPFamilyIPv4 identifies IPv4 addressing.
	IPFamilyIPv4 IPFamily = cdbm.IPBlockProtocolVersionV4
	// IPFamilyIPv6 identifies IPv6 addressing.
	IPFamilyIPv6 IPFamily = cdbm.IPBlockProtocolVersionV6
)

// APIInterfaceCreateRequest is the data structure to capture user request to create a new Interface
type APIInterfaceCreateOrUpdateRequest struct {
	// SubnetID is the ID of the Subnet
	SubnetID *string `json:"subnetId"`
	// VpcPrefixID is the ID of the VpcPrefix
	VpcPrefixID *string `json:"vpcPrefixId"`
	// VpcID is the ID of the VPC from which the Controller selects a prefix
	VpcID *string `json:"vpcId"`
	// IPFamilies lists the address families requested for a VPC-selected Interface
	IPFamilies []IPFamily `json:"ipFamilies"`
	// IPAddress is the explicitly requested IP address for the Interface
	IPAddress *string `json:"ipAddress"`
	// InlineRoutingProfile narrows VPC routing-profile options for this Interface.
	InlineRoutingProfile *APIInterfaceInlineRoutingProfile `json:"inlineRoutingProfile"`
	// Device is the device name of the Interface
	Device *string `json:"device"`
	// DeviceInstance is the ID of the DeviceInstance
	DeviceInstance *int `json:"deviceInstance"`
	// VirtualFunctionID is the ID of the Virtual Function
	VirtualFunctionID *int `json:"virtualFunctionId"`
	// IsPhysical indicates whether the network is bound on a physical Interface
	IsPhysical bool `json:"isPhysical"`
}

// APIInterfaceInlineRoutingProfile captures inline interface-local routing profile options.
type APIInterfaceInlineRoutingProfile struct {
	// AllowedAnycastPrefixes is the list of CIDR prefixes this interface may announce as anycast.
	AllowedAnycastPrefixes []string `json:"allowedAnycastPrefixes"`
}

// Validate ensures the routing profile contains valid CIDR prefixes.
func (rp *APIInterfaceInlineRoutingProfile) Validate() error {
	if rp == nil {
		return nil
	}
	if rp.AllowedAnycastPrefixes == nil {
		rp.AllowedAnycastPrefixes = []string{}
	}
	err := validation.Validate(rp.AllowedAnycastPrefixes,
		validation.Each(validation.By(validateInterfaceAnycastPrefix)),
	)
	if err != nil {
		return validation.Errors{
			"allowedAnycastPrefixes": err,
		}
	}
	return nil
}

func validateInterfaceAnycastPrefix(value any) error {
	prefix, ok := value.(string)
	if !ok {
		return nil
	}
	if _, err := netip.ParsePrefix(prefix); err != nil {
		return fmt.Errorf("invalid anycast prefix `%s`", prefix)
	}
	return nil
}

// ToDB converts the API routing profile into the DB model shape.
func (rp *APIInterfaceInlineRoutingProfile) ToDB() *cdbm.InterfaceInlineRoutingProfile {
	if rp == nil {
		return nil
	}
	prefixes := []string{}
	if rp.AllowedAnycastPrefixes != nil {
		prefixes = append(prefixes, rp.AllowedAnycastPrefixes...)
	}
	return &cdbm.InterfaceInlineRoutingProfile{AllowedAnycastPrefixes: prefixes}
}

// FromDB populates the API routing profile from the DB model shape.
func (rp *APIInterfaceInlineRoutingProfile) FromDB(dbProfile *cdbm.InterfaceInlineRoutingProfile) {
	if rp == nil || dbProfile == nil {
		return
	}
	prefixes := []string{}
	if dbProfile.AllowedAnycastPrefixes != nil {
		prefixes = append(prefixes, dbProfile.AllowedAnycastPrefixes...)
	}
	rp.AllowedAnycastPrefixes = prefixes
}

func (ifcr APIInterfaceCreateOrUpdateRequest) IsMultiEthernetInterface() bool {
	return ifcr.Device != nil && ifcr.DeviceInstance != nil
}

func validateInterfaceRequestedIpAddressHostBit(value any) error {
	ipStr, ok := value.(*string)
	if !ok || ipStr == nil {
		return nil
	}

	addr, err := netip.ParseAddr(*ipStr)
	if err != nil {
		return errors.New("ipAddress must be a valid IPv4 or IPv6 address")
	}

	ipBytes := addr.AsSlice()
	if len(ipBytes) == 0 || ipBytes[len(ipBytes)-1]&0x01 == 0 {
		return errors.New("ipAddress must have a final host bit of 1")
	}

	return nil
}

// Validate ensure the values passed in request are acceptable
func (ifcr *APIInterfaceCreateOrUpdateRequest) Validate() error {
	err := validation.ValidateStruct(ifcr,
		validation.Field(&ifcr.SubnetID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ifcr.VpcPrefixID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ifcr.VpcID,
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ifcr.IPAddress,
			validation.When(ifcr.IPAddress != nil, validation.By(validateInterfaceRequestedIpAddressHostBit)),
		),
		validation.Field(&ifcr.InlineRoutingProfile),
		validation.Field(&ifcr.DeviceInstance,
			validation.Min(0).Error("deviceInstance must be equal or greater than 0")),
		validation.Field(&ifcr.VirtualFunctionID,
			validation.Min(0).Error("virtualFunctionId must be between 0 and 15"),
			validation.Max(15).Error("virtualFunctionId must be between 0 and 15")),
	)
	if err != nil {
		return err
	}

	selectorCount := 0
	for _, specified := range []bool{ifcr.SubnetID != nil, ifcr.VpcPrefixID != nil, ifcr.VpcID != nil} {
		if specified {
			selectorCount++
		}
	}
	if selectorCount != 1 {
		return validation.Errors{
			validationCommonErrorField: errors.New("exactly one of `subnetId`, `vpcPrefixId`, or `vpcId` must be specified"),
		}
	}

	// Validate and normalize the requested address families.
	if ifcr.VpcID != nil {
		if ifcr.IPFamilies == nil {
			return validation.Errors{
				"ipFamilies": errors.New("must be specified when `vpcId` is specified"),
			}
		}
		if len(ifcr.IPFamilies) == 0 {
			return validation.Errors{
				"ipFamilies": errors.New("must contain at least one value when `vpcId` is specified"),
			}
		}

		seenIPv4 := false
		seenIPv6 := false
		for _, family := range ifcr.IPFamilies {
			switch family {
			case IPFamilyIPv4:
				seenIPv4 = true
			case IPFamilyIPv6:
				seenIPv6 = true
			default:
				return validation.Errors{
					"ipFamilies": fmt.Errorf("invalid IP family `%s`", family),
				}
			}
		}

		normalizedFamilies := make([]IPFamily, 0, 2)
		if seenIPv4 {
			normalizedFamilies = append(normalizedFamilies, IPFamilyIPv4)
		}
		if seenIPv6 {
			normalizedFamilies = append(normalizedFamilies, IPFamilyIPv6)
		}
		ifcr.IPFamilies = normalizedFamilies
	} else if ifcr.IPFamilies != nil {
		return validation.Errors{
			"ipFamilies": errors.New("cannot be specified without `vpcId`"),
		}
	}

	if ifcr.IPAddress != nil && (ifcr.SubnetID != nil || ifcr.VpcID != nil) {
		if ifcr.VpcID != nil {
			return validation.Errors{
				"ipAddress": errors.New("cannot be specified when `vpcId` is specified"),
			}
		}
		return validation.Errors{
			"ipAddress": errors.New("cannot be specified for Subnet based Interfaces"),
		}
	}

	if ifcr.InlineRoutingProfile != nil && ifcr.SubnetID != nil {
		return validation.Errors{
			"inlineRoutingProfile": errors.New("cannot be specified for Subnet based Interfaces"),
		}
	}

	if ifcr.Device != nil {
		if ifcr.DeviceInstance == nil {
			return validation.Errors{
				"deviceInstance": errors.New("must be specified when `device` is specified"),
			}
		}

		if ifcr.VpcPrefixID == nil && ifcr.VpcID == nil {
			return validation.Errors{
				validationCommonErrorField: errors.New("either `vpcPrefixId` or `vpcId` must be specified when `device` and `deviceInstance` are specified"),
			}
		}

		// virtualFunctionId is required when Device/DeviceInstance is present and isPhysical is false
		if !ifcr.IsPhysical && ifcr.VirtualFunctionID == nil {
			return validation.Errors{
				"virtualFunctionId": errors.New("must be specified when `device` and `deviceInstance` are specified and `isPhysical` is false"),
			}
		}
	} else if ifcr.DeviceInstance != nil {
		return validation.Errors{
			"device": errors.New("must be specified when `deviceInstance` is specified"),
		}
	}

	return nil
}

// VpcIPFamilyMode converts validated VPC-selection families to the DB model.
func (ifcr APIInterfaceCreateOrUpdateRequest) VpcIPFamilyMode() cdbm.InterfaceVpcIPFamilyMode {
	if len(ifcr.IPFamilies) == 2 {
		return cdbm.InterfaceVpcIPFamilyModeDualStack
	}
	if len(ifcr.IPFamilies) == 1 && ifcr.IPFamilies[0] == IPFamilyIPv6 {
		return cdbm.InterfaceVpcIPFamilyModeIPv6Only
	}
	return cdbm.InterfaceVpcIPFamilyModeIPv4Only
}

// APIInterface is the data structure to capture Interface
type APIInterface struct {
	// ID is the unique UUID v4 identifier for the Interface
	ID string `json:"id"`
	// InstanceID is the ID of the associated Instance
	InstanceID string `json:"instanceId"`
	// Instance is the summary of the Instance
	Instance *APIInstanceSummary `json:"instance,omitempty"`
	// SubnetID is the ID of the associated Subnet
	SubnetID *string `json:"subnetId"`
	// Subnet is the summary of the Subnet
	Subnet *APISubnetSummary `json:"subnet,omitempty"`
	// VpcPrefixID is the ID of the explicitly selected VpcPrefix
	VpcPrefixID *string `json:"vpcPrefixId"`
	// VpcPrefix is the summary of the VpcPrefix
	VpcPrefix *APIVpcPrefixSummary `json:"vpcprefix,omitempty"`
	// VpcID is the VPC selected for automatic prefix allocation
	VpcID *string `json:"vpcId,omitempty"`
	// IPFamilies lists the address families requested for automatic prefix allocation
	IPFamilies []IPFamily `json:"ipFamilies,omitempty"`
	// Device is the device name of the Interface
	Device *string `json:"device"`
	// DeviceInstance is the ID of the DeviceInstance
	DeviceInstance *int `json:"deviceInstance"`
	// VirtualFunctionID is the ID of the Virtual Function
	VirtualFunctionID *int `json:"virtualFunctionId"`
	// IsPhysical indicates whether the network is bound on a physical Interface
	IsPhysical bool `json:"isPhysical"`
	// MacAddress is the MAC address of the Interface
	MacAddress *string `json:"macAddress"`
	// IPAddresses is the list of IP addresses assigned to the Interface
	IPAddresses []string `json:"ipAddresses"`
	// IPPrefixes lists reported IPv4 or IPv6 CIDRs, independently of IPAddresses.
	IPPrefixes []string `json:"ipPrefixes"`
	// RequestedIpAddress is the explicitly requested IP address for the Interface
	RequestedIpAddress *string `json:"requestedIpAddress"`
	// InlineRoutingProfile contains interface-local routing profile options.
	InlineRoutingProfile *APIInterfaceInlineRoutingProfile `json:"inlineRoutingProfile"`
	// Status is the status of the Interface
	Status string `json:"status"`
	// Created is the date and time the entity was created
	Created time.Time `json:"created"`
	// Updated is the date and time the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIInterface creates a new APIInterface
func NewAPIInterface(dbis *cdbm.Interface) *APIInterface {
	// Unknown prefixes still serialize as an empty JSON array, not null.
	apiInterface := &APIInterface{
		ID:                 dbis.ID.String(),
		InstanceID:         dbis.InstanceID.String(),
		IsPhysical:         dbis.IsPhysical,
		MacAddress:         dbis.MacAddress,
		IPAddresses:        dbis.IPAddresses,
		IPPrefixes:         append([]string{}, dbis.IPPrefixes...),
		RequestedIpAddress: dbis.RequestedIpAddress,
		Status:             dbis.Status,
		Created:            dbis.Created,
		Updated:            dbis.Updated,
	}

	if dbis.InlineRoutingProfile != nil {
		apiInterface.InlineRoutingProfile = &APIInterfaceInlineRoutingProfile{}
		apiInterface.InlineRoutingProfile.FromDB(dbis.InlineRoutingProfile)
	}

	if dbis.Instance != nil {
		apiInterface.Instance = NewAPIInstanceSummary(dbis.Instance)
	}

	if dbis.SubnetID != nil {
		apiInterface.SubnetID = cutil.GetPtr(dbis.SubnetID.String())
	}

	if dbis.Subnet != nil {
		apiInterface.Subnet = NewAPISubnetSummary(dbis.Subnet)
	}

	if dbis.VpcPrefixID != nil && dbis.VpcID == nil {
		apiInterface.VpcPrefixID = cutil.GetPtr(dbis.VpcPrefixID.String())
	}

	if dbis.VpcPrefix != nil && dbis.VpcID == nil {
		apiInterface.VpcPrefix = NewAPIVpcPrefixSummary(dbis.VpcPrefix)
	}

	if dbis.VpcID != nil {
		apiInterface.VpcID = cutil.GetPtr(dbis.VpcID.String())
	}

	if dbis.VpcIPFamilyMode != nil {
		switch *dbis.VpcIPFamilyMode {
		case cdbm.InterfaceVpcIPFamilyModeIPv4Only:
			apiInterface.IPFamilies = []IPFamily{IPFamilyIPv4}
		case cdbm.InterfaceVpcIPFamilyModeIPv6Only:
			apiInterface.IPFamilies = []IPFamily{IPFamilyIPv6}
		case cdbm.InterfaceVpcIPFamilyModeDualStack:
			apiInterface.IPFamilies = []IPFamily{IPFamilyIPv4, IPFamilyIPv6}
		}
	}

	if dbis.Device != nil {
		apiInterface.Device = dbis.Device
	}

	if dbis.DeviceInstance != nil {
		apiInterface.DeviceInstance = dbis.DeviceInstance
	}

	if dbis.VirtualFunctionID != nil {
		apiInterface.VirtualFunctionID = dbis.VirtualFunctionID
	}

	return apiInterface
}
