// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

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
