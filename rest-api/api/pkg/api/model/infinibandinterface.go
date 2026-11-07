// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIInfiniBandInterfaceCreateRequest is the data structure to capture user request to create a new InfiniBandInterface
type APIInfiniBandInterfaceCreateOrUpdateRequest struct {
	// InfiniBandPartitionID is the ID of the InfiniBandPartition
	InfiniBandPartitionID string `json:"partitionId"`
	// Device is the name of the InfinitBand device to use
	Device string `json:"device"`
	// Vendor is the name of the vendor of the InfiniBand device
	Vendor *string `json:"vendor"`
	// DeviceInstance is the index of the device to use
	DeviceInstance int `json:"deviceInstance"`
	// IsPhysical indicates whether the Partition is attach to Instance over a physical Interface
	IsPhysical bool `json:"isPhysical"`
	// VirtualFunctionID must be specified if isPhysical is false
	VirtualFunctionID *int `json:"virtualFunctionId"`
}

// Validate ensure the values passed in request are acceptable
func (ibicr APIInfiniBandInterfaceCreateOrUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&ibicr,
		validation.Field(&ibicr.InfiniBandPartitionID,
			validation.Required.Error(validationErrorValueRequired),
			validationIs.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ibicr.Device,
			validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&ibicr.DeviceInstance,
			validation.Min(0).Error("value must be equal or greater than 0")),
	)

	if err != nil {
		return err
	}

	// `isPhysical` must be set to true for InfiniBand Interfaces
	if !ibicr.IsPhysical {
		return validation.Errors{
			"isPhysical": errors.New("must be set to true. Virtual functions are currently not supported for InfiniBand interfaces"),
		}
	}

	// `virtualFunctionId` supports
	if ibicr.VirtualFunctionID != nil {
		return validation.Errors{
			"virtualFunctionId": errors.New("virtual functions are currently not supported for InfiniBand interfaces"),
		}
	}

	return err
}

// APIInfiniBandInterface is the data structure to capture InfiniBandInterface
type APIInfiniBandInterface struct {
	// ID is the unique UUID v4 identifier for the InfiniBandInterface
	ID string `json:"id"`
	// InstanceID is the ID of the associated Instance
	InstanceID string `json:"instanceId"`
	// Instance is the summary of the Instance
	Instance *APIInstanceSummary `json:"instance,omitempty"`
	// InfiniBandPartitonID is the ID of the associated InfiniBandPartition
	InfiniBandPartitonID string `json:"partitionId"`
	// InfiniBandPartiton is the summary of the InfiniBandPartiton
	InfiniBandPartition *APIInfiniBandPartitionSummary `json:"partition,omitempty"`
	// Device is the name of the InfiniBand device
	Device string `json:"device"`
	// Vendor is the name of the vendor of the InfiniBand device
	Vendor *string `json:"vendor"`
	// DeviceInstance is the index of the device where partition attach to
	DeviceInstance int `json:"deviceInstance"`
	// IsPhysical indicates whether the Subnet is bound on a physical Interface
	IsPhysical bool `json:"isPhysical"`
	// VirtualFunctionID must be specified if isPhysical is false
	VirtualFunctionID *int `json:"virtualFunctionId"`
	// GUID must be specified if isPhysical is false
	GUID *string `json:"guid"`
	// Status is the status of the InfiniBandInterface
	Status string `json:"status"`
	// Created is the date and time the entity was created
	Created time.Time `json:"created"`
	// Updated is the date and time the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIInfiniBandInterface creates a new APIInfiniBandInterface
func NewAPIInfiniBandInterface(dbibi *cdbm.InfiniBandInterface) *APIInfiniBandInterface {
	apiInfiniBandInterface := &APIInfiniBandInterface{
		ID:                   dbibi.ID.String(),
		InstanceID:           dbibi.InstanceID.String(),
		InfiniBandPartitonID: dbibi.InfiniBandPartitionID.String(),
		Device:               dbibi.Device,
		Vendor:               dbibi.Vendor,
		DeviceInstance:       dbibi.DeviceInstance,
		IsPhysical:           dbibi.IsPhysical,
		VirtualFunctionID:    dbibi.VirtualFunctionID,
		GUID:                 dbibi.GUID,
		Status:               dbibi.Status,
		Created:              dbibi.Created,
		Updated:              dbibi.Updated,
	}

	if dbibi.Instance != nil {
		apiInfiniBandInterface.Instance = NewAPIInstanceSummary(dbibi.Instance)
	}

	if dbibi.InfiniBandPartition != nil {
		apiInfiniBandInterface.InfiniBandPartition = NewAPIInfiniBandPartitionSummary(dbibi.InfiniBandPartition)
	}

	return apiInfiniBandInterface
}
