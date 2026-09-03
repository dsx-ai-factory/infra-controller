// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"
)

// APISpectrumXAttachmentCreateOrUpdateRequest is the data structure to capture a user request to attach a SpectrumX Partition to an Instance
type APISpectrumXAttachmentCreateOrUpdateRequest struct {
	// SpectrumXPartitionID is the ID of the SpectrumX Partition
	SpectrumXPartitionID string `json:"spectrumXPartitionId"`
	// Device is the SpectrumX device to attach over, matching the device description reported
	// for the Machine's SpectrumX interfaces
	Device string `json:"device"`
	// DeviceInstance is the index of the device to use. This is a pointer so that an omitted
	// property is rejected rather than decoding to 0 and attaching to the first device.
	DeviceInstance *int `json:"deviceInstance"`
	// AttachmentType is the type of SpectrumX attachment: Physical, Virtual, or OVN
	AttachmentType cdbm.SpectrumXAttachmentType `json:"attachmentType"`
	// VirtualFunctionID must be omitted, as virtual functions are not currently supported
	VirtualFunctionID *int `json:"virtualFunctionId"`
}

// Validate ensures the values passed in request are acceptable
func (sacr APISpectrumXAttachmentCreateOrUpdateRequest) Validate() error {
	err := validation.ValidateStruct(&sacr,
		validation.Field(&sacr.SpectrumXPartitionID,
			validation.Required.Error(validationErrorValueRequired),
			validationIs.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&sacr.Device,
			validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&sacr.DeviceInstance,
			validation.NotNil.Error(validationErrorValueRequired),
			validation.Min(0).Error("value must be equal or greater than 0")),
		validation.Field(&sacr.AttachmentType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(cdbm.SpectrumXAttachmentTypePhysical, cdbm.SpectrumXAttachmentTypeVirtual, cdbm.SpectrumXAttachmentTypeOVN).Error("must be one of 'Physical', 'Virtual', or 'OVN'")),
	)
	if err != nil {
		return err
	}

	// Core's allocate_spx_port_mac rejects a Virtual attachment, so reject it here and give the
	// caller a 400 rather than a Site failure. Enabling it later only widens what is accepted.
	if sacr.AttachmentType == cdbm.SpectrumXAttachmentTypeVirtual {
		return validation.Errors{
			"attachmentType": errors.New("virtual functions are currently not supported for SpectrumX attachments"),
		}
	}

	if sacr.VirtualFunctionID != nil {
		return validation.Errors{
			"virtualFunctionId": errors.New("virtual functions are currently not supported for SpectrumX attachments"),
		}
	}

	return nil
}

// APISpectrumXAttachment is the data structure to capture the API representation of a
// SpectrumX Attachment on an Instance.
//
// MacAddress and IPAddress are allocated by the Site and reported back through Instance
// inventory, so they are absent until the attachment reaches Ready.
type APISpectrumXAttachment struct {
	// ID is the unique UUID v4 identifier for the SpectrumX Attachment
	ID string `json:"id"`
	// InstanceID is the ID of the associated Instance
	InstanceID string `json:"instanceId"`
	// Instance is the summary of the Instance
	Instance *APIInstanceSummary `json:"instance,omitempty"`
	// SpectrumXPartitionID is the ID of the associated SpectrumX Partition
	SpectrumXPartitionID string `json:"spectrumXPartitionId"`
	// SpectrumXPartition is the summary of the SpectrumX Partition
	SpectrumXPartition *APISpectrumXPartitionSummary `json:"spectrumXPartition,omitempty"`
	// Device is the SpectrumX device the Partition is attached over
	Device string `json:"device"`
	// DeviceInstance is the index of the device the Partition is attached to
	DeviceInstance int `json:"deviceInstance"`
	// AttachmentType is the type of SpectrumX attachment
	AttachmentType cdbm.SpectrumXAttachmentType `json:"attachmentType"`
	// VirtualFunctionID is the virtual function the attachment uses
	VirtualFunctionID *int `json:"virtualFunctionId"`
	// MacAddress is the MAC address the Site allocated for the attachment
	MacAddress *string `json:"macAddress"`
	// IPAddress is the IP address the Site allocated for the attachment
	IPAddress *string `json:"ipAddress"`
	// Status is the status of the SpectrumX Attachment
	Status string `json:"status"`
	// Created is the date and time the entity was created
	Created time.Time `json:"created"`
	// Updated is the date and time the entity was last updated
	Updated time.Time `json:"updated"`
}

// NewAPISpectrumXAttachment accepts a DB layer SpectrumXAttachment object and returns an
// API layer object. Returns nil for a nil DB model.
func NewAPISpectrumXAttachment(dbsxa *cdbm.SpectrumXAttachment) *APISpectrumXAttachment {
	if dbsxa == nil {
		return nil
	}

	apiSxa := &APISpectrumXAttachment{
		ID:                   dbsxa.ID.String(),
		InstanceID:           dbsxa.InstanceID.String(),
		SpectrumXPartitionID: dbsxa.SpectrumXPartitionID.String(),
		Device:               dbsxa.Device,
		DeviceInstance:       dbsxa.DeviceInstance,
		AttachmentType:       dbsxa.AttachmentType,
		VirtualFunctionID:    dbsxa.VirtualFunctionID,
		MacAddress:           dbsxa.MacAddress,
		IPAddress:            dbsxa.IPAddress,
		Status:               dbsxa.Status,
		Created:              dbsxa.Created,
		Updated:              dbsxa.Updated,
	}

	if dbsxa.Instance != nil {
		apiSxa.Instance = NewAPIInstanceSummary(dbsxa.Instance)
	}

	if dbsxa.SpectrumXPartition != nil {
		apiSxa.SpectrumXPartition = NewAPISpectrumXPartitionSummary(dbsxa.SpectrumXPartition)
	}

	return apiSxa
}
