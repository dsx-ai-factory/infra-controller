// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"
)

// SpectrumXAttachmentType is how an Instance attaches to a SpectrumX Partition.
type SpectrumXAttachmentType string

const (
	// SpectrumXAttachmentTypePhysical attaches the SpectrumX Partition over a physical interface
	SpectrumXAttachmentTypePhysical SpectrumXAttachmentType = "Physical"
	// SpectrumXAttachmentTypeVirtual attaches the SpectrumX Partition over a virtual function
	SpectrumXAttachmentTypeVirtual SpectrumXAttachmentType = "Virtual"
	// SpectrumXAttachmentTypeOVN attaches the SpectrumX Partition over OVN
	SpectrumXAttachmentTypeOVN SpectrumXAttachmentType = "OVN"
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
	AttachmentType SpectrumXAttachmentType `json:"attachmentType"`
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
			validation.In(SpectrumXAttachmentTypePhysical, SpectrumXAttachmentTypeVirtual, SpectrumXAttachmentTypeOVN).Error("must be one of 'Physical', 'Virtual', or 'OVN'")),
	)
	if err != nil {
		return err
	}

	// Core's allocate_spx_port_mac rejects a Virtual attachment, so reject it here and give the
	// caller a 400 rather than a Site failure. Enabling it later only widens what is accepted.
	if sacr.AttachmentType == SpectrumXAttachmentTypeVirtual {
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

// ToProto converts the validated request to forge.InstanceSpxAttachment.
func (sacr APISpectrumXAttachmentCreateOrUpdateRequest) ToProto() *corev1.InstanceSpxAttachment {
	attachment := &corev1.InstanceSpxAttachment{
		Device:         sacr.Device,
		DeviceInstance: uint32(*sacr.DeviceInstance),
		SpxPartitionId: &corev1.SpxPartitionId{Value: sacr.SpectrumXPartitionID},
		AttachmentType: sacr.AttachmentType.ToProto(),
	}
	if sacr.VirtualFunctionID != nil {
		vfID := uint32(*sacr.VirtualFunctionID)
		attachment.VirtualFunctionId = &vfID
	}
	return attachment
}

// ToProto converts a SpectrumXAttachmentType into its Core proto enum. An unrecognized
// value returns Physical, the zero enum, because Validate is the gate that rejects it
// upstream of the wire.
func (t SpectrumXAttachmentType) ToProto() corev1.SpxAttachmentType {
	switch t {
	case SpectrumXAttachmentTypePhysical:
		return corev1.SpxAttachmentType_Physical
	case SpectrumXAttachmentTypeVirtual:
		return corev1.SpxAttachmentType_Virtual
	case SpectrumXAttachmentTypeOVN:
		return corev1.SpxAttachmentType_Ovn
	default:
		return corev1.SpxAttachmentType_Physical
	}
}
