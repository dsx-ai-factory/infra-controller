// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestValidateSpectrumXAttachmentsForMachine(t *testing.T) {
	capabilities := []cdbm.MachineCapability{
		{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(2), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{Type: cdbm.MachineCapabilityTypeNetwork, Name: "BlueField-3", Count: cutil.GetPtr(1), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{Type: cdbm.MachineCapabilityTypeNetwork, Name: "ConnectX-8", Count: cutil.GetPtr(8), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU)},
		{Type: cdbm.MachineCapabilityTypeNetwork, Name: "generic NIC", Count: cutil.GetPtr(4)},
		{Type: cdbm.MachineCapabilityTypeInfiniBand, Name: "wrong category", Count: cutil.GetPtr(4), DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
		{Type: cdbm.MachineCapabilityTypeNetwork, Name: "unknown count", DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX)},
	}
	attachment := func(device string, ordinal int) APISpectrumXAttachmentCreateOrUpdateRequest {
		return APISpectrumXAttachmentCreateOrUpdateRequest{Device: device, DeviceInstance: &ordinal}
	}
	for _, test := range []struct {
		name        string
		attachments []APISpectrumXAttachmentCreateOrUpdateRequest
		invalidAt   *int
	}{
		{name: "empty attachments impose no constraint"},
		{name: "all requested device groups match", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 1), attachment("BlueField-3", 0)}},
		{name: "every attachment must fit", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 1), attachment("BlueField-3", 1)}, invalidAt: cutil.GetPtr(1)},
		{name: "same-name DPU count cannot satisfy SpectrumX ordinal", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 2)}, invalidAt: cutil.GetPtr(0)},
		{name: "device name is case sensitive", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("connectx-8", 0)}, invalidAt: cutil.GetPtr(0)},
		{name: "generic network capability is insufficient", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("generic NIC", 0)}, invalidAt: cutil.GetPtr(0)},
		{name: "network category is required", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("wrong category", 0)}, invalidAt: cutil.GetPtr(0)},
		{name: "missing count supplies no capacity", attachments: []APISpectrumXAttachmentCreateOrUpdateRequest{attachment("unknown count", 0)}, invalidAt: cutil.GetPtr(0)},
	} {
		t.Run(test.name, func(t *testing.T) {
			err := ValidateSpectrumXAttachmentsForMachine(capabilities, test.attachments)
			if test.invalidAt == nil {
				require.NoError(t, err)
				return
			}
			var fields validation.Errors
			require.ErrorAs(t, err, &fields)
			var selectors validation.Errors
			require.ErrorAs(t, fields["spectrumXAttachments"], &selectors)
			assert.Contains(t, selectors, fmt.Sprint(*test.invalidAt))
		})
	}
}

func TestAPISpectrumXAttachmentCreateOrUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		spectrumXPartitionID string
		device               string
		deviceInstance       *int
		attachmentType       cdbm.SpectrumXAttachmentType
		virtualFunctionID    *int
	}
	tests := []struct {
		name   string
		fields fields
		// body, when set, is decoded instead of building the request from fields so a case can
		// exercise what an omitted or null JSON property actually decodes to.
		body    string
		wantErr bool
	}{
		{
			name: "test validation success, Physical attachment",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
			},
			wantErr: false,
		},
		{
			// Core rejects a Virtual attachment, so the REST layer rejects it up front even
			// though `Virtual` is a syntactically accepted attachmentType.
			name: "test validation failure, Virtual attachment type is not supported",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(3),
				attachmentType:       cdbm.SpectrumXAttachmentTypeVirtual,
			},
			wantErr: true,
		},
		{
			name: "test validation success, OVS attachment",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       cdbm.SpectrumXAttachmentTypeOVS,
			},
			wantErr: false,
		},
		{
			name: "test validation failure, invalid SpectrumX Partition ID",
			fields: fields{
				spectrumXPartitionID: "badid",
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
			},
			wantErr: true,
		},
		{
			name: "test validation failure, missing device",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
			},
			wantErr: true,
		},
		{
			name: "test validation failure, omitted deviceInstance",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				attachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
			},
			wantErr: true,
		},
		{
			name: "test validation failure, invalid attachmentType",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       "Bogus",
			},
			wantErr: true,
		},
		{
			name: "test validation failure, virtualFunctionId is not supported",
			fields: fields{
				spectrumXPartitionID: uuid.New().String(),
				device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
				deviceInstance:       cutil.GetPtr(0),
				attachmentType:       cdbm.SpectrumXAttachmentTypePhysical,
				virtualFunctionID:    cutil.GetPtr(2),
			},
			wantErr: true,
		},
		{
			name:    "test validation failure, deviceInstance omitted from the JSON body",
			body:    `{"spectrumXPartitionId":"8e6f2a1c-9b3d-4e5f-a6b7-c8d9e0f1a2b3","device":"NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC","attachmentType":"Physical"}`,
			wantErr: true,
		},
		{
			name:    "test validation failure, deviceInstance null in the JSON body",
			body:    `{"spectrumXPartitionId":"8e6f2a1c-9b3d-4e5f-a6b7-c8d9e0f1a2b3","device":"NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC","deviceInstance":null,"attachmentType":"Physical"}`,
			wantErr: true,
		},
		{
			name:    "test validation success, explicit zero deviceInstance in the JSON body",
			body:    `{"spectrumXPartitionId":"8e6f2a1c-9b3d-4e5f-a6b7-c8d9e0f1a2b3","device":"NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC","deviceInstance":0,"attachmentType":"Physical"}`,
			wantErr: false,
		},
		{
			name:    "deviceInstance exceeding uint32 cannot wrap during conversion",
			body:    `{"spectrumXPartitionId":"8e6f2a1c-9b3d-4e5f-a6b7-c8d9e0f1a2b3","device":"ConnectX-8","deviceInstance":4294967296,"attachmentType":"Physical"}`,
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sacr := APISpectrumXAttachmentCreateOrUpdateRequest{
				SpectrumXPartitionID: tt.fields.spectrumXPartitionID,
				Device:               tt.fields.device,
				DeviceInstance:       tt.fields.deviceInstance,
				AttachmentType:       tt.fields.attachmentType,
				VirtualFunctionID:    tt.fields.virtualFunctionID,
			}
			if tt.body != "" {
				sacr = APISpectrumXAttachmentCreateOrUpdateRequest{}
				require.NoError(t, json.Unmarshal([]byte(tt.body), &sacr))
			}
			err := sacr.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
