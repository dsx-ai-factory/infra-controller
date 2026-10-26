// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/go-ozzo/ozzo-validation/v4/is"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIInterfaceSummary is the owner-safe projection of a VPC-backed Interface.
type APIInterfaceSummary struct {
	ID                string    `json:"id"`
	VpcID             string    `json:"vpcId"`
	VpcPrefixID       *string   `json:"vpcPrefixId"`
	InstanceID        string    `json:"instanceId"`
	InstanceTenantID  string    `json:"instanceTenantId"`
	Device            *string   `json:"device"`
	DeviceInstance    *int      `json:"deviceInstance"`
	IsPhysical        bool      `json:"isPhysical"`
	VirtualFunctionID *int      `json:"virtualFunctionId"`
	Status            string    `json:"status"`
	Created           time.Time `json:"created"`
	Updated           time.Time `json:"updated"`
}

// APIInterfaceSummaryGetAllRequest captures optional filters for listing Interface summaries.
type APIInterfaceSummaryGetAllRequest struct {
	VpcID            *string `query:"vpcId"`
	CrossTenant      *bool   `query:"crossTenant"`
	InstanceTenantID *string `query:"instanceTenantId"`
}

// Validate ensures Interface summary filter IDs are valid UUIDs.
func (r APIInterfaceSummaryGetAllRequest) Validate() error {
	return validation.ValidateStruct(&r,
		validation.Field(&r.VpcID, validation.When(r.VpcID != nil, is.UUID)),
		validation.Field(&r.InstanceTenantID, validation.When(r.InstanceTenantID != nil, is.UUID)),
	)
}

// FromDBModel populates the owner-safe Interface projection from a persisted Interface.
func (s *APIInterfaceSummary) FromDBModel(ifc *cdbm.Interface) {
	s.ID = ifc.ID.String()
	if ifc.VpcID != nil {
		s.VpcID = ifc.VpcID.String()
	} else if ifc.VpcPrefix != nil {
		s.VpcID = ifc.VpcPrefix.VpcID.String()
	}
	// Inventory reconciliation also stores the resolved VPC Prefix on an Interface that selects
	// the VPC directly, so report a VPC Prefix only when it was the selector.
	if ifc.VpcID == nil && ifc.VpcPrefixID != nil {
		vpcPrefixID := ifc.VpcPrefixID.String()
		s.VpcPrefixID = &vpcPrefixID
	}
	s.InstanceID = ifc.InstanceID.String()
	if ifc.Instance != nil {
		s.InstanceTenantID = ifc.Instance.TenantID.String()
	}
	s.Device = ifc.Device
	s.DeviceInstance = ifc.DeviceInstance
	s.IsPhysical = ifc.IsPhysical
	s.VirtualFunctionID = ifc.VirtualFunctionID
	s.Status = ifc.Status
	s.Created = ifc.Created
	s.Updated = ifc.Updated
}
