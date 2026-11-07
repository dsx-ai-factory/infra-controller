// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

var (
	// Time when nvLinklogicalPartitionId attribute will be deprecated
	nvLinkLogicalPartitionIDDeprecationTime = time.Date(2026, time.September, 10, 0, 0, 0, 0, time.UTC)

	// Deprecations for the NVLinkInterface model
	nvLinkLogicalPartitionIDDeprecations = []DeprecatedEntity{
		{
			OldValue:     "nvLinklogicalPartitionId",
			NewValue:     cutil.GetPtr("nvLinkLogicalPartitionId"),
			Type:         DeprecationTypeAttribute,
			TakeActionBy: nvLinkLogicalPartitionIDDeprecationTime,
		},
	}
)

// APINVLinkInterfaceCreateRequest is the data structure to capture user request to create a new NVLinkInterface
type APINVLinkInterfaceCreateOrUpdateRequest struct {
	// NVLinkLogicalPartitionID is the ID of the NVLinkLogicalPartition
	NVLinkLogicalPartitionID string `json:"nvLinkLogicalPartitionId"`
	// DeviceInstance is the index of the GPU
	DeviceInstance int `json:"deviceInstance"`
}

// Validate ensure the values passed in request are acceptable
func (nvlicr APINVLinkInterfaceCreateOrUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&nvlicr,
		validation.Field(&nvlicr.NVLinkLogicalPartitionID,
			validation.Required.Error(validationErrorValueRequired),
			validationIs.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&nvlicr.DeviceInstance,
			validation.Min(0).Error("deviceInstance must be between 0 and 3"),
			validation.Max(3).Error("deviceInstance must be between 0 and 3")),
	)

	if err != nil {
		return err
	}

	return err
}

// APINVLinkInterface is the data structure to capture NVLinkInterface
type APINVLinkInterface struct {
	// ID is the unique UUID v4 identifier for the NVLinkInterface
	ID string `json:"id"`
	// InstanceID is the ID of the associated Instance
	InstanceID string `json:"instanceId"`
	// Instance is the summary of the Instance
	Instance *APIInstanceSummary `json:"instance,omitempty"`
	// NVLinkLogicalPartitionIDDeprecated is the ID of the associated NVLinkLogicalPartition (deprecated)
	NVLinkLogicalPartitionIDDeprecated *string `json:"nvLinklogicalPartitionId,omitempty"`
	// NVLinkLogicalPartitionID is the ID of the associated NVLinkLogicalPartition
	NVLinkLogicalPartitionID string `json:"nvLinkLogicalPartitionId"`
	// NVLinkLogicalPartition is the summary of the NVLinkLogicalPartition
	NVLinkLogicalPartition *APINVLinkLogicalPartitionSummary `json:"nvLinkLogicalPartition,omitempty"`
	// NVLinkDomainID is the id of the physical NVLink domain that the interface is attached to
	NVLinkDomainID *string `json:"nvLinkDomainId"`
	// DeviceInstance is the index of the GPU
	DeviceInstance int `json:"deviceInstance"`
	// GpuGUID is the GUID of the GPU
	GpuGUID *string `json:"gpuGuid"`
	// Status is the status of the NVLinkInterface
	Status string `json:"status"`
	// Created is the date and time the entity was created
	Created time.Time `json:"created"`
	// Updated is the date and time the entity was last updated
	Updated time.Time `json:"updated"`
	// Deprecations is the list of deprecations for the NVLinkInterface
	Deprecations []APIDeprecation `json:"deprecations,omitempty"`
}

// NewAPINVLinkInterface creates a new APINVLinkInterface
func NewAPINVLinkInterface(dbnvli *cdbm.NVLinkInterface) *APINVLinkInterface {
	apiNVLinkInterface := &APINVLinkInterface{
		ID:                       dbnvli.ID.String(),
		InstanceID:               dbnvli.InstanceID.String(),
		NVLinkLogicalPartitionID: dbnvli.NVLinkLogicalPartitionID.String(),
		DeviceInstance:           dbnvli.DeviceInstance,
		Status:                   dbnvli.Status,
		Created:                  dbnvli.Created,
		Updated:                  dbnvli.Updated,
	}

	if dbnvli.Instance != nil {
		apiNVLinkInterface.Instance = NewAPIInstanceSummary(dbnvli.Instance)
	}

	if dbnvli.NVLinkDomainID != nil {
		apiNVLinkInterface.NVLinkDomainID = cutil.GetPtr(dbnvli.NVLinkDomainID.String())
	}

	if dbnvli.GpuGUID != nil {
		apiNVLinkInterface.GpuGUID = dbnvli.GpuGUID
	}

	if dbnvli.NVLinkLogicalPartition != nil {
		apiNVLinkInterface.NVLinkLogicalPartition = NewAPINVLinkLogicalPartitionSummary(dbnvli.NVLinkLogicalPartition)
	}

	if time.Now().Before(nvLinkLogicalPartitionIDDeprecationTime) {
		apiNVLinkInterface.NVLinkLogicalPartitionIDDeprecated = cutil.GetPtr(dbnvli.NVLinkLogicalPartitionID.String())
	}

	for _, deprecation := range nvLinkLogicalPartitionIDDeprecations {
		apiNVLinkInterface.Deprecations = append(apiNVLinkInterface.Deprecations, NewAPIDeprecation(deprecation))
	}

	return apiNVLinkInterface
}

// APINVLinkInterfaceSummary is the data structure to capture API summary of a NVLinkInterface
type APINVLinkInterfaceSummary struct {
	// ID of the NVLinkInterface
	ID string `json:"id"`
	// InstanceID is the ID of the Instance
	InstanceID string `json:"instanceId"`
	// NVLinkLogicalPartitionID is the ID of the NVLinkLogicalPartition
	NVLinkLogicalPartitionID string `json:"nvLinkLogicalPartitionId"`
	// DeviceInstance is the index of the GPU
	DeviceInstance int `json:"deviceInstance"`
	// Status is the status of the NVLinkInterface
	Status string `json:"status"`
}

// NewAPINVLinkInterfaceSummary accepts a DB layer NVLinkInterface object returns an API layer object
func NewAPINVLinkInterfaceSummary(dbnvli *cdbm.NVLinkInterface) *APINVLinkInterfaceSummary {
	apinvlifcs := APINVLinkInterfaceSummary{
		ID:                       dbnvli.ID.String(),
		InstanceID:               dbnvli.InstanceID.String(),
		NVLinkLogicalPartitionID: dbnvli.NVLinkLogicalPartitionID.String(),
		DeviceInstance:           dbnvli.DeviceInstance,
		Status:                   dbnvli.Status,
	}
	return &apinvlifcs
}
