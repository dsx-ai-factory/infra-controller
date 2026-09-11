// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"

	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
)

func TestCreateInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}
	type args struct {
		reqData *model.APIInstanceTypeCreateRequest
	}

	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, orgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	itcrValid := &model.APIInstanceTypeCreateRequest{
		Name:        "x2.large",
		Description: sutil.GetPtr("Test Description"),
		SiteID:      st.ID.String(),
		Labels: map[string]string{
			"name":        "a-dpu-instance",
			"description": "Multi-DPU Instance Type",
		},
		ControllerMachineType: sutil.GetPtr("intel_xeon_e5_2650v2"),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:      cdbm.MachineCapabilityTypeCPU,
				Name:      "AMD Opteron Series x10",
				Frequency: sutil.GetPtr("3.0Hz"),
				Cores:     sutil.GetPtr(16),
				Threads:   sutil.GetPtr(32),
				Count:     sutil.GetPtr(2),
			},
			{
				Type:     cdbm.MachineCapabilityTypeMemory,
				Name:     "Corsair Vengeance LPX",
				Capacity: sutil.GetPtr("32GB"),
				Count:    sutil.GetPtr(4),
			},
			{
				Type:            cdbm.MachineCapabilityTypeInfiniBand,
				Name:            "MT28908 Family [ConnectX-6]",
				Vendor:          sutil.GetPtr("Mellanox Technologies"),
				Count:           sutil.GetPtr(2),
				InactiveDevices: []int{1, 3},
			},
			{
				Type:       cdbm.MachineCapabilityTypeNetwork,
				Name:       "MT43244 BlueField-3 integrated ConnectX-7 network controller",
				Vendor:     sutil.GetPtr("Mellanox Technologies"),
				DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
				Count:      sutil.GetPtr(2),
			},
		},
	}

	itcrInValidMachineCapabilitiesName := &model.APIInstanceTypeCreateRequest{
		Name:                  "x2.large",
		Description:           sutil.GetPtr("Test Description"),
		SiteID:                st.ID.String(),
		ControllerMachineType: sutil.GetPtr("intel_xeon_e5_2650v2"),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:      cdbm.MachineCapabilityTypeCPU,
				Name:      " ",
				Frequency: sutil.GetPtr("3.0Hz"),
				Cores:     sutil.GetPtr(16),
				Threads:   sutil.GetPtr(32),
				Count:     sutil.GetPtr(2),
			},
			{
				Type:     cdbm.MachineCapabilityTypeMemory,
				Name:     "Corsair Vengeance LPX",
				Capacity: sutil.GetPtr("32GB"),
				Count:    sutil.GetPtr(4),
			},
		},
	}

	itcrInValidMachineCapabilitiesInactiveDevices := &model.APIInstanceTypeCreateRequest{
		Name:        "x3.large",
		Description: sutil.GetPtr("Test Description"),
		SiteID:      st.ID.String(),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:            cdbm.MachineCapabilityTypeMemory,
				Name:            "Corsair Vengeance LPX",
				Capacity:        sutil.GetPtr("32GB"),
				Count:           sutil.GetPtr(4),
				InactiveDevices: []int{1, 3},
			},
		},
	}

	itcrValidGPUNVLink := &model.APIInstanceTypeCreateRequest{
		Name:        "gpu-nvlink.large",
		Description: sutil.GetPtr("Instance type with GPU NVLink capability"),
		SiteID:      st.ID.String(),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:       cdbm.MachineCapabilityTypeGPU,
				Name:       "NVIDIA GB200",
				Capacity:   sutil.GetPtr("189471 MiB"),
				Frequency:  sutil.GetPtr("2062 MHz"),
				DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
				Count:      sutil.GetPtr(4),
			},
		},
	}

	itcrInvalidGPUDeviceType := &model.APIInstanceTypeCreateRequest{
		Name:        "gpu-bad-device.large",
		Description: sutil.GetPtr("Instance type with unsupported GPU device type"),
		SiteID:      st.ID.String(),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:       cdbm.MachineCapabilityTypeGPU,
				Name:       "NVIDIA GB200",
				DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
				Count:      sutil.GetPtr(4),
			},
		},
	}

	itcrInvalidNetworkDeviceType := &model.APIInstanceTypeCreateRequest{
		Name:        "network-bad-device.large",
		Description: sutil.GetPtr("Instance type with unsupported Network device type"),
		SiteID:      st.ID.String(),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:       cdbm.MachineCapabilityTypeNetwork,
				Name:       "MT43244 BlueField-3 integrated ConnectX-7 network controller",
				DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
				Count:      sutil.GetPtr(2),
			},
		},
	}

	itcrInvalidCPUDeviceType := &model.APIInstanceTypeCreateRequest{
		Name:        "cpu-with-device-type.large",
		Description: sutil.GetPtr("Instance type with device type on unsupported capability"),
		SiteID:      st.ID.String(),
		MachineCapabilities: []model.APIMachineCapability{
			{
				Type:       cdbm.MachineCapabilityTypeCPU,
				Name:       "Intel Xeon Gold 6354",
				DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
				Count:      sutil.GetPtr(2),
			},
		},
	}

	itcrValidWithoutMachineCapabilities := &model.APIInstanceTypeCreateRequest{
		Name:        "x2.large.missing.mc",
		Description: sutil.GetPtr("Test Description"),
		SiteID:      st.ID.String(),
	}

	itcrInvalid1 := &model.APIInstanceTypeCreateRequest{
		Name: "x2.large",
	}

	itcrInvalid2 := &model.APIInstanceTypeCreateRequest{
		Name:   "x2.large",
		SiteID: uuid.New().String(),
	}

	common.TestBuildInstanceType(t, dbSession, "test-it-name-1", nil, st, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	common.TestBuildInstanceType(t, dbSession, "test-it-name-2", nil, st, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)

	itcrDupicateName := &model.APIInstanceTypeCreateRequest{
		Name:                  "test-it-name-1",
		Description:           sutil.GetPtr("Test Description"),
		SiteID:                st.ID.String(),
		ControllerMachineType: sutil.GetPtr("intel_xeon_e5_2650v2"),
	}

	cfg := common.GetTestConfig()

	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceType", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceType", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo denied mocking
	//
	scpWithNICoDenied := sc.NewClientPool(tcfg)
	tscWithNICoDenied := &tmocks.Client{}

	scpWithNICoDenied.IDClientMap[st.ID.String()] = tscWithNICoDenied

	wrunWithNICoDenied := &tmocks.WorkflowRun{}
	wrunWithNICoDenied.On("GetID").Return("workflow-WithNICoDenied")

	wrunWithNICoDenied.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoDenied, errors.New("NICo went bananas")))

	tscWithNICoDenied.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceType", mock.Anything).Return(wrunWithNICoDenied, nil)

	tscWithNICoDenied.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// NICo is strict about changes in capabilities and allowing
	// instance type updates, so we sort before sending the data nico.
	// For the tests to pass, we'll need to sort the caps in the request
	// and the response.
	sort_caps := func(a, b model.APIMachineCapability) int {
		return strings.Compare(a.Name, b.Name)
	}

	tests := []struct {
		name                        string
		fields                      fields
		args                        args
		wantErr                     bool
		errMsg                      string
		respCode                    int
		expectedResourcesCount      int
		expectedMachineCapabilities int
		verifyChildSpanner          bool
	}{

		{
			name: "test instance type create API endpoint fail, workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceTypeCreateRequest{
					Name:        "x9001.large",
					Description: sutil.GetPtr("Test Description"),
					SiteID:      st.ID.String(),
					Labels: map[string]string{
						"name":        "x9001-instance-type",
						"description": "Test x9001 Instance Type ",
					},
					ControllerMachineType: sutil.GetPtr("intel_goku_e9001_dbzv2"),
					MachineCapabilities:   []model.APIMachineCapability{},
				},
			},
			wantErr:                false,
			expectedResourcesCount: 1,
			respCode:               http.StatusInternalServerError,
		},
		{
			name: "test instance type create API endpoint, nico missing endpoint/denied",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoDenied,
				scp:       scpWithNICoDenied,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceTypeCreateRequest{
					Name:                  "x9001.large",
					Description:           sutil.GetPtr("Test Description"),
					SiteID:                st.ID.String(),
					ControllerMachineType: sutil.GetPtr("intel_goku_e9001_dbzv2"),
					MachineCapabilities:   []model.APIMachineCapability{},
				},
			},
			wantErr:                false,
			expectedResourcesCount: 1,
			respCode:               http.StatusCreated,
		},
		{
			name: "test create Instance Type API endpoint with valid data",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrValid,
			},
			wantErr:                     false,
			respCode:                    http.StatusCreated,
			expectedResourcesCount:      4,
			expectedMachineCapabilities: 4,
			verifyChildSpanner:          true,
		},
		{
			name: "test create Instance Type API endpoint with valid data but no machine capabilities",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrValidWithoutMachineCapabilities,
			},
			wantErr:                     false,
			respCode:                    http.StatusCreated,
			expectedResourcesCount:      1,
			expectedMachineCapabilities: 0,
		},
		{
			name: "error create Instance Type API endpoint with name clash",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrDupicateName,
			},
			wantErr:  false,
			respCode: http.StatusConflict,
			errMsg:   fmt.Sprintf("Instance Type with name: %s for Site: %s already exists", itcrDupicateName.Name, itcrDupicateName.SiteID),
		},
		{
			name: "test create Instance Type API endpoint with Site missing in request data",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInvalid1,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
		},
		{
			name: "test create Instance Type API endpoint with invalid Site ID in request data",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInvalid2,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
		},
		{
			name: "test create Instance Type API endpoint invalid machine capabilities, name missing in request data",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInValidMachineCapabilitiesName,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
		},
		{
			name: "test create Instance Type API endpoint with invalid machine capabilities, inactive devices specified for non-InfiniBand capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInValidMachineCapabilitiesInactiveDevices,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
		},
		{
			name: "test create Instance Type API endpoint with valid GPU NVLink device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrValidGPUNVLink,
			},
			wantErr:                     false,
			respCode:                    http.StatusCreated,
			expectedResourcesCount:      2,
			expectedMachineCapabilities: 1,
		},
		{
			name: "test create Instance Type API endpoint with unsupported GPU device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInvalidGPUDeviceType,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
			errMsg:   "Unsupported Device Type specified for GPU Capability",
		},
		{
			name: "test create Instance Type API endpoint with unsupported Network device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInvalidNetworkDeviceType,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
			errMsg:   "Unsupported Device Type specified for Network Capability",
		},
		{
			name: "test create Instance Type API endpoint with device type on unsupported capability type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: itcrInvalidCPUDeviceType,
			},
			wantErr:  false,
			respCode: http.StatusBadRequest,
			errMsg:   "Unsupported Device Type: DPU specified for Capability type CPU",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cith := CreateInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			reqDataJSON, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(reqDataJSON)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(ip.Org)
			ec.Set("user", ipu)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := cith.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("CreateInstanceTypeHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if rec.Code != tt.respCode {
				t.Errorf("CreateInstanceTypeHandler.Handle() response %v", rec.Body.String())
			}

			require.Equal(t, tt.respCode, rec.Code)

			if tt.errMsg != "" {
				assert.Contains(t, rec.Body.String(), tt.errMsg)
			}

			if tt.respCode != http.StatusCreated {
				return
			}

			rst := &model.APIInstanceType{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.args.reqData.Name, rst.Name)
			require.NotNil(t, rst.Description)
			assert.Equal(t, *tt.args.reqData.Description, *rst.Description)
			assert.Equal(t, tt.args.reqData.SiteID, rst.SiteID)

			if tt.args.reqData.Labels != nil {
				assert.Equal(t, tt.args.reqData.Labels, rst.Labels)
			} else {
				assert.Equal(t, map[string]string{}, rst.Labels)
			}
			assert.Equal(t, cdbm.InstanceTypeStatusReady, rst.Status)
			assert.Equal(t, len(rst.StatusHistory), 1)
			assert.Equal(t, cdbm.InstanceTypeStatusReady, rst.StatusHistory[0].Status)
			assert.Equal(t, tt.expectedMachineCapabilities, len(rst.MachineCapabilities))

			slices.SortFunc(tt.args.reqData.MachineCapabilities, sort_caps)
			slices.SortFunc(rst.MachineCapabilities, sort_caps)

			for index, rmc := range rst.MachineCapabilities {
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Name, rmc.Name)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Frequency, rmc.Frequency)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Capacity, rmc.Capacity)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Cores, rmc.Cores)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Threads, rmc.Threads)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Vendor, rmc.Vendor)
				assert.Equal(t, tt.args.reqData.MachineCapabilities[index].Count, rmc.Count)
				if tt.args.reqData.MachineCapabilities[index].DeviceType != nil {
					assert.Equal(t, tt.args.reqData.MachineCapabilities[index].DeviceType, rmc.DeviceType)
				}

				if tt.args.reqData.MachineCapabilities[index].InactiveDevices != nil {
					assert.Equal(t, tt.args.reqData.MachineCapabilities[index].InactiveDevices, []int{1, 3})
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		org   string
		query url.Values
		user  *cdbm.User
	}

	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Steps to create test data
	// 1. Create Infrastructure Provider
	// 2. Create Site
	// 3. Create Tenant
	// 4. Create Allocation
	// 5. Create Instance Type
	// 6. Create Machine
	// 7. Create Allocation Constraint
	// 8. Create Operating System
	// 9. Create VPC
	// 10. Create Instance
	// 11. Create Tenant Site Accociation
	ipOrg := "test-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id-123", ipOrg, ipRoles)
	ipuv := common.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipViewerRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	st2 := common.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)

	// Create a second infrastructure provider with an instance type just to make sure
	// we only return types for the provider making the request.
	ipOrg2 := "test-org"
	ipu2 := common.TestBuildUser(t, dbSession, "test-starfleet-id-456", ipOrg, ipRoles)
	ip2 := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider 2", ipOrg2, ipu2)
	st3 := common.TestBuildSite(t, dbSession, ip2, "Test Site 3", ipu2)
	ip2it := common.TestBuildInstanceType(t, dbSession, fmt.Sprintf("test-instance-type-ip2"), sutil.GetPtr(uuid.New()), st3, map[string]string{
		"name":        "test-instance-type-ip2",
		"description": "Test Instance Type IP2 Description",
	}, ipu2)
	common.TestBuildMachineCapability(t, dbSession, nil, &ip2it.ID, cdbm.MachineCapabilityTypeCPU, "Intel Xeon E5-2650v2", sutil.GetPtr("3.0Hz"), nil, nil, sutil.GetPtr(2), nil, nil)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := common.TestBuildTenant(t, dbSession, "Test Tenant 1", tnOrg1, tnu1)

	tnu2 := common.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "Test Tenant 2", tnOrg2, tnu2)

	common.TestBuildTenantSite(t, dbSession, tn1, st, ipu)
	common.TestBuildTenantSite(t, dbSession, tn2, st, ipu)

	totalCount := 25

	its := []cdbm.InstanceType{}
	tmpSite := st
	for i := 0; i < totalCount*2; i++ {
		if i >= totalCount {
			tmpSite = st2
		}
		it := common.TestBuildInstanceType(t, dbSession, fmt.Sprintf("test-instance-type-%02d", i), sutil.GetPtr(uuid.New()), tmpSite, map[string]string{
			"name":        fmt.Sprintf("test-instance-type-%02d", i),
			"description": fmt.Sprintf("Test Instance Type %02d Description", i),
		}, ipu)
		common.TestBuildMachineCapability(t, dbSession, nil, &it.ID, cdbm.MachineCapabilityTypeCPU, "Intel Xeon E5-2650v2", sutil.GetPtr("3.0Hz"), nil, nil, sutil.GetPtr(2), sutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
		its = append(its, *it)
	}

	ms := []cdbm.Machine{}
	tmpSite = st
	for i := 0; i < totalCount; i++ {
		m := common.TestBuildMachine(t, dbSession, ip, st, &its[0].ID, sutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
		common.TestBuildMachineInstanceType(t, dbSession, m, &its[0])
		ms = append(ms, *m)
	}

	// build allocation constraints
	al1 := common.TestBuildAllocation(t, dbSession, st, tn1, "Test Allocation 1", ipu)
	alc1 := common.TestBuildAllocationConstraint(t, dbSession, al1, &its[0], nil, 5, ipu)

	al2 := common.TestBuildAllocation(t, dbSession, st, tn1, "Test Allocation 2", ipu)
	alc2 := common.TestBuildAllocationConstraint(t, dbSession, al2, &its[0], nil, 3, ipu)

	al3 := common.TestBuildAllocation(t, dbSession, st, tn2, "Test Allocation 3", ipu)
	alc3 := common.TestBuildAllocationConstraint(t, dbSession, al3, &its[0], nil, 7, ipu)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st, sutil.GetPtr(uuid.New()), nil, sutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)

	// Build Instance
	tn1inss := []cdbm.Instance{}
	ins1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st.ID, &its[0].ID, vpc1.ID, sutil.GetPtr(ms[0].ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	tn1inss = append(tn1inss, *ins1)
	ins2 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st.ID, &its[0].ID, vpc1.ID, sutil.GetPtr(ms[0].ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	tn1inss = append(tn1inss, *ins2)

	// This instance is not associated with an instance type
	// But has tenant reference
	// Case of Targeted Instance
	targetedInstance := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st.ID, nil, vpc1.ID, sutil.GetPtr(ms[0].ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, targetedInstance)

	os2 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn2, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn2, st, sutil.GetPtr(uuid.New()), nil, sutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu2)

	tn2inss := []cdbm.Instance{}
	ins3 := testInstanceBuildInstance(t, dbSession, "test-instance-3", tn2.ID, ip.ID, st.ID, &its[0].ID, vpc2.ID, sutil.GetPtr(ms[0].ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	tn2inss = append(tn2inss, *ins3)

	// Org with both Provider and Tenant roles: same org acts as its own infrastructure provider and tenant
	orgName := "test-provider-and-tenant-org"
	orgRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}
	orgUser := common.TestBuildUser(t, dbSession, uuid.NewString(), orgName, orgRoles)
	orgProvider := common.TestBuildInfrastructureProvider(t, dbSession, "Test Org Provider", orgName, orgUser)
	orgSite := common.TestBuildSite(t, dbSession, orgProvider, "Test Org Site", orgUser)
	orgTenant := common.TestBuildTenant(t, dbSession, "test-org-tenant", orgName, orgUser)
	common.TestBuildTenantSite(t, dbSession, orgTenant, orgSite, orgUser)

	// Build 4 ITs owned by orgProvider: 1 allocated to orgTenant, 3 unallocated.
	// orgProvider and orgTenant belong to the same org, so the user has both roles.
	orgITCount := 4
	var orgAllocatedIT *cdbm.InstanceType
	for i := 0; i < orgITCount; i++ {
		it := common.TestBuildInstanceType(t, dbSession, fmt.Sprintf("test-org-instance-type-%02d", i), sutil.GetPtr(uuid.New()), orgSite, map[string]string{
			"name":        fmt.Sprintf("test-org-instance-type-%02d", i),
			"description": fmt.Sprintf("Test Org Instance Type %02d", i),
		}, orgUser)
		common.TestBuildMachineCapability(t, dbSession, nil, &it.ID, cdbm.MachineCapabilityTypeCPU, "Intel Xeon E5-2650v2", sutil.GetPtr("3.0Hz"), nil, nil, sutil.GetPtr(2), sutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
		if i == 0 {
			orgAllocatedIT = it
		}
	}
	// Create 2 machines for the allocated IT so the allocation constraint is satisfiable
	orgM1 := common.TestBuildMachine(t, dbSession, orgProvider, orgSite, &orgAllocatedIT.ID, sutil.GetPtr("test-org-machine"), cdbm.MachineStatusReady)
	orgM2 := common.TestBuildMachine(t, dbSession, orgProvider, orgSite, &orgAllocatedIT.ID, sutil.GetPtr("test-org-machine"), cdbm.MachineStatusReady)
	common.TestBuildMachineInstanceType(t, dbSession, orgM1, orgAllocatedIT)
	common.TestBuildMachineInstanceType(t, dbSession, orgM2, orgAllocatedIT)
	orgAlloc := common.TestBuildAllocation(t, dbSession, orgSite, orgTenant, "test-org-allocation", orgUser)
	common.TestBuildAllocationConstraint(t, dbSession, orgAlloc, orgAllocatedIT, nil, 1, orgUser)

	// Create a site owned by a DIFFERENT provider (ip) but give orgTenant a TenantSite there.
	// This exercises the dual-role bug fix: provider perspective should be skipped (site not owned
	// by orgProvider), but tenant perspective should succeed.
	externalSite := common.TestBuildSite(t, dbSession, ip, "External Site For Dual-Role Tenant", ipu)
	common.TestBuildTenantSite(t, dbSession, orgTenant, externalSite, orgUser)
	externalSiteITCount := 3
	for i := 0; i < externalSiteITCount; i++ {
		it := common.TestBuildInstanceType(t, dbSession, fmt.Sprintf("test-ext-site-it-%02d", i), sutil.GetPtr(uuid.New()), externalSite, map[string]string{
			"name": fmt.Sprintf("test-ext-site-it-%02d", i),
		}, ipu)
		common.TestBuildMachineCapability(t, dbSession, nil, &it.ID, cdbm.MachineCapabilityTypeCPU, "Intel Xeon E5-2650v2", sutil.GetPtr("3.0Hz"), nil, nil, sutil.GetPtr(2), sutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	}

	// Create a second site owned by orgProvider but where orgTenant does NOT have a TenantSite.
	// This exercises the reverse dual-role scenario: tenant perspective should be skipped,
	// but provider perspective should succeed.
	orgSiteNoTenant := common.TestBuildSite(t, dbSession, orgProvider, "Org Site Without Tenant", orgUser)
	orgSiteNoTenantITCount := 2
	for i := 0; i < orgSiteNoTenantITCount; i++ {
		it := common.TestBuildInstanceType(t, dbSession, fmt.Sprintf("test-org-no-tenant-it-%02d", i), sutil.GetPtr(uuid.New()), orgSiteNoTenant, map[string]string{
			"name": fmt.Sprintf("test-org-no-tenant-it-%02d", i),
		}, orgUser)
		common.TestBuildMachineCapability(t, dbSession, nil, &it.ID, cdbm.MachineCapabilityTypeCPU, "Intel Xeon E5-2650v2", sutil.GetPtr("3.0Hz"), nil, nil, sutil.GetPtr(2), sutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	}

	e := echo.New()

	cfg := common.GetTestConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		fields                            fields
		args                              args
		wantCount                         int
		wantTotalCount                    int
		wantFirstEntry                    *cdbm.InstanceType
		wantRespCode                      int
		wantErr                           bool
		includeMachineAssignment          bool
		includeAllocationStats            bool
		wantTotalAllocationStatsCount     int
		wantUsedAllocationStatsCount      int
		wantMaxAllocatableStatsCount      *int
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
	}{
		{
			name: "get all Instance Type for Provider admin success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
				},
				user: ipu,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type for Provider viewer success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
				},
				user: ipuv,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type for Provider viewer without siteID success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
				},
				user: ipu,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: 2*totalCount + externalSiteITCount, // st+st2 ITs plus external site ITs for same provider ip
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type for Tenant viewer without siteID success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  tnOrg1,
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			// Org has both Provider and Tenant roles. Every instance type
			// belongs to a site owned by orgProvider and also accessible by orgTenant via TenantSite,
			// so each instance type appears in both the provider and tenant DB queries.
			// The mapset must deduplicate them so the response contains exactly orgITCount
			// unique instance types, not 2×orgITCount.
			name: "get all Instance Types for org with both provider and tenant roles (results deduplicated)",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org:  orgName,
				user: orgUser,
			},
			// Provider path: orgSite (orgITCount) + orgSiteNoTenant (orgSiteNoTenantITCount). Tenant path: orgSite + externalSite. Union after dedup.
			wantCount:      orgITCount + orgSiteNoTenantITCount + externalSiteITCount,
			wantTotalCount: orgITCount + orgSiteNoTenantITCount + externalSiteITCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			// excludeUnallocated applies only to the tenant query, never the provider query.
			// A user with both provider and tenant roles always sees their full owned inventory as a provider (including
			// unallocated ITs), while the tenant query is filtered to only return ITs with an
			// active allocation. For ITs the provider owns, excludeUnallocated has no effect —
			// they appear from the provider query regardless.
			//
			// Setup: orgITCount=4 ITs total on orgSite, 1 has an AllocationConstraint for
			// orgTenant (orgAllocatedIT), 3 do not.
			//
			// Expected: provider query contributes orgSite + orgSiteNoTenant ITs; tenant query with excludeUnallocated
			// only adds the allocated IT (subset of orgSite). Merge union equals provider-side count.
			name: "get all Instance Types for org with both provider and tenant roles with excludeUnallocated (provider query unaffected)",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: orgName,
				query: url.Values{
					"excludeUnallocated": []string{"true"},
				},
				user: orgUser,
			},
			wantCount:      orgITCount + orgSiteNoTenantITCount,
			wantTotalCount: orgITCount + orgSiteNoTenantITCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			// Dual-role user queries with siteId pointing to a site owned by a DIFFERENT provider.
			// The provider perspective should be silently skipped (site not owned by orgProvider),
			// while the tenant perspective succeeds via TenantSite association.
			// Before the fix this returned 403 because the provider block's hard return killed the handler.
			name: "get all Instance Types for dual-role org with siteId owned by different provider (tenant perspective succeeds)",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: orgName,
				query: url.Values{
					"siteId": []string{externalSite.ID.String()},
				},
				user: orgUser,
			},
			wantCount:      externalSiteITCount,
			wantTotalCount: externalSiteITCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			// Dual-role user queries with siteId pointing to a site owned by orgProvider,
			// but where orgTenant does NOT have a TenantSite association.
			// The tenant perspective should be silently skipped, while the provider
			// perspective succeeds and returns instance types.
			// Before the fix this returned 403 because the tenant block's hard return discarded provider results.
			name: "get all Instance Types for dual-role org with siteId where tenant has no TenantSite (provider perspective succeeds)",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: orgName,
				query: url.Values{
					"siteId": []string{orgSiteNoTenant.ID.String()},
				},
				user: orgUser,
			},
			wantCount:      orgSiteNoTenantITCount,
			wantTotalCount: orgSiteNoTenantITCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			// Dual-role user queries with siteId pointing to a site that neither their
			// provider owns nor their tenant has a TenantSite association with.
			// Both perspectives should be skipped, resulting in an empty result set (200 OK, 0 items).
			name: "get all Instance Types for dual-role org with siteId accessible by neither perspective (empty result)",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: orgName,
				query: url.Values{
					"siteId": []string{st3.ID.String()},
				},
				user: orgUser,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type for Tenant success only with InstanceType association with Instance",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
				},
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all instance types for Provider with pagination success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"pageNumber":               []string{"2"},
					"pageSize":                 []string{"5"},
					"orderBy":                  []string{"NAME_ASC"},
				},
				user: ipu,
			},
			wantCount:          5,
			wantTotalCount:     totalCount,
			wantFirstEntry:     &its[5],
			wantRespCode:       http.StatusOK,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "get all Instance Types by Provider with pagination failure, invalid page size",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"pageNumber":               []string{"1"},
					"pageSize":                 []string{"200"},
					"orderBy":                  []string{"NAME_ASC"},
				},
				user: ipu,
			},
			wantRespCode: http.StatusBadRequest,
			wantErr:      false,
		},
		{
			name: "get all Instance Type including machine association success with includeMachineAssignment",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"includeMachineAssignment": []string{"True"},
				},
				user: ipu,
			},
			wantCount:                cdbp.DefaultLimit,
			wantTotalCount:           totalCount,
			wantRespCode:             http.StatusOK,
			wantFirstEntry:           &its[0],
			wantErr:                  false,
			includeMachineAssignment: true,
		},
		{
			name: "get all Instance Type including machine association failure, requested by Tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId":                 []string{tn1.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"includeMachineAssignment": []string{"True"},
				},
				user: tnu1,
			},
			wantRespCode: http.StatusForbidden,
			wantErr:      false,
		},
		{
			name: "get all Instance Type for Infrastructure Provider and Site include relation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"includeRelation":          []string{cdbm.InfrastructureProviderRelationName, cdbm.SiteRelationName},
				},
				user: ipu,
			},
			wantCount:                         cdbp.DefaultLimit,
			wantTotalCount:                    totalCount,
			wantRespCode:                      http.StatusOK,
			wantErr:                           false,
			expectedInfrastructureProviderOrg: sutil.GetPtr(ip.Org),
			expectedSiteName:                  sutil.GetPtr(st.Name),
		},
		{
			name: "get all Instance Type for Tenant success with allocation stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId":               []string{tn1.ID.String()},
					"siteId":                 []string{st.ID.String()},
					"includeAllocationStats": []string{"True"},
				},
				user: tnu1,
			},
			wantCount:                     cdbp.DefaultLimit,
			wantTotalCount:                totalCount,
			wantRespCode:                  http.StatusOK,
			wantFirstEntry:                &its[0],
			wantErr:                       false,
			includeAllocationStats:        true,
			wantTotalAllocationStatsCount: alc1.ConstraintValue + alc2.ConstraintValue,
			wantUsedAllocationStatsCount:  len(tn1inss),
			wantMaxAllocatableStatsCount:  nil,
		},
		{
			name: "get all Instance Type for Tenant success with allocation stats, without empty allocations",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId":               []string{tn1.ID.String()},
					"siteId":                 []string{st.ID.String()},
					"includeAllocationStats": []string{"True"},
					"excludeUnallocated":     []string{"True"},
				},
				user: tnu1,
			},
			wantCount:                     1,
			wantTotalCount:                1,
			wantRespCode:                  http.StatusOK,
			wantFirstEntry:                &its[0],
			wantErr:                       false,
			includeAllocationStats:        true,
			wantTotalAllocationStatsCount: alc1.ConstraintValue + alc2.ConstraintValue,
			wantUsedAllocationStatsCount:  len(tn1inss),
			wantMaxAllocatableStatsCount:  nil,
		},
		{
			name: "get all Instance Type for Provider success with allocation stats",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"includeAllocationStats":   []string{"True"},
				},
				user: ipu,
			},
			wantCount:                     cdbp.DefaultLimit,
			wantTotalCount:                totalCount,
			wantRespCode:                  http.StatusOK,
			wantFirstEntry:                &its[0],
			wantErr:                       false,
			includeAllocationStats:        true,
			wantTotalAllocationStatsCount: alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue,
			wantUsedAllocationStatsCount:  len(tn1inss) + len(tn2inss),
			wantMaxAllocatableStatsCount:  sutil.GetPtr(totalCount - (alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue)),
		},
		{
			name: "get all Instance Type for Provider success with allocation stats, without empty allocations, fail because only tenants can excludeUnallocated",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: ipOrg,
				query: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
					"siteId":                   []string{st.ID.String()},
					"includeAllocationStats":   []string{"True"},
					"excludeUnallocated":       []string{"True"},
				},
				user: ipu,
			},
			wantCount:      1,
			wantTotalCount: 1,
			wantRespCode:   http.StatusForbidden,
			wantFirstEntry: &its[0],
			wantErr:        false,
		},
		{
			name: "get all Instance Type success with name search query",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"query":    []string{"test-instance-type"},
				},
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type success with status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"query":    []string{"pending"},
				},
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type success with unexisted status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"query":    []string{"ready"},
				},
				user: tnu1,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type success with name and status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"query":    []string{"test-instance-type ready"},
				},
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type success InstanceTypeStatusPending status",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"status":   []string{cdbm.InstanceTypeStatusPending},
				},
				user: tnu1,
			},
			wantCount:      cdbp.DefaultLimit,
			wantTotalCount: totalCount,
			wantRespCode:   http.StatusOK,
			wantErr:        false,
		},
		{
			name: "get all Instance Type success BadStatus status",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				org: tnOrg1,
				query: url.Values{
					"tenantId": []string{tn1.ID.String()},
					"siteId":   []string{st.ID.String()},
					"status":   []string{"BadStatus"},
				},
				user: tnu1,
			},
			wantCount:      0,
			wantTotalCount: 0,
			wantRespCode:   http.StatusBadRequest,
			wantErr:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gaith := GetAllInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/instance/type?%s", tt.args.org, tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gaith.Handle(ec)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantRespCode != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			require.Equal(t, tt.wantRespCode, rec.Code)

			if tt.wantRespCode != http.StatusOK {
				return
			}

			rst := []model.APIInstanceType{}

			err = json.Unmarshal(rec.Body.Bytes(), &rst)
			require.NoError(t, err)

			assert.Equal(t, tt.wantCount, len(rst))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			require.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			require.NoError(t, err)

			assert.Equal(t, tt.wantTotalCount, pr.Total)

			if tt.wantFirstEntry != nil {
				assert.Equal(t, tt.wantFirstEntry.Name, rst[0].Name)
			}

			if len(rst) > 0 {
				for _, itr := range rst {
					assert.Equal(t, len(itr.MachineCapabilities), 1)
				}

				if tt.includeMachineAssignment {
					assert.Equal(t, tt.wantTotalCount, len(rst[0].MachineInstanceTypes))
					assert.Equal(t, tt.wantFirstEntry.ID.String(), rst[0].MachineInstanceTypes[0].InstanceTypeID)
				} else {
					assert.Nil(t, rst[0].MachineInstanceTypes)
				}

				if tt.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tt.expectedInfrastructureProviderOrg, rst[0].InfrastructureProvider.Org)
				} else {
					assert.Nil(t, rst[0].InfrastructureProvider)
				}
				if tt.expectedSiteName != nil {
					assert.Equal(t, *tt.expectedSiteName, rst[0].Site.Name)
				} else {
					assert.Nil(t, rst[0].Site)
				}
				if tt.includeAllocationStats {
					assert.NotNil(t, rst[0].AllocationStats)
					if tt.wantFirstEntry != nil {
						assert.Equal(t, rst[0].AllocationStats.Total, tt.wantTotalAllocationStatsCount)
						assert.Equal(t, rst[0].AllocationStats.Used, tt.wantUsedAllocationStatsCount)

						if tt.wantMaxAllocatableStatsCount == nil {
							assert.Nil(t, rst[0].AllocationStats.MaxAllocatable)
						} else {
							assert.Equal(t, *rst[0].AllocationStats.MaxAllocatable, *tt.wantMaxAllocatableStatsCount)
						}
					}
				} else {
					assert.Nil(t, rst[0].AllocationStats)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id-123", org, ipRoles)
	ipuv := common.TestBuildUser(t, dbSession, uuid.NewString(), org, ipViewerRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)

	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	tnOrg1 := org
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := common.TestBuildUser(t, dbSession, "test-starfleet-id-456", tnOrg1, tnRoles)
	tn1 := common.TestBuildTenant(t, dbSession, "Test Tenant 1", tnOrg1, tnu1)

	tnu2 := common.TestBuildUser(t, dbSession, "test-starfleet-id-789", tnOrg2, tnRoles)
	tn2 := common.TestBuildTenant(t, dbSession, "Test Tenant 2", tnOrg2, tnu2)

	tnu3 := common.TestBuildUser(t, dbSession, "test-starfleet-id-101112", tnOrg3, tnRoles)
	tn3 := common.TestBuildTenant(t, dbSession, "Test Tenant 3", tnOrg3, tnu2)

	it := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)

	al1 := common.TestBuildAllocation(t, dbSession, st, tn1, "Test Allocation", ipu)
	alc1 := common.TestBuildAllocationConstraint(t, dbSession, al1, it, nil, 8, ipu)
	common.TestBuildTenantSite(t, dbSession, tn1, st, ipu)

	common.TestBuildAllocation(t, dbSession, st, tn3, "Test Allocation", ipu)
	common.TestBuildTenantSite(t, dbSession, tn3, st, ipu)

	// Setup echo server/context
	e := echo.New()

	// build machines
	ms := []cdbm.Machine{}
	for i := 0; i < 15; i++ {
		m := common.TestBuildMachine(t, dbSession, ip, st, &it.ID, nil, cdbm.MachineStatusReady)
		common.TestBuildMachineInstanceType(t, dbSession, m, it)
		ms = append(ms, *m)
	}

	// build vpc
	vpc1 := common.TestBuildVPC(t, dbSession, "Test-Controller-VPC", ip, tn1, st, nil, nil, nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	// build os
	os1 := common.TestBuildOperatingSystem(t, dbSession, "", tn1, cdbm.OperatingSystemStatusReady, tnu1)

	// build instance
	inst1 := common.TestBuildInstance(t, dbSession, "Test-Controller-Instance", tn1.ID, ip.ID, st.ID, it.ID, vpc1.ID, sutil.GetPtr(ms[0].ID), os1.ID)
	assert.NotNil(t, inst1)

	cfg := common.GetTestConfig()

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	type args struct {
		user           *cdbm.User
		org            string
		instanceTypeID uuid.UUID
		query          url.Values
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		fields                            fields
		args                              args
		wantRespCode                      int
		wantMachine                       *cdbm.Machine
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		includeAllocationStats            bool
		expectTotalAllocationStats        int
		expectUsedAllocationStats         int
		expectMaxAllocableAllocationStats *int
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
	}{
		{
			name: "test get Instance Type by Provider admin",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test get Instance Type by Provider viewer",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipuv,
				org:            ip.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test get Instance Type by Infrastructure Provider",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test get Instance Type by Infrastructure Provider failure, Instance Type not found",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: uuid.New(),
			},
			wantRespCode: http.StatusNotFound,
		},
		{
			name: "test get Instance Type by Tenant with Allocation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           tnu1,
				org:            tn1.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "test get Instance Type by Tenant failure, Tenant does not have Allocation",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           tnu2,
				org:            tn2.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "test get Instance Type including Machine Association success with includeMachineAssignment",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID,
				query: url.Values{
					"includeMachineAssignment": []string{"true"},
				},
			},
			wantRespCode: http.StatusOK,
			wantMachine:  &ms[0],
		},
		{
			name: "test get Instance Type including Machine Association failure, requested by Tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           tnu3,
				org:            tn3.Org,
				instanceTypeID: it.ID,
				query: url.Values{
					"includeMachineAssignment": []string{"true"},
				},
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "test get Instance Type include Infrastructure Provider and Site relation success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID,
			},
			wantRespCode:                      http.StatusOK,
			queryIncludeRelations1:            sutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            sutil.GetPtr(cdbm.SiteRelationName),
			expectedInfrastructureProviderOrg: sutil.GetPtr(ip.Org),
			expectedSiteName:                  sutil.GetPtr(st.Name),
		},
		{
			name: "test get Instance Type by Tenant with Allocation Stats success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           tnu1,
				org:            tn1.Org,
				instanceTypeID: it.ID,
				query: url.Values{
					"includeAllocationStats": []string{"true"},
				},
			},
			wantRespCode:                      http.StatusOK,
			includeAllocationStats:            true,
			expectTotalAllocationStats:        8,
			expectUsedAllocationStats:         1,
			expectMaxAllocableAllocationStats: nil,
		},
		{
			name: "test get Instance Type Provider with Allocation Stats success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID,
				query: url.Values{
					"includeAllocationStats": []string{"true"},
				},
			},
			wantRespCode:                      http.StatusOK,
			includeAllocationStats:            true,
			expectTotalAllocationStats:        alc1.ConstraintValue,
			expectUsedAllocationStats:         1,
			expectMaxAllocableAllocationStats: sutil.GetPtr(len(ms) - alc1.ConstraintValue),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gith := GetInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/instance/type/%s?%s", tt.args.org, tt.args.instanceTypeID.String(), tt.args.query.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			q := req.URL.Query()
			if tt.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations1)
			}
			if tt.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tt.queryIncludeRelations2)
			}

			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.instanceTypeID.String())
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gith.Handle(ec)
			assert.NoError(t, err)

			if tt.wantRespCode != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			require.Equal(t, tt.wantRespCode, rec.Code)

			if rec.Code != http.StatusOK {
				return
			}

			rst := &model.APIInstanceType{}

			err = json.Unmarshal(rec.Body.Bytes(), rst)
			require.NoError(t, err)

			assert.Equal(t, rst.ID, it.ID.String())
			assert.Equal(t, rst.Name, it.Name)
			require.NotNil(t, rst.Description)
			assert.Equal(t, *rst.Description, *it.Description)

			if tt.wantMachine != nil {
				assert.Equal(t, len(ms), len(rst.MachineInstanceTypes))
				assert.Equal(t, tt.wantMachine.ID, rst.MachineInstanceTypes[0].MachineID)
			} else {
				assert.Nil(t, rst.MachineInstanceTypes)
			}

			if tt.queryIncludeRelations1 != nil || tt.queryIncludeRelations2 != nil {
				if tt.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tt.expectedInfrastructureProviderOrg, rst.InfrastructureProvider.Org)
				}
				if tt.expectedSiteName != nil {
					assert.Equal(t, *tt.expectedSiteName, rst.Site.Name)
				}
			} else {
				assert.Nil(t, rst.InfrastructureProvider)
				assert.Nil(t, rst.Site)
			}

			if tt.includeAllocationStats {
				assert.NotNil(t, rst.AllocationStats)
				assert.Equal(t, tt.expectTotalAllocationStats, rst.AllocationStats.Total)
				assert.Equal(t, tt.expectUsedAllocationStats, rst.AllocationStats.Used)

				if tt.expectMaxAllocableAllocationStats != nil {
					assert.Equal(t, tt.expectMaxAllocableAllocationStats, rst.AllocationStats.MaxAllocatable)
				} else {
					assert.Nil(t, rst.AllocationStats.MaxAllocatable)
				}
			} else {
				assert.Nil(t, rst.AllocationStats)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestUpdateInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, orgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)

	it2 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-2", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)
	assert.NotNil(t, it2)
	cfg := common.GetTestConfig()

	e := echo.New()

	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInstanceType", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInstanceType", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo denied mocking
	//
	scpWithNICoDenied := sc.NewClientPool(tcfg)
	tscWithNICoDenied := &tmocks.Client{}

	scpWithNICoDenied.IDClientMap[st.ID.String()] = tscWithNICoDenied

	wrunWithNICoDenied := &tmocks.WorkflowRun{}
	wrunWithNICoDenied.On("GetID").Return("workflow-WithNICoDenied")

	wrunWithNICoDenied.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoDenied, errors.New("NICo went bananas")))

	tscWithNICoDenied.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInstanceType", mock.Anything).Return(wrunWithNICoDenied, nil)

	tscWithNICoDenied.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	type args struct {
		user           *cdbm.User
		org            string
		instanceTypeID uuid.UUID
		reqData        *model.APIInstanceTypeUpdateRequest
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantRespCode       int
		errMsg             string
		verifyChildSpanner bool
	}{
		{
			name: "test instance type update API endpoint fail, workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name:        sutil.GetPtr("x2.large"),
					Description: sutil.GetPtr("Test Site Description"),
				},
			},
			wantRespCode: http.StatusInternalServerError,
		},
		{
			name: "test Instance Type update nico unavailable/denied, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoDenied,
				scp:       scpWithNICoDenied,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name:        sutil.GetPtr("x2.large"),
					Description: sutil.GetPtr("Test Site Description"),
					Labels:      map[string]string{"updated-test-key": "updated-test-value"},
				},
			},
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance Type update success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name:        sutil.GetPtr("x2.large"),
					Description: sutil.GetPtr("Test Site Description"),
				},
			},
			wantRespCode:       http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance Type update success with same name",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name: sutil.GetPtr("test-instance-type-2"),
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update error when name clashes",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name:        sutil.GetPtr("test-instance-type-2"),
					Description: sutil.GetPtr("Test Site Description"),
				},
			},
			wantRespCode: http.StatusConflict,
		},
		{
			name: "test Instance Type update failure, invalid Instance Type ID",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: uuid.New(),
				reqData: &model.APIInstanceTypeUpdateRequest{
					Name: sutil.GetPtr("x2.large"),
				},
			},
			wantRespCode: http.StatusNotFound,
		},
		{
			name: "test Instance Type update success with same display name",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData:        &model.APIInstanceTypeUpdateRequest{},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update fail with new capability of a bad type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{Type: "BAD TYPE", Name: "could be anything"},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "test Instance Type update fail with new capability of unsupported device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{Type: cdbm.MachineCapabilityTypeNetwork, DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceType("ETHERNET"))},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "test Instance Type update fail with duplicate capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type: "CPU",
							Name: "Intel Kryptonite",
						},
						{
							Type: "CPU",
							Name: "Intel Kryptonite",
						},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "test Instance Type update success with new capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type: "CPU",
							Name: "Intel Kryptonite",
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update success with reposition existing capability and new capabilities",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type: "GPU",
							Name: "NVDIA JeeBee200",
						},
						{
							Type: "Memory",
							Name: "DDR4",
						},
						{
							Type: "CPU",
							Name: "Intel Kryptonite",
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update success with new device type for network capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       "Network",
							Name:       "MT43244 BlueField-3 integrated ConnectX-7 network controller",
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
							Count:      sutil.GetPtr(2),
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update success with GPU NVLink device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       cdbm.MachineCapabilityTypeGPU,
							Name:       "NVIDIA GB200",
							Capacity:   sutil.GetPtr("189471 MiB"),
							Frequency:  sutil.GetPtr("2062 MHz"),
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
							Count:      sutil.GetPtr(4),
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update fail with unsupported GPU device type DPU",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       cdbm.MachineCapabilityTypeGPU,
							Name:       "NVIDIA GB200",
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
							Count:      sutil.GetPtr(4),
						},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
			errMsg:       "Unsupported Device Type specified for GPU Capability",
		},
		{
			name: "test Instance Type update fail with NVLink device type on Network capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       cdbm.MachineCapabilityTypeNetwork,
							Name:       "MT43244 BlueField-3 integrated ConnectX-7 network controller",
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
							Count:      sutil.GetPtr(2),
						},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
			errMsg:       "Unsupported Device Type specified for Network Capability",
		},
		{
			name: "test Instance Type update fail with device type on CPU capability",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       cdbm.MachineCapabilityTypeCPU,
							Name:       "Intel Xeon Gold 6354",
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
							Count:      sutil.GetPtr(2),
						},
					},
				},
			},
			wantRespCode: http.StatusBadRequest,
			errMsg:       "Unsupported Device Type: DPU specified for Capability type CPU",
		},
		{
			name: "test Instance Type update success with mixed capabilities including valid device types",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:       cdbm.MachineCapabilityTypeNetwork,
							Name:       "MT43244 BlueField-3 integrated ConnectX-7 network controller",
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU),
							Count:      sutil.GetPtr(2),
						},
						{
							Type:       cdbm.MachineCapabilityTypeGPU,
							Name:       "NVIDIA GB200",
							Capacity:   sutil.GetPtr("189471 MiB"),
							DeviceType: sutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink),
							Count:      sutil.GetPtr(4),
						},
						{
							Type:  cdbm.MachineCapabilityTypeCPU,
							Name:  "Intel Xeon Gold 6354",
							Count: sutil.GetPtr(2),
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
		{
			name: "test Instance Type update success with Network capability without device type",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            org,
				instanceTypeID: it2.ID,
				reqData: &model.APIInstanceTypeUpdateRequest{
					MachineCapabilities: []model.APIMachineCapability{
						{
							Type:  cdbm.MachineCapabilityTypeNetwork,
							Name:  "MT43244 BlueField-3 integrated ConnectX-7 network controller",
							Count: sutil.GetPtr(2),
						},
					},
				},
			},
			wantRespCode: http.StatusOK,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			uith := UpdateInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/instance/type/%s", tt.args.org, tt.args.instanceTypeID.String())

			reqJSON, err := json.Marshal(tt.args.reqData)
			require.NoError(t, err)

			req := httptest.NewRequest(http.MethodGet, path, strings.NewReader(string(reqJSON)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.instanceTypeID.String())
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err = uith.Handle(ec)
			assert.NoError(t, err)

			require.Equal(t, tt.wantRespCode, rec.Code)

			if tt.errMsg != "" {
				assert.Contains(t, rec.Body.String(), tt.errMsg)
			}

			if rec.Code != http.StatusOK {
				return
			}

			require.Equal(t, http.StatusOK, rec.Code)

			rst := &model.APIInstanceType{}

			err = json.Unmarshal(rec.Body.Bytes(), rst)
			require.NoError(t, err)

			if tt.args.reqData.Name != nil {
				assert.Equal(t, *tt.args.reqData.Name, rst.Name)
			}

			if tt.args.reqData.MachineCapabilities != nil {
				assert.Equal(t, len(tt.args.reqData.MachineCapabilities), len(rst.MachineCapabilities), "length of capabilities in response do not match the request")

				for i := range tt.args.reqData.MachineCapabilities {
					assert.Equal(t, tt.args.reqData.MachineCapabilities[i], rst.MachineCapabilities[i], "capability mismatch between request and response")
				}
			}

			if tt.args.reqData.Description != nil {
				assert.Equal(t, *rst.Description, *tt.args.reqData.Description)
			}

			if tt.args.reqData.Labels != nil {
				assert.Equal(t, tt.args.reqData.Labels, rst.Labels)
			}

			assert.NotEqual(t, rst.Updated.String(), it.Updated.String())

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDeleteInstanceTypeHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	org := "test-provider-org"
	orgRoles := []string{authz.ProviderAdminRole}

	ipu := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, orgRoles)
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, ipu)
	st := common.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	it := common.TestBuildInstanceType(t, dbSession, "test-instance-type-1", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	it2 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-2", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)
	it3 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-3", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-3",
		"description": "Test Instance Type 3 Description",
	}, ipu)
	it4 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-4", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-4",
		"description": "Test Instance Type 4 Description",
	}, ipu)
	it5 := common.TestBuildInstanceType(t, dbSession, "test-instance-type-5", sutil.GetPtr(uuid.New()), st, map[string]string{
		"name":        "test-instance-type-5",
		"description": "Test Instance Type 5 Description",
	}, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := common.TestBuildUser(t, dbSession, "test-starfleet-id-456", tnOrg, tnRoles)
	tn := common.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	al := common.TestBuildAllocation(t, dbSession, st, tn, "Test Allocation", ipu)
	common.TestBuildAllocationConstraint(t, dbSession, al, it2, nil, 10, ipu)

	m := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, sutil.GetPtr(it.ID), sutil.GetPtr("mcType"), true, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m)

	m1 := testMachineBuildMachine(t, dbSession, ip.ID, st.ID, sutil.GetPtr(it2.ID), sutil.GetPtr("mcType"), true, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m1)

	// Setup echo server/context
	e := echo.New()

	cfg := common.GetTestConfig()

	tc := &tmocks.Client{}
	tcfg, _ := cfg.GetTemporalConfig()

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceType", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[st.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceType", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo denied mocking
	//
	scpWithNICoDenied := sc.NewClientPool(tcfg)
	tscWithNICoDenied := &tmocks.Client{}

	scpWithNICoDenied.IDClientMap[st.ID.String()] = tscWithNICoDenied

	wrunWithNICoDenied := &tmocks.WorkflowRun{}
	wrunWithNICoDenied.On("GetID").Return("workflow-WithNICoDenied")

	wrunWithNICoDenied.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoDenied, errors.New("NICo went bananas")))

	tscWithNICoDenied.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceType", mock.Anything).Return(wrunWithNICoDenied, nil)

	tscWithNICoDenied.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Prepare client pool for sync calls
	// to site(s).

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceType", mock.Anything).Return(wrun, nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	type args struct {
		user           *cdbm.User
		org            string
		instanceTypeID string
	}

	tests := []struct {
		name                  string
		fields                fields
		args                  args
		wantRespCode          int
		wantInstanceTypeCount int
		verifyMachine         bool
		verifyChildSpanner    bool
	}{
		{
			name: "test delete Instance Type success",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it.ID.String(),
			},
			wantRespCode:          http.StatusAccepted,
			wantInstanceTypeCount: 4,
			verifyMachine:         true,
			verifyChildSpanner:    true,
		},
		{
			name: "test Instance Type delete API endpoint nico not-found, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it3.ID.String(),
			},
			wantRespCode:          http.StatusAccepted,
			wantInstanceTypeCount: 3,
			verifyChildSpanner:    true,
		},
		{
			name: "test Instance Type delete API endpoint nico denied/unimplemented, still success",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoDenied,
				scp:       scpWithNICoDenied,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it5.ID.String(),
			},
			wantRespCode:          http.StatusAccepted,
			wantInstanceTypeCount: 2,
			verifyChildSpanner:    true,
		},
		{
			name: "test VPC delete API endpoint workflow timeout failure",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it4.ID.String(),
			},
			wantRespCode:       http.StatusInternalServerError,
			verifyChildSpanner: true,
		},
		{
			name: "test delete Instance Type failure, Allocation Constraint present",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: it2.ID.String(),
			},
			wantRespCode:          http.StatusBadRequest,
			wantInstanceTypeCount: 2,
		},
		{
			name: "test delete Instance Type failure, non existent ID",
			fields: fields{
				dbSession: dbSession,
				tc:        &tmocks.Client{},
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				user:           ipu,
				org:            ip.Org,
				instanceTypeID: uuid.NewString(),
			},
			wantRespCode:          http.StatusNotFound,
			wantInstanceTypeCount: 2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dith := DeleteInstanceTypeHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/instance/type/%s", tt.args.org, tt.args.instanceTypeID)

			req := httptest.NewRequest(http.MethodDelete, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.instanceTypeID)
			ec.Set("user", tt.args.user)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			err := dith.Handle(ec)
			require.NoError(t, err)

			assert.Equal(t, tt.wantRespCode, rec.Code)

			if rec.Code != http.StatusAccepted {
				return
			}

			itDAO := cdbm.NewInstanceTypeDAO(dbSession)
			its, _, terr := itDAO.GetAll(context.Background(), nil, cdbm.InstanceTypeFilterInput{InfrastructureProviderID: &ip.ID, SiteIDs: []uuid.UUID{st.ID}}, nil, nil, nil, nil)
			assert.Nil(t, terr)

			// One of the Instance Types should be deleted
			assert.Equal(t, tt.wantInstanceTypeCount, len(its))

			if tt.verifyMachine {
				mcDAO := cdbm.NewMachineDAO(dbSession)
				itID, _ := uuid.Parse(tt.args.instanceTypeID)
				mcs, _, terr := mcDAO.GetAll(context.Background(), nil, cdbm.MachineFilterInput{InstanceTypeIDs: []uuid.UUID{itID}}, cdbp.PageInput{}, nil)
				assert.Nil(t, terr)
				assert.Equal(t, 0, len(mcs))
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInstanceTypeHandlers(t *testing.T) {
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	cith := CreateInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
		scp:       scp,
	}

	if got := NewCreateInstanceTypeHandler(dbSession, tc, scp, cfg); !reflect.DeepEqual(got, cith) {
		t.Errorf("NewCreateInstanceTypeHandler() = %v, want %v", got, cith)
	}

	gaith := GetAllInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}

	if got := NewGetAllInstanceTypeHandler(dbSession, tc, cfg); !reflect.DeepEqual(got, gaith) {
		t.Errorf("NewGetAllInstanceTypeHandler() = %v, want %v", got, gaith)
	}

	gith := GetInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}

	if got := NewGetInstanceTypeHandler(dbSession, tc, cfg); !reflect.DeepEqual(got, gith) {
		t.Errorf("NewGetInstanceTypeHandler() = %v, want %v", got, gith)
	}

	uith := UpdateInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
		scp:       scp,
	}

	if got := NewUpdateInstanceTypeHandler(dbSession, tc, scp, cfg); !reflect.DeepEqual(got, uith) {
		t.Errorf("NewUpdateInstanceTypeHandler() = %v, want %v", got, uith)
	}

	dith := DeleteInstanceTypeHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
		scp:       scp,
	}

	if got := NewDeleteInstanceTypeHandler(dbSession, tc, scp, cfg); !reflect.DeepEqual(got, dith) {
		t.Errorf("NewDeleteInstanceTypeHandler() = %v, want %v", got, dith)
	}
}
