// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
)

func TestGetAllMachineCapabilityHandler_Handle(t *testing.T) {
	ctx := context.Background()

	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := common.TestBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnRoles)
	common.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	st1 := common.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	st2 := common.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "x2.large", cutil.GetPtr(uuid.New()), st1, map[string]string{
		"name":        "x2.large",
		"description": "X2 Large Instance Type",
	}, ipu)

	m1 := common.TestBuildMachine(t, dbSession, ip, st1, nil, nil, cdbm.MachineStatusReady)
	common.TestBuildMachineCapability(t, dbSession, &m1.ID, nil, cdbm.MachineCapabilityTypeCPU, "Intel Xeon 6345", cutil.GetPtr("3.8Ghz"), nil, cutil.GetPtr("Genuine Intel"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m1.ID, nil, cdbm.MachineCapabilityTypeGPU, "Nvidia V100", nil, cutil.GetPtr("128GB"), cutil.GetPtr("Nvidia"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m1.ID, nil, cdbm.MachineCapabilityTypeStorage, "Dell Ent NVMe CM6 RI 1.92TB", nil, nil, nil, cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)

	m2 := common.TestBuildMachine(t, dbSession, ip, st1, &it.ID, nil, cdbm.MachineStatusReady)
	common.TestBuildMachineCapability(t, dbSession, &m2.ID, nil, cdbm.MachineCapabilityTypeCPU, "Intel Xeon 6345", cutil.GetPtr("3.8Ghz"), nil, cutil.GetPtr("Genuine Intel"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m2.ID, nil, cdbm.MachineCapabilityTypeMemory, "DDR4", nil, cutil.GetPtr("16GB"), nil, cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m1.ID, nil, cdbm.MachineCapabilityTypeStorage, "SSDPF2KE016T9L", nil, nil, nil, cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m2.ID, nil, cdbm.MachineCapabilityTypeNetwork, "BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller", nil, nil, nil, cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)

	common.TestBuildMachineInstanceType(t, dbSession, m2, it)

	m3 := common.TestBuildMachine(t, dbSession, ip, st2, nil, nil, cdbm.MachineStatusReady)
	common.TestBuildMachineCapability(t, dbSession, &m3.ID, nil, cdbm.MachineCapabilityTypeCPU, "Intel Celeron CX", cutil.GetPtr("2.4Ghz"), nil, cutil.GetPtr("Genuine Intel"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m3.ID, nil, cdbm.MachineCapabilityTypeMemory, "DDR4", nil, cutil.GetPtr("16GB"), nil, cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m3.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), []int{1, 3})

	m4 := common.TestBuildMachine(t, dbSession, ip, st2, nil, nil, cdbm.MachineStatusReady)
	common.TestBuildMachineCapability(t, dbSession, &m4.ID, nil, cdbm.MachineCapabilityTypeCPU, "Intel Celeron DX", cutil.GetPtr("2.4Ghz"), nil, cutil.GetPtr("Genuine Intel"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m4.ID, nil, cdbm.MachineCapabilityTypeMemory, "DDR4", nil, cutil.GetPtr("16GB"), nil, cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	common.TestBuildMachineCapability(t, dbSession, &m4.ID, nil, cdbm.MachineCapabilityTypeNetwork, "MT43244 BlueField-3 integrated ConnectX-7 network controller", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)
	common.TestBuildMachineCapability(t, dbSession, &m4.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type args struct {
		siteID          *uuid.UUID
		hasInstanceType *bool
		capabilityType  *string
		name            *string
		frequency       *string
		capacity        *string
		vendor          *string
		count           *int
		deviceType      *string
		inactiveDevices []string
		pageNumber      *int
		pageSize        *int
		orderBy         *string
	}
	tests := []struct {
		name                string
		org                 string
		args                args
		user                *cdbm.User
		wantRespCode        int
		wantRespCount       int
		wantType            *cdbm.MachineCapabilityType
		wantName            *string
		wantFrequency       *string
		wantCapacity        *string
		wantVendor          *string
		wantCount           *int
		wantDeviceType      *cdbm.MachineCapabilityDeviceType
		wantInactiveDevices []int
		wantFirstType       *cdbm.MachineCapabilityType
		wantOrderBy         *string
	}{
		{
			name: "success retrieving all distinct Machine Capabilities from a Site",
			org:  ipOrg,
			args: args{
				siteID: cutil.GetPtr(st1.ID),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 6,
			wantFirstType: cutil.GetPtr(cdbm.MachineCapabilityTypeCPU),
			wantOrderBy:   cutil.GetPtr("TYPE_ASC"),
		},
		{
			name: "success retrieving Machine Capabilities ordered by type descending",
			org:  ipOrg,
			args: args{
				siteID:  cutil.GetPtr(st1.ID),
				orderBy: cutil.GetPtr("TYPE_DESC"),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 6,
			wantFirstType: cutil.GetPtr(cdbm.MachineCapabilityTypeStorage),
			wantOrderBy:   cutil.GetPtr("TYPE_DESC"),
		},
		{
			name: "reject ordering distinct Machine Capabilities by creation time",
			org:  ipOrg,
			args: args{
				siteID:  cutil.GetPtr(st1.ID),
				orderBy: cutil.GetPtr("CREATED_ASC"),
			},
			user:         ipu,
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "success retrieving & filtering by Capability type",
			org:  ipOrg,
			args: args{
				siteID:         cutil.GetPtr(st1.ID),
				capabilityType: cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeStorage),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 2,
			wantType:      cutil.GetPtr(cdbm.MachineCapabilityTypeStorage),
		},
		{
			name: "success retrieving & filtering by Capability type and hasInstanceType set to true",
			org:  ipOrg,
			args: args{
				siteID:          cutil.GetPtr(st1.ID),
				hasInstanceType: cutil.GetPtr(true),
				capabilityType:  cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeMemory),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 1,
			wantType:      cutil.GetPtr(cdbm.MachineCapabilityTypeMemory),
		},
		{
			name: "success retrieving & filtering by Capability type and hasInstanceType set to true",
			org:  ipOrg,
			args: args{
				siteID:          cutil.GetPtr(st1.ID),
				hasInstanceType: cutil.GetPtr(false),
				capabilityType:  cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeMemory),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 0,
		},
		{
			name: "success retrieving & filtering by name",
			org:  ipOrg,
			args: args{
				siteID: cutil.GetPtr(st2.ID),
				name:   cutil.GetPtr("Intel Celeron CX"),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 1,
			wantName:      cutil.GetPtr("Intel Celeron CX"),
		},
		{
			name: "success retrieving & filtering by frequency",
			org:  ipOrg,
			args: args{
				siteID:    cutil.GetPtr(st2.ID),
				frequency: cutil.GetPtr("2.4Ghz"),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 2,
			wantFrequency: cutil.GetPtr("2.4Ghz"),
		},
		{
			name: "success retrieving & filtering by vendor & count",
			org:  ipOrg,
			args: args{
				siteID: cutil.GetPtr(st2.ID),
				vendor: cutil.GetPtr("Mellanox Technologies"),
				count:  cutil.GetPtr(2),
			},
			user:          ipu,
			wantRespCode:  http.StatusOK,
			wantRespCount: 3,
			wantVendor:    cutil.GetPtr("Mellanox Technologies"),
			wantCount:     cutil.GetPtr(2),
		},
		{
			name: "success retrieving & filtering by count & device type",
			org:  ipOrg,
			args: args{
				siteID:     cutil.GetPtr(st2.ID),
				deviceType: cutil.GetPtr("DPU"),
				count:      cutil.GetPtr(2),
			},
			user:           ipu,
			wantRespCode:   http.StatusOK,
			wantRespCount:  1,
			wantDeviceType: cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
			wantCount:      cutil.GetPtr(2),
		},
		{
			name: "success retrieving & filtering by inactive devices",
			org:  ipOrg,
			args: args{
				siteID:          cutil.GetPtr(st2.ID),
				inactiveDevices: []string{"1", "3"},
			},
			user:                ipu,
			wantRespCode:        http.StatusOK,
			wantRespCount:       1,
			wantInactiveDevices: []int{1, 3},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gamch := GetAllMachineCapabilityHandler{
				dbSession: dbSession,
			}

			e := echo.New()

			q := url.Values{}
			if tt.args.siteID != nil {
				q.Set("siteId", tt.args.siteID.String())
			}
			if tt.args.hasInstanceType != nil {
				q.Set("hasInstanceType", fmt.Sprintf("%v", *tt.args.hasInstanceType))
			}
			if tt.args.capabilityType != nil {
				q.Set("type", *tt.args.capabilityType)
			}
			if tt.args.name != nil {
				q.Set("name", *tt.args.name)
			}
			if tt.args.frequency != nil {
				q.Set("frequency", *tt.args.frequency)
			}
			if tt.args.capacity != nil {
				q.Set("capacity", *tt.args.capacity)
			}
			if tt.args.vendor != nil {
				q.Set("vendor", *tt.args.vendor)
			}
			if tt.args.count != nil {
				q.Set("count", fmt.Sprintf("%v", *tt.args.count))
			}
			if tt.args.deviceType != nil {
				q.Set("devicetype", *tt.args.deviceType)
			}
			if tt.args.inactiveDevices != nil {
				q["inactiveDevices"] = tt.args.inactiveDevices
			}
			if tt.args.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tt.args.pageNumber))
			}
			if tt.args.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tt.args.pageSize))
			}
			if tt.args.orderBy != nil {
				q.Set("orderBy", *tt.args.orderBy)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/machine-capability?%s", tt.org, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.org)
			if tt.user != nil {
				ec.Set("user", tt.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gamch.Handle(ec)
			assert.NoError(t, err)

			assert.Equal(t, tt.wantRespCode, rec.Code)
			if tt.wantRespCode != rec.Code {
				t.Logf("response body: %s", rec.Body.String())
			}
			if rec.Code != http.StatusOK {
				return
			}

			resp := []model.APIMachineCapability{}
			err = json.Unmarshal(rec.Body.Bytes(), &resp)
			assert.Nil(t, err)
			assert.Equal(t, tt.wantRespCount, len(resp))
			if tt.wantFirstType != nil && assert.NotEmpty(t, resp) {
				assert.Equal(t, *tt.wantFirstType, resp[0].Type)
			}
			if tt.wantOrderBy != nil {
				pageResp := pagination.PageResponse{}
				err = json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &pageResp)
				assert.NoError(t, err)
				assert.Equal(t, tt.wantOrderBy, pageResp.OrderBy)
			}

			for _, r := range resp {
				if tt.wantType != nil {
					assert.Equal(t, *tt.wantType, r.Type)
				}
				if tt.wantName != nil {
					assert.Equal(t, *tt.wantName, r.Name)
				}
				if tt.wantFrequency != nil {
					assert.Equal(t, *tt.wantFrequency, *r.Frequency)
				}
				if tt.wantCapacity != nil {
					assert.Equal(t, *tt.wantCapacity, *r.Capacity)
				}
				if tt.wantVendor != nil {
					assert.Equal(t, *tt.wantVendor, *r.Vendor)
				}
				if tt.wantCount != nil {
					assert.Equal(t, *tt.wantCount, *r.Count)
				}
				if tt.wantDeviceType != nil {
					assert.Equal(t, *tt.wantDeviceType, *r.DeviceType)
				}
				if tt.wantInactiveDevices != nil {
					assert.Equal(t, tt.wantInactiveDevices, r.InactiveDevices)
				}
			}
		})
	}
}

func TestNewGetAllMachineCapabilityHandler(t *testing.T) {
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	type args struct {
		dbSession *cdb.Session
	}
	tests := []struct {
		name string
		args args
		want GetAllMachineCapabilityHandler
	}{
		{
			name: "success creating a new GetAllMachineCapabilityHandler",
			args: args{
				dbSession: dbSession,
			},
			want: GetAllMachineCapabilityHandler{
				dbSession: dbSession,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewGetAllMachineCapabilityHandler(tt.args.dbSession)
			assert.IsType(t, tt.want, got)
		})
	}
}
