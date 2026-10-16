// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"github.com/google/uuid"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIVpcAttachment is the owner-safe projection of an Interface attached to a VPC.
type APIVpcAttachment struct {
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

// FromDBModel populates the owner-safe attachment projection from a persisted Interface.
func (a *APIVpcAttachment) FromDBModel(ifc *cdbm.Interface, vpcID uuid.UUID) {
	a.ID = ifc.ID.String()
	a.VpcID = vpcID.String()
	if ifc.VpcPrefixID != nil {
		vpcPrefixID := ifc.VpcPrefixID.String()
		a.VpcPrefixID = &vpcPrefixID
	}
	a.InstanceID = ifc.InstanceID.String()
	if ifc.Instance != nil {
		a.InstanceTenantID = ifc.Instance.TenantID.String()
	}
	a.Device = ifc.Device
	a.DeviceInstance = ifc.DeviceInstance
	a.IsPhysical = ifc.IsPhysical
	a.VirtualFunctionID = ifc.VirtualFunctionID
	a.Status = ifc.Status
	a.Created = ifc.Created
	a.Updated = ifc.Updated
}
