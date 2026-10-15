// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"math"
	"regexp"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationIs "github.com/go-ozzo/ozzo-validation/v4/is"
)

// Keep these, and the lengths checked in Validate, in sync with `AttachmentOvs` in
// `crates/agent/proto/weave_ew_vpc.proto`. Weave rejects a name outside them, but only once the
// DPU agent creates the attachment. So without them the request succeeds and the attachment never
// reaches Ready.
var (
	spectrumXAttachmentBridgeNameRegexp     = regexp.MustCompile(`^[a-zA-Z0-9_.-]+$`)
	spectrumXAttachmentOvnNetworkNameRegexp = regexp.MustCompile(`^[a-zA-Z0-9_-]+$`)
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
	// AttachmentType is the type of SpectrumX attachment: Physical, Virtual, or OVS
	AttachmentType cdbm.SpectrumXAttachmentType `json:"attachmentType"`
	// VirtualFunctionID must be omitted, as virtual functions are not currently supported
	VirtualFunctionID *int `json:"virtualFunctionId"`
	// BridgeName is the OVS bridge the attachment uses. Required for an OVS attachment and
	// must be omitted for any other attachment type.
	BridgeName *string `json:"bridgeName"`
	// OvnNetworkName is the OVN network the OVS attachment maps onto. Optional for an OVS
	// attachment and must be omitted for any other attachment type.
	OvnNetworkName *string `json:"ovnNetworkName"`
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
			validation.Min(0).Error("value must be equal or greater than 0"),
			validation.Max(int64(math.MaxUint32)).Error("value must not exceed 4294967295")),
		validation.Field(&sacr.AttachmentType,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(cdbm.SpectrumXAttachmentTypePhysical, cdbm.SpectrumXAttachmentTypeVirtual, cdbm.SpectrumXAttachmentTypeOVS).Error("must be one of 'Physical', 'Virtual', or 'OVS'")),
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

	// OVS metadata is client-owned config Core requires for an OVS attachment: bridge_name is
	// mandatory and ovn_network_name is optional. For any other type the fields carry no meaning
	// and must be omitted so a caller cannot silently attach OVS metadata to a Physical row.
	if sacr.AttachmentType == cdbm.SpectrumXAttachmentTypeOVS {
		err = validation.ValidateStruct(&sacr,
			validation.Field(&sacr.BridgeName,
				validation.Required.Error("bridgeName is required for an OVS attachment"),
				validation.Match(spectrumXAttachmentBridgeNameRegexp).Error("bridgeName can only contain letters, digits, '_', '.' and '-'"),
				validation.Length(1, 32).Error("bridgeName must be 32 characters or less")),
			validation.Field(&sacr.OvnNetworkName,
				validation.NilOrNotEmpty.Error("ovnNetworkName cannot be empty"),
				validation.Match(spectrumXAttachmentOvnNetworkNameRegexp).Error("ovnNetworkName can only contain letters, digits, '_' and '-'"),
				validation.Length(1, 256).Error("ovnNetworkName must be 256 characters or less")),
		)
		if err != nil {
			return err
		}
	} else {
		if sacr.BridgeName != nil {
			return validation.Errors{
				"bridgeName": errors.New("bridgeName is only supported for an OVS attachment"),
			}
		}
		if sacr.OvnNetworkName != nil {
			return validation.Errors{
				"ovnNetworkName": errors.New("ovnNetworkName is only supported for an OVS attachment"),
			}
		}
	}

	return nil
}

// ValidateSpectrumXAttachmentsForMachine checks selectors against a machine's persisted
// capabilities. A same-name generic NIC or DPU must not satisfy a SpectrumX
// request, and every attachment must fit its own device-description group.
func ValidateSpectrumXAttachmentsForMachine(capabilities []cdbm.MachineCapability, attachments []APISpectrumXAttachmentCreateOrUpdateRequest) error {
	for i, attachment := range attachments {
		matched := false
		for _, capability := range capabilities {
			if capability.Type == cdbm.MachineCapabilityTypeNetwork &&
				capability.DeviceType != nil && *capability.DeviceType == cdbm.MachineCapabilityDeviceTypeSpectrumX &&
				capability.Name == attachment.Device && capability.Count != nil &&
				attachment.DeviceInstance != nil && *attachment.DeviceInstance >= 0 && *attachment.DeviceInstance < *capability.Count {
				matched = true
				break
			}
		}
		if !matched {
			return validation.Errors{
				"spectrumXAttachments": validation.Errors{
					fmt.Sprint(i): errors.New("device and deviceInstance must select a SpectrumX interface in the Machine's capabilities"),
				},
			}
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
	// BridgeName is the OVS bridge the attachment uses, set only for an OVS attachment
	BridgeName *string `json:"bridgeName"`
	// OvnNetworkName is the OVN network the OVS attachment maps onto, set only for an OVS attachment
	OvnNetworkName *string `json:"ovnNetworkName"`
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
		BridgeName:           dbsxa.BridgeName,
		OvnNetworkName:       dbsxa.OvnNetworkName,
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
