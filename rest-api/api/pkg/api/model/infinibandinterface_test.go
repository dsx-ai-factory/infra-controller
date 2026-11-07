// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIInfiniBandInterface(t *testing.T) {
	type args struct {
		dbibi *cdbm.InfiniBandInterface
	}

	dbibi := &cdbm.InfiniBandInterface{
		ID:                    uuid.New(),
		InstanceID:            uuid.New(),
		SiteID:                uuid.New(),
		InfiniBandPartitionID: uuid.New(),
		Device:                "mlx5_0",
		Vendor:                cutil.GetPtr("Mellanox Technologies"),
		DeviceInstance:        1,
		IsPhysical:            false,
		VirtualFunctionID:     cutil.GetPtr(2),
		Status:                cdbm.InfiniBandInterfaceStatusReady,
		Created:               time.Now(),
		Updated:               time.Now(),
	}

	tests := []struct {
		name string
		args args
		want *APIInfiniBandInterface
	}{
		{
			name: "test new API InfiniBand Interface initializer",
			args: args{
				dbibi: dbibi,
			},
			want: &APIInfiniBandInterface{
				ID:                   dbibi.ID.String(),
				InstanceID:           dbibi.InstanceID.String(),
				InfiniBandPartitonID: dbibi.InfiniBandPartitionID.String(),
				Device:               dbibi.Device,
				Vendor:               dbibi.Vendor,
				DeviceInstance:       dbibi.DeviceInstance,
				IsPhysical:           dbibi.IsPhysical,
				VirtualFunctionID:    dbibi.VirtualFunctionID,
				Status:               dbibi.Status,
				Created:              dbibi.Created,
				Updated:              dbibi.Updated,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIInfiniBandInterface(tt.args.dbibi); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIInfiniBandInterface() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestAPIInfiniBandInterfaceCreateOrUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		partitionID       string
		device            string
		vendor            *string
		deviceInstance    int
		isPhysical        bool
		virtualFunctionID *int
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test validation success",
			fields: fields{
				partitionID:    uuid.New().String(),
				device:         "MT28908 Family [ConnectX-6]",
				vendor:         cutil.GetPtr("Mellanox Technologies"),
				deviceInstance: 1,
				isPhysical:     true,
			},
			wantErr: false,
		},
		{
			name: "test validation failure, invalid Partition ID",
			fields: fields{
				partitionID:    "badid",
				device:         "MT28908 Family [ConnectX-6]",
				deviceInstance: 1,
				isPhysical:     true,
			},
			wantErr: true,
		},
		{
			name: "test validation failure, Virtual Function not supported",
			fields: fields{
				partitionID:       uuid.New().String(),
				device:            "MT28908 Family [ConnectX-6]",
				deviceInstance:    1,
				isPhysical:        false,
				virtualFunctionID: cutil.GetPtr(3),
			},
			wantErr: true,
		},
		{
			name: "test validation failure, isPhysical is false not allowed",
			fields: fields{
				partitionID:    uuid.New().String(),
				device:         "MT28908 Family [ConnectX-6]",
				deviceInstance: 1,
				isPhysical:     false,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ibicr := APIInfiniBandInterfaceCreateOrUpdateRequest{
				InfiniBandPartitionID: tt.fields.partitionID,
				Device:                tt.fields.device,
				Vendor:                tt.fields.vendor,
				DeviceInstance:        tt.fields.deviceInstance,
				IsPhysical:            tt.fields.isPhysical,
				VirtualFunctionID:     tt.fields.virtualFunctionID,
			}
			err := ibicr.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
