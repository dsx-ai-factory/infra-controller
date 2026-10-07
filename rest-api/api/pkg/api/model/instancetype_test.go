// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPIInstanceType(t *testing.T) {
	type args struct {
		dbit  *cdbm.InstanceType
		dbsds []cdbm.StatusDetail
		mcs   []cdbm.MachineCapability
		mit   []cdbm.MachineInstanceType
	}

	dbit := &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     "test-name",
		Description:              cutil.GetPtr("test-description"),
		ControllerMachineType:    cutil.GetPtr("test-controller-machine-type"),
		InfrastructureProviderID: uuid.New(),
		SiteID:                   cutil.GetPtr(uuid.New()),
		Status:                   "test-status",
		Created:                  time.Now(),
		Updated:                  time.Now(),
	}

	dbsd := cdbm.StatusDetail{
		ID:       uuid.New(),
		EntityID: dbit.ID.String(),
		Status:   "test-status",
		Message:  cutil.GetPtr("test-message"),
		Created:  time.Now(),
		Updated:  time.Now(),
	}

	dbmc := cdbm.MachineCapability{
		ID:             uuid.New(),
		InstanceTypeID: &dbit.ID,
		Type:           "test-type",
		Name:           "test-name",
		Capacity:       cutil.GetPtr("test-capacity"),
		Count:          cutil.GetPtr(2),
		Created:        time.Now(),
		Updated:        time.Now(),
	}

	mit := cdbm.MachineInstanceType{
		ID:             uuid.New(),
		MachineID:      uuid.New().String(),
		InstanceTypeID: dbit.ID,
	}

	tests := []struct {
		name string
		args args
		want *APIInstanceType
	}{
		{
			name: "test new API Instance Type initializer",
			args: args{
				dbit:  dbit,
				dbsds: []cdbm.StatusDetail{dbsd},
				mcs:   []cdbm.MachineCapability{dbmc},
				mit:   []cdbm.MachineInstanceType{},
			},
			want: &APIInstanceType{
				ID:                       dbit.ID.String(),
				Name:                     dbit.Name,
				Description:              dbit.Description,
				ControllerMachineType:    dbit.ControllerMachineType,
				InfrastructureProviderID: dbit.InfrastructureProviderID.String(),
				SiteID:                   dbit.SiteID.String(),
				Status:                   dbit.Status,
				Created:                  dbit.Created,
				Updated:                  dbit.Updated,
				StatusHistory: []APIStatusDetail{
					{
						Status:  dbsd.Status,
						Message: dbsd.Message,
						Created: dbsd.Created,
						Updated: dbsd.Updated,
					},
				},
				MachineCapabilities: []APIMachineCapability{
					{
						Type:     dbmc.Type,
						Name:     dbmc.Name,
						Capacity: dbmc.Capacity,
						Count:    dbmc.Count,
					},
				},
				MachineInstanceTypes: []APIMachineInstanceType{},
			},
		},
		{
			name: "test new API Instance Type initializer with deprecation",
			args: args{
				dbit:  dbit,
				dbsds: []cdbm.StatusDetail{dbsd},
				mcs:   []cdbm.MachineCapability{dbmc},
				mit:   []cdbm.MachineInstanceType{mit},
			},
			want: func() *APIInstanceType {
				expected := &APIInstanceType{
					ID:                       dbit.ID.String(),
					Name:                     dbit.Name,
					Description:              dbit.Description,
					ControllerMachineType:    dbit.ControllerMachineType,
					InfrastructureProviderID: dbit.InfrastructureProviderID.String(),
					SiteID:                   dbit.SiteID.String(),
					Status:                   dbit.Status,
					Created:                  dbit.Created,
					Updated:                  dbit.Updated,
					StatusHistory: []APIStatusDetail{
						{
							Status:  dbsd.Status,
							Message: dbsd.Message,
							Created: dbsd.Created,
							Updated: dbsd.Updated,
						},
					},
					MachineCapabilities: []APIMachineCapability{
						{
							Type:     dbmc.Type,
							Name:     dbmc.Name,
							Capacity: dbmc.Capacity,
							Count:    dbmc.Count,
						},
					},
					MachineInstanceTypes: []APIMachineInstanceType{
						*NewAPIMachineInstanceType(&mit),
					},
				}

				return expected
			}(),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIInstanceType(tt.args.dbit, tt.args.dbsds, tt.args.mcs, tt.args.mit, nil); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIInstanceType() = %+v, want %+v", got, tt.want)
			}
		})
	}
}

func TestAPIInstanceTypeCreateRequest_Validate(t *testing.T) {
	type fields struct {
		Name                  string
		Description           *string
		SiteID                string
		Labels                map[string]string
		ControllerMachineType *string
		MachineCapabilities   []APIMachineCapability
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid Instance Type create request",
			fields: fields{
				Name:        "test-name",
				Description: cutil.GetPtr("test-description"),
				SiteID:      uuid.New().String(),
				Labels: map[string]string{
					"name":        "a-nv100-instance",
					"description": "",
				},
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:     cdbm.MachineCapabilityTypeCPU,
						Name:     "AMD Opteron Series x10",
						Capacity: cutil.GetPtr("3.0GHz"),
						Count:    cutil.GetPtr(2),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance Type create request - invalid Site ID",
			fields: fields{
				Name:                  "test-name",
				Description:           cutil.GetPtr("test-description"),
				SiteID:                "",
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:     cdbm.MachineCapabilityTypeCPU,
						Name:     "AMD Opteron Series x10",
						Capacity: cutil.GetPtr("3.0GHz"),
						Count:    cutil.GetPtr(2),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - invalid Labels",
			fields: fields{
				Name:        "test-name",
				Description: cutil.GetPtr("test-description"),
				SiteID:      uuid.New().String(),
				Labels: map[string]string{
					"name": "a-nv100-instance",
					"":     "test",
				},
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:     cdbm.MachineCapabilityTypeCPU,
						Name:     "AMD Opteron Series x10",
						Capacity: cutil.GetPtr("3.0GHz"),
						Count:    cutil.GetPtr(2),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - invalid Machine Capability type",
			fields: fields{
				Name:                  "test-name",
				Description:           cutil.GetPtr("test-description"),
				SiteID:                uuid.New().String(),
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:     "test-type",
						Name:     "test-name",
						Capacity: cutil.GetPtr("test-capacity"),
						Count:    cutil.GetPtr(1),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - multiple Machine Capability specified with same name",
			fields: fields{
				Name:                  "test-name",
				Description:           cutil.GetPtr("test-description"),
				SiteID:                uuid.New().String(),
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:  cdbm.MachineCapabilityTypeCPU,
						Name:  "test-name",
						Count: cutil.GetPtr(1),
					},
					{
						Type:     cdbm.MachineCapabilityTypeCPU,
						Name:     "test-name",
						Capacity: cutil.GetPtr("test-capacity"),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - invalid Machine Capability device type",
			fields: fields{
				Name:                  "test-name",
				Description:           cutil.GetPtr("test-description"),
				SiteID:                uuid.New().String(),
				ControllerMachineType: cutil.GetPtr("test-controller-machine-type"),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:       cdbm.MachineCapabilityTypeNetwork,
						Name:       "test-name",
						DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceType("test-device-type")),
						Count:      cutil.GetPtr(1),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - GPU capability requires NVLink device type",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:       cdbm.MachineCapabilityTypeGPU,
						Name:       "gpu-0",
						DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance Type create request - GPU + NVLink device type",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:       cdbm.MachineCapabilityTypeGPU,
						Name:       "gpu-0",
						DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance Type create request - device type on capability that does not allow one",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:       cdbm.MachineCapabilityTypeCPU,
						Name:       "cpu-0",
						DeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - InactiveDevices on non-InfiniBand capability",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:            cdbm.MachineCapabilityTypeStorage,
						Name:            "storage-0",
						InactiveDevices: []int{1, 2},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test valid Instance Type create request - InactiveDevices on InfiniBand capability",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:            cdbm.MachineCapabilityTypeInfiniBand,
						Name:            "ib-0",
						InactiveDevices: []int{1, 2},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid Instance Type create request - InactiveDevices contains negative entry",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:            cdbm.MachineCapabilityTypeInfiniBand,
						Name:            "ib-0",
						InactiveDevices: []int{1, -1, 2},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - InactiveDevices entry exceeds uint32",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:            cdbm.MachineCapabilityTypeInfiniBand,
						Name:            "ib-0",
						InactiveDevices: []int{math.MaxUint32 + 1},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - Count exceeds uint32",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:  cdbm.MachineCapabilityTypeCPU,
						Name:  "cpu-0",
						Count: cutil.GetPtr(math.MaxUint32 + 1),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Instance Type create request - Count is negative",
			fields: fields{
				Name:   "test-name",
				SiteID: uuid.New().String(),
				MachineCapabilities: []APIMachineCapability{
					{
						Type:  cdbm.MachineCapabilityTypeCPU,
						Name:  "cpu-0",
						Count: cutil.GetPtr(-1),
					},
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			itcr := APIInstanceTypeCreateRequest{
				Name:                  tt.fields.Name,
				Description:           tt.fields.Description,
				SiteID:                tt.fields.SiteID,
				Labels:                tt.fields.Labels,
				ControllerMachineType: tt.fields.ControllerMachineType,
				MachineCapabilities:   tt.fields.MachineCapabilities,
			}
			err := itcr.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestAPIInstanceTypeUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		Name        *string
		Description *string
		Labels      map[string]string
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid Instance Type update request",
			fields: fields{
				Name:        cutil.GetPtr("test-name"),
				Description: cutil.GetPtr("test-description"),
				Labels: map[string]string{
					"name":        "a-nv100-instance",
					"description": "",
				},
			},
			wantErr: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			itur := APIInstanceTypeUpdateRequest{
				Name:        tt.fields.Name,
				Description: tt.fields.Description,
				Labels:      tt.fields.Labels,
			}
			err := itur.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestAPIInstanceTypeCreateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"

	t.Run("derives id metadata and capabilities from the entity", func(t *testing.T) {
		it := &cdbm.InstanceType{
			ID:          id,
			Name:        "small",
			Description: &desc,
			Labels:      map[string]string{"env": "prod"},
			Capabilities: []*cdbm.MachineCapability{
				{Type: cdbm.MachineCapabilityTypeCPU, Name: "cpu-0"},
			},
		}
		itcr := APIInstanceTypeCreateRequest{}
		req := itcr.ToProto(it)
		require.NotNil(t, req)
		require.NotNil(t, req.Id)
		assert.Equal(t, id.String(), *req.Id)
		require.NotNil(t, req.Metadata)
		assert.Equal(t, "small", req.Metadata.Name)
		assert.Equal(t, "primary", req.Metadata.Description)
		require.Len(t, req.Metadata.Labels, 1)
		assert.Equal(t, "env", req.Metadata.Labels[0].Key)
		require.NotNil(t, req.Metadata.Labels[0].Value)
		assert.Equal(t, "prod", *req.Metadata.Labels[0].Value)
		require.NotNil(t, req.InstanceTypeAttributes)
		require.Len(t, req.InstanceTypeAttributes.DesiredCapabilities, 1)
		assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_CPU,
			req.InstanceTypeAttributes.DesiredCapabilities[0].CapabilityType)
	})

	t.Run("nil description, labels, and capabilities yield empty metadata + nil filter", func(t *testing.T) {
		it := &cdbm.InstanceType{ID: id, Name: "small"}
		itcr := APIInstanceTypeCreateRequest{}
		req := itcr.ToProto(it)
		require.NotNil(t, req)
		assert.Equal(t, "", req.Metadata.Description)
		assert.Nil(t, req.Metadata.Labels)
		require.NotNil(t, req.InstanceTypeAttributes)
		assert.Nil(t, req.InstanceTypeAttributes.DesiredCapabilities)
	})
}

func TestAPIInstanceTypeUpdateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"

	t.Run("derives id metadata from the post-merge entity, no caps yields nil filter", func(t *testing.T) {
		it := &cdbm.InstanceType{
			ID:          id,
			Name:        "small",
			Description: &desc,
		}
		itur := APIInstanceTypeUpdateRequest{}
		req := itur.ToProto(it)
		require.NotNil(t, req)
		assert.Equal(t, id.String(), req.Id)
		require.NotNil(t, req.Metadata)
		assert.Equal(t, "small", req.Metadata.Name)
		assert.Equal(t, "primary", req.Metadata.Description)
		require.NotNil(t, req.InstanceTypeAttributes)
		assert.Nil(t, req.InstanceTypeAttributes.DesiredCapabilities)
	})

	t.Run("populates capabilities from the entity when provided", func(t *testing.T) {
		it := &cdbm.InstanceType{
			ID:   id,
			Name: "small",
			Capabilities: []*cdbm.MachineCapability{
				{Type: cdbm.MachineCapabilityTypeCPU, Name: "cpu-0"},
			},
		}
		itur := APIInstanceTypeUpdateRequest{}
		req := itur.ToProto(it)
		require.NotNil(t, req.InstanceTypeAttributes)
		require.Len(t, req.InstanceTypeAttributes.DesiredCapabilities, 1)
		assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_CPU,
			req.InstanceTypeAttributes.DesiredCapabilities[0].CapabilityType)
	})
}
