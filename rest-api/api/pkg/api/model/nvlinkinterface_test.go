// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPINVLinkInterface(t *testing.T) {
	type args struct {
		dbnvli *cdbm.NVLinkInterface
	}

	dbnvli := &cdbm.NVLinkInterface{
		ID:                       uuid.New(),
		InstanceID:               uuid.New(),
		SiteID:                   uuid.New(),
		NVLinkLogicalPartitionID: uuid.New(),
		DeviceInstance:           1,
		Status:                   cdbm.NVLinkInterfaceStatusReady,
		Created:                  time.Now(),
		Updated:                  time.Now(),
	}

	tests := []struct {
		name string
		args args
		want *APINVLinkInterface
	}{
		{
			name: "test new API NVLink Interface initializer",
			args: args{
				dbnvli: dbnvli,
			},
			want: &APINVLinkInterface{
				ID:                       dbnvli.ID.String(),
				InstanceID:               dbnvli.InstanceID.String(),
				NVLinkLogicalPartitionID: dbnvli.NVLinkLogicalPartitionID.String(),
				DeviceInstance:           dbnvli.DeviceInstance,
				Status:                   dbnvli.Status,
				Created:                  dbnvli.Created,
				Updated:                  dbnvli.Updated,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPINVLinkInterface(tt.args.dbnvli)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.InstanceID, got.InstanceID)
			assert.Equal(t, tt.want.NVLinkLogicalPartitionID, got.NVLinkLogicalPartitionID)
			assert.Equal(t, tt.want.DeviceInstance, got.DeviceInstance)
			assert.Equal(t, tt.want.Status, got.Status)
			assert.Equal(t, tt.want.Created, got.Created)
			assert.Equal(t, tt.want.Updated, got.Updated)
		})
	}
}

func TestAPINVLinkInterfaceCreateOrUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		nvLinkLogicalPartitionID string
		deviceInstance           int
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test validation success",
			fields: fields{
				nvLinkLogicalPartitionID: uuid.New().String(),
				deviceInstance:           0,
			},
			wantErr: false,
		},
		{
			name: "test validation failure, invalid NVLink Logical Partition ID",
			fields: fields{
				nvLinkLogicalPartitionID: "badid",
				deviceInstance:           1,
			},
			wantErr: true,
		},
		{
			name: "test validation failure, GPU Index not supported",
			fields: fields{
				nvLinkLogicalPartitionID: uuid.New().String(),
				deviceInstance:           4,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			nvlicr := APINVLinkInterfaceCreateOrUpdateRequest{
				NVLinkLogicalPartitionID: tt.fields.nvLinkLogicalPartitionID,
				DeviceInstance:           tt.fields.deviceInstance,
			}
			err := nvlicr.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestNewAPINVLinkInterfaceSummary(t *testing.T) {
	type fields struct {
		status string
	}

	id := uuid.New()
	instanceID := uuid.New()
	nvllpID := uuid.New()

	tests := []struct {
		name   string
		fields fields
		want   *APINVLinkInterfaceSummary
	}{
		{
			name:   "maps Ready status",
			fields: fields{status: cdbm.NVLinkInterfaceStatusReady},
			want: &APINVLinkInterfaceSummary{
				ID:                       id.String(),
				InstanceID:               instanceID.String(),
				NVLinkLogicalPartitionID: nvllpID.String(),
				DeviceInstance:           2,
				Status:                   cdbm.NVLinkInterfaceStatusReady,
			},
		},
		{
			name:   "maps Pending status",
			fields: fields{status: cdbm.NVLinkInterfaceStatusPending},
			want: &APINVLinkInterfaceSummary{
				ID:                       id.String(),
				InstanceID:               instanceID.String(),
				NVLinkLogicalPartitionID: nvllpID.String(),
				DeviceInstance:           2,
				Status:                   cdbm.NVLinkInterfaceStatusPending,
			},
		},
		{
			name:   "maps Provisioning status",
			fields: fields{status: cdbm.NVLinkInterfaceStatusProvisioning},
			want: &APINVLinkInterfaceSummary{
				ID:                       id.String(),
				InstanceID:               instanceID.String(),
				NVLinkLogicalPartitionID: nvllpID.String(),
				DeviceInstance:           2,
				Status:                   cdbm.NVLinkInterfaceStatusProvisioning,
			},
		},
		{
			name:   "maps Error status",
			fields: fields{status: cdbm.NVLinkInterfaceStatusError},
			want: &APINVLinkInterfaceSummary{
				ID:                       id.String(),
				InstanceID:               instanceID.String(),
				NVLinkLogicalPartitionID: nvllpID.String(),
				DeviceInstance:           2,
				Status:                   cdbm.NVLinkInterfaceStatusError,
			},
		},
		{
			name:   "maps Deleting status",
			fields: fields{status: cdbm.NVLinkInterfaceStatusDeleting},
			want: &APINVLinkInterfaceSummary{
				ID:                       id.String(),
				InstanceID:               instanceID.String(),
				NVLinkLogicalPartitionID: nvllpID.String(),
				DeviceInstance:           2,
				Status:                   cdbm.NVLinkInterfaceStatusDeleting,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dbnvli := &cdbm.NVLinkInterface{
				ID:                       id,
				InstanceID:               instanceID,
				SiteID:                   uuid.New(),
				NVLinkLogicalPartitionID: nvllpID,
				DeviceInstance:           2,
				Status:                   tt.fields.status,
			}
			got := NewAPINVLinkInterfaceSummary(dbnvli)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestAPINVLinkInterfaceSummary_JSONStatusField(t *testing.T) {
	type fields struct {
		deviceInstance int
		status         string
	}
	tests := []struct {
		name   string
		fields fields
	}{
		{
			name: "json round-trip preserves Ready status",
			fields: fields{
				deviceInstance: 1,
				status:         cdbm.NVLinkInterfaceStatusReady,
			},
		},
		{
			name: "json round-trip preserves Pending status",
			fields: fields{
				deviceInstance: 0,
				status:         cdbm.NVLinkInterfaceStatusPending,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dbnvli := &cdbm.NVLinkInterface{
				ID:                       uuid.New(),
				InstanceID:               uuid.New(),
				SiteID:                   uuid.New(),
				NVLinkLogicalPartitionID: uuid.New(),
				DeviceInstance:           tt.fields.deviceInstance,
				Status:                   tt.fields.status,
			}
			summary := NewAPINVLinkInterfaceSummary(dbnvli)

			b, err := json.Marshal(summary)
			require.NoError(t, err)

			var raw map[string]any
			require.NoError(t, json.Unmarshal(b, &raw))
			assert.Equal(t, tt.fields.status, raw["status"])

			var roundTrip APINVLinkInterfaceSummary
			require.NoError(t, json.Unmarshal(b, &roundTrip))
			assert.Equal(t, summary.Status, roundTrip.Status)
		})
	}
}
