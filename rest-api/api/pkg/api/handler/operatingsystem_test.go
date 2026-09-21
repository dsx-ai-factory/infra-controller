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
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdmu "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	oteltrace "go.opentelemetry.io/otel/trace"
	"go.temporal.io/api/enums/v1"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
)

func TestOperatingSystemHandler_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	site1 := testMachineBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site1)

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2, tnOrg3}, tnRoles)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-1")
	testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-3")

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, tnu.ID)
	assert.NotNil(t, ts1)

	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant-2")
	testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant-4")

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant2.ID, site.ID, tnu.ID)
	assert.NotNil(t, ts2)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant2.ID, site1.ID, tnu.ID)
	assert.NotNil(t, ts3)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	osObj := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-1", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), IpxeScript: cutil.GetPtr("ipxe"), ImageDisk: cutil.GetPtr("/dev/sda"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), IsCloudInit: true, AllowOverride: false}
	okBody, err := json.Marshal(osObj)
	assert.Nil(t, err)

	osImageURLObj1 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-2", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), SiteIDs: []string{site.ID.String()}, ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), ImageSHA: nil}
	errImageURLBody, err := json.Marshal(osImageURLObj1)
	assert.Nil(t, err)

	osImageURLObj2 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-3", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), SiteIDs: []string{site.ID.String()}, ImageURL: cutil.GetPtr("//cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit)}
	badImageURLBody, err := json.Marshal(osImageURLObj2)
	assert.Nil(t, err)

	osImageURLObj3 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-4", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant2.ID.String()), SiteIDs: []string{site.ID.String()}, ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), ImageAuthType: cutil.GetPtr("Basic"), ImageAuthToken: cutil.GetPtr("rsa"), ImageDisk: cutil.GetPtr("/dev/nvme1n3"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), IsCloudInit: false, AllowOverride: true}
	validImageURLBody, err := json.Marshal(osImageURLObj3)
	assert.Nil(t, err)

	osImageURLObj4 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-5", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), SiteIDs: []string{site.ID.String()}, IpxeScript: cutil.GetPtr("ipxe"), ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), RootFsLabel: cutil.GetPtr("test-label"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit)}
	errIpxeImageUrlBody, err := json.Marshal(osImageURLObj4)
	assert.Nil(t, err)

	osImageURLObj5 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-6", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), SiteIDs: []string{site.ID.String()}, ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), RootFsID: cutil.GetPtr("abc123"), RootFsLabel: cutil.GetPtr("test-label"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit)}
	errRootFsBody, err := json.Marshal(osImageURLObj5)
	assert.Nil(t, err)

	osImageURLObj6 := model.APIOperatingSystemCreateRequest{Name: "test-operating-system-7", Description: cutil.GetPtr("test"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant1.ID.String()), SiteIDs: []string{site.ID.String()}, ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), RootFsID: cutil.GetPtr("abc123"), ImageDisk: cutil.GetPtr("/dev/junk"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit)}
	errDiskImageBody, err := json.Marshal(osImageURLObj6)
	assert.Nil(t, err)

	osImageURLObj7 := model.APIOperatingSystemCreateRequest{Name: "terminate-workflow-operating-system", Description: cutil.GetPtr("test termination"), InfrastructureProviderID: nil, TenantID: cutil.GetPtr(tenant2.ID.String()), SiteIDs: []string{site1.ID.String()}, ImageURL: cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), ImageAuthType: cutil.GetPtr("Basic"), ImageAuthToken: cutil.GetPtr("rsa"), ImageDisk: cutil.GetPtr("/dev/nvme1n3"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), IsCloudInit: false, AllowOverride: true}
	terminateCreationImageURLBody, err := json.Marshal(osImageURLObj7)
	assert.Nil(t, err)

	errBodyNameClash, err := json.Marshal(osObj)
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "test"})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tempClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateOsImage", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	tsc1 := &tmocks.Client{}
	scp.IDClientMap[site1.ID.String()] = tsc1

	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateOsImage", mock.Anything).Return(wruntimeout, nil)

	tsc1.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name                          string
		reqOrgName                    string
		reqBody                       string
		reqBodyModel                  *model.APIOperatingSystemCreateRequest
		user                          *cdbm.User
		expectedErr                   bool
		expectedStatus                int
		expectedOperatingSystemStatus string
		expectedStatusHistoryCount    int
		expectedImageURL              bool
		verifyChildSpanner            bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			reqBody:        string(okBody),
			reqBodyModel:   &osObj,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			reqBody:        string(okBody),
			reqBodyModel:   &osObj,
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when request body doesnt bind",
			reqOrgName:     tnOrg1,
			reqBody:        "SomeNonJsonBody",
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when request doesnt validate",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyDoesntValidate),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when org does not have a tenant",
			reqOrgName:     tnOrg3,
			reqBody:        string(okBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when both imageURL and idxScrip specified in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(errIpxeImageUrlBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when bad disk path specified in request",
			reqOrgName:     tnOrg1,
			user:           tnu,
			reqBody:        string(errDiskImageBody),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when bad imageURL specified in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(badImageURLBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when imageURL specified in request but imageSHA is nil",
			reqOrgName:     tnOrg1,
			reqBody:        string(errImageURLBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when both rootFsID and rootFsLabel specified in request",
			reqOrgName:     tnOrg1,
			reqBody:        string(errRootFsBody),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "imageURL specified in request (temporarily expect StatusBadRequest - Image based OS creation not allowed)",
			reqOrgName:         tnOrg2,
			reqBody:            string(validImageURLBody),
			reqBodyModel:       &osImageURLObj3,
			user:               tnu,
			expectedErr:        true,
			expectedStatus:     http.StatusBadRequest,
			verifyChildSpanner: false,
		},
		{
			name:           "imageURL specified in request, context deadline timeout (temporarily expect StatusBadRequest - Image based OS creation not allowed)",
			reqOrgName:     tnOrg2,
			reqBody:        string(terminateCreationImageURLBody),
			reqBodyModel:   &osImageURLObj7,
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:                          "success case",
			reqOrgName:                    tnOrg1,
			reqBody:                       string(okBody),
			reqBodyModel:                  &osObj,
			user:                          tnu,
			expectedErr:                   false,
			expectedStatus:                http.StatusCreated,
			expectedOperatingSystemStatus: cdbm.OperatingSystemStatusReady,
			expectedStatusHistoryCount:    1,
			verifyChildSpanner:            true,
		},
		{
			name:           "error due to name clash",
			reqOrgName:     tnOrg1,
			reqBody:        string(errBodyNameClash),
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			cosh := CreateOperatingSystemHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}

			err := cosh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusCreated)
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIOperatingSystem{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				// validate response fields
				assert.Equal(t, len(rsp.StatusHistory), tc.expectedStatusHistoryCount)
				assert.Equal(t, tenant1.ID.String(), *rsp.TenantID)
				if !tc.expectedImageURL {
					assert.Equal(t, *rsp.IpxeScript, *tc.reqBodyModel.IpxeScript)
					assert.Equal(t, *rsp.Type, "iPXE")
				} else {
					assert.Equal(t, *rsp.ImageURL, *tc.reqBodyModel.ImageURL)
					assert.Equal(t, *rsp.Type, "Image")
					if tc.reqBodyModel.ImageSHA != nil {
						assert.Equal(t, *rsp.ImageSHA, *tc.reqBodyModel.ImageSHA)
					}
					if tc.reqBodyModel.ImageAuthType != nil {
						assert.Equal(t, *rsp.ImageAuthType, *tc.reqBodyModel.ImageAuthType)
					}
					if tc.reqBodyModel.ImageAuthToken != nil {
						assert.Equal(t, *rsp.ImageAuthToken, *tc.reqBodyModel.ImageAuthToken)
					}
					if tc.reqBodyModel.ImageDisk != nil {
						assert.Equal(t, *rsp.ImageDisk, *tc.reqBodyModel.ImageDisk)
					}
					if tc.reqBodyModel.RootFsID != nil {
						assert.Equal(t, *rsp.RootFsID, *tc.reqBodyModel.RootFsID)
					}
					if tc.reqBodyModel.RootFsLabel != nil {
						assert.Equal(t, *rsp.RootFsLabel, *tc.reqBodyModel.RootFsLabel)
					}
				}

				assert.Equal(t, *rsp.UserData, *tc.reqBodyModel.UserData)
				assert.Equal(t, rsp.Name, tc.reqBodyModel.Name)
				assert.Equal(t, rsp.Status, tc.expectedOperatingSystemStatus)
				if tc.reqBodyModel.PhoneHomeEnabled != nil {
					assert.Equal(t, rsp.PhoneHomeEnabled, *tc.reqBodyModel.PhoneHomeEnabled)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2, tnOrg3, tnOrg4}, tnRoles)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-1")
	testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant-2")

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, tnu.ID)
	assert.NotNil(t, ts1)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	totalCount := 30

	oss := []cdbm.OperatingSystem{}

	for i := 0; i < totalCount; i++ {
		os := &cdbm.OperatingSystem{}
		if i == 0 || i == 1 {
			os, err := osDAO.Create(
				ctx,
				nil,
				cdbm.OperatingSystemCreateInput{
					Name:               fmt.Sprintf("test-os-%02d", i),
					Description:        cutil.GetPtr("test"),
					Org:                tnOrg1,
					TenantID:           &tenant1.ID,
					OsType:             cdbm.OperatingSystemTypeImage,
					ImageURL:           cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"),
					ImageSHA:           cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"),
					ImageAuthType:      cutil.GetPtr("Basic"),
					ImageAuthToken:     cutil.GetPtr("rsa"),
					ImageDisk:          cutil.GetPtr("/dev/nvme1n3"),
					RootFsId:           cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"),
					UserData:           cutil.GetPtr(cdmu.TestCommonCloudInit),
					AllowOverride:      false,
					EnableBlockStorage: false,
					PhoneHomeEnabled:   false,
					Status:             cdbm.OperatingSystemStatusSyncing,
					CreatedBy:          tnu.ID,
				},
			)
			assert.Nil(t, err)
			common.TestBuildOperatingSystemSiteAssociation(t, dbSession, os.ID, site.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSyncing, tnu)
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusSyncing, cutil.GetPtr("received Operating System creation request, syncing"))
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusReady, cutil.GetPtr("Operating System is now ready for use"))
		} else {
			os, err := osDAO.Create(
				ctx,
				nil,
				cdbm.OperatingSystemCreateInput{
					Name:               fmt.Sprintf("test-os-%02d", i),
					Description:        cutil.GetPtr("test"),
					Org:                tnOrg1,
					TenantID:           &tenant1.ID,
					OsType:             cdbm.OperatingSystemTypeIPXE,
					IpxeScript:         cutil.GetPtr("ipxe"),
					AllowOverride:      false,
					EnableBlockStorage: false,
					PhoneHomeEnabled:   false,
					Status:             cdbm.OperatingSystemStatusPending,
					CreatedBy:          tnu.ID,
				},
			)
			assert.Nil(t, err)
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusPending, cutil.GetPtr("request received, pending processing"))
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusReady, cutil.GetPtr("OPerating System is now ready for use"))
		}
		oss = append(oss, *os)
	}

	site2 := testMachineBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site2)

	tenant4 := testMachineBuildTenant(t, dbSession, tnOrg4, "test-tenant-4")
	testMachineBuildTenant(t, dbSession, tnOrg4, "test-tenant-4")

	ts4 := testBuildTenantSiteAssociation(t, dbSession, tnOrg4, tenant4.ID, site2.ID, tnu.ID)
	assert.NotNil(t, ts4)

	imageoss := []cdbm.OperatingSystem{}
	imageossa := []cdbm.OperatingSystemSiteAssociation{}
	ipxeoss := []cdbm.OperatingSystem{}
	totalCount2 := 18

	for i := 0; i < totalCount2; i++ {
		if i%2 == 0 {
			os, err := osDAO.Create(
				ctx,
				nil,
				cdbm.OperatingSystemCreateInput{
					Name:               fmt.Sprintf("site-test-os-%02d", i),
					Description:        cutil.GetPtr("test"),
					Org:                tnOrg4,
					TenantID:           &tenant4.ID,
					OsType:             cdbm.OperatingSystemTypeImage,
					ImageURL:           cutil.GetPtr("https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/debian-12.1.0-amd64-netinst.iso"),
					ImageSHA:           cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"),
					ImageAuthType:      cutil.GetPtr("Basic"),
					ImageAuthToken:     cutil.GetPtr("rsa"),
					ImageDisk:          cutil.GetPtr("/dev/nvme1n3"),
					RootFsId:           cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"),
					UserData:           cutil.GetPtr(cdmu.TestCommonCloudInit),
					AllowOverride:      false,
					EnableBlockStorage: false,
					PhoneHomeEnabled:   false,
					Status:             cdbm.OperatingSystemStatusSyncing,
					CreatedBy:          tnu.ID,
				},
			)
			assert.Nil(t, err)

			ossa := common.TestBuildOperatingSystemSiteAssociation(t, dbSession, os.ID, site2.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSyncing, tnu)
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusSyncing, cutil.GetPtr("received Operating System creation request, syncing"))
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusReady, cutil.GetPtr("Operating System is now ready for use"))

			assert.Nil(t, err)
			imageoss = append(imageoss, *os)
			imageossa = append(imageossa, *ossa)

		} else {
			os, err := osDAO.Create(
				ctx,
				nil,
				cdbm.OperatingSystemCreateInput{
					Name:               fmt.Sprintf("site-test-os-%02d", i),
					Description:        cutil.GetPtr("test"),
					Org:                tnOrg4,
					TenantID:           &tenant4.ID,
					OsType:             cdbm.OperatingSystemTypeIPXE,
					IpxeScript:         cutil.GetPtr("ipxe"),
					AllowOverride:      false,
					EnableBlockStorage: false,
					PhoneHomeEnabled:   false,
					Status:             cdbm.OperatingSystemStatusPending,
					CreatedBy:          tnu.ID,
				},
			)
			assert.Nil(t, err)

			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusPending, cutil.GetPtr("request received, pending processing"))
			common.TestBuildStatusDetail(t, dbSession, os.ID.String(), cdbm.OperatingSystemStatusReady, cutil.GetPtr("OPerating System is now ready for use"))

			ipxeoss = append(ipxeoss, *os)
		}
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		siteID                            *uuid.UUID
		osType                            *string
		querySearch                       *string
		queryStatus                       *string
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.OperatingSystem
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user is not a member of org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg3,
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case when objects returned",
			reqOrgName:         tnOrg1,
			user:               tnu,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			verifyChildSpanner: true,
		},
		{
			name:                   "success when tenant relation are specified",
			reqOrgName:             tnOrg1,
			user:                   tnu,
			queryIncludeRelations1: cutil.GetPtr(cdbm.TenantRelationName),
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			expectedCnt:            paginator.DefaultLimit,
			expectedTotal:          &totalCount,
			expectedTenantOrg:      cutil.GetPtr(tenant1.Org),
		},
		{
			name:           "success case when no objects returned",
			reqOrgName:     tnOrg2,
			user:           tnu,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         tnOrg1,
			user:               tnu,
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("NAME_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectedFirstEntry: &oss[29],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:           "success when name query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when unexisted status query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("ready"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when name and status query search specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			querySearch:    cutil.GetPtr("test ready"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  &totalCount,
		},
		{
			name:           "success when OperatingSystemStatusPending status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr(cdbm.OperatingSystemStatusPending),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    paginator.DefaultLimit,
			expectedTotal:  cutil.GetPtr(totalCount),
		},
		{
			name:           "success when BadStatus status is specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			queryStatus:    cutil.GetPtr("BadRequest"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when site and type filter specified",
			reqOrgName:     tnOrg1,
			user:           tnu,
			siteID:         &site.ID,
			osType:         cutil.GetPtr(cdbm.OperatingSystemTypeImage),
			querySearch:    cutil.GetPtr("test"),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    2,
			expectedTotal:  cutil.GetPtr(2),
		},
		{
			name:           "success when only site filter specified",
			reqOrgName:     tnOrg4,
			user:           tnu,
			siteID:         &site2.ID,
			osType:         nil,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    len(imageoss) + len(ipxeoss),
			expectedTotal:  cutil.GetPtr(len(imageoss) + len(ipxeoss)),
		},
		{
			name:           "success when only image type filter specified",
			reqOrgName:     tnOrg4,
			user:           tnu,
			osType:         cutil.GetPtr(cdbm.OperatingSystemTypeImage),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    len(imageoss),
			expectedTotal:  cutil.GetPtr(len(imageoss)),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.siteID != nil {
				q.Add("siteId", tc.siteID.String())
			}
			if tc.osType != nil {
				q.Add("type", *tc.osType)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryStatus != nil {
				q.Add("status", *tc.queryStatus)
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/operating-system?%s", tc.reqOrgName, q.Encode())

			req := httptest.NewRequest(http.MethodGet, path, nil)

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			mh := GetAllOperatingSystemHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := mh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)
			require.Equal(t, tc.expectedStatus, rec.Code)

			if !tc.expectedErr {
				rsp := []model.APIOperatingSystem{}
				err := json.Unmarshal(rec.Body.Bytes(), &rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedCnt, len(rsp))

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp[0].InfrastructureProvider.Org)
					}

					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp[0].Tenant.Org)
					}
				} else {
					if len(rsp) > 0 {
						assert.Nil(t, rsp[0].Tenant)
						assert.Nil(t, rsp[0].InfrastructureProvider)
					}
				}

				for _, apios := range rsp {
					assert.Equal(t, 2, len(apios.StatusHistory))
					assert.True(t, apios.IsActive, "Operating System should be active by default")
				}

				if tc.osType != nil && *tc.osType == cdbm.OperatingSystemTypeImage {
					for _, apios := range rsp {
						assert.Greater(t, len(apios.SiteAssociations), 0)
					}
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemHandler_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	orgRoles := []string{authz.TenantAdminRole}

	user := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3, tnOrg1, tnOrg2}, orgRoles)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	assert.NotNil(t, tenant1)

	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "t2")
	assert.NotNil(t, tenant2)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, user.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tenant2.ID, site.ID, user.ID)
	assert.NotNil(t, ts2)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	osDAO := cdbm.NewOperatingSystemDAO(dbSession)
	os1, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test1",
			Description:        cutil.GetPtr("test"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusPending,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os1)

	os2, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test2",
			Description:        cutil.GetPtr("test"),
			Org:                ipOrg2,
			TenantID:           &tenant2.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusPending,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os2)

	os3, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test3",
			Description:        cutil.GetPtr("test"),
			Org:                ipOrg2,
			TenantID:           &tenant2.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os3)

	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
	ossaDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os3.ID,
			SiteID:            site.ID,
			Status:            cdbm.OperatingSystemSiteAssociationStatusSynced,
			CreatedBy:         user.ID,
		},
	)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		os                                *cdbm.OperatingSystem
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		expectedErr                       bool
		expectedStatus                    int
		expectedTenantOrg                 *string
		expectedInfrastructureProviderOrg *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			os:             os1,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           user,
			os:             os1,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     ipOrg3,
			user:           user,
			os:             os1,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:       "error when os id doesnt exist",
			reqOrgName: tnOrg1,
			user:       user,
			os: &cdbm.OperatingSystem{
				ID: uuid.New(),
			},
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when os tenant doesnt match tenant in org",
			reqOrgName:     ipOrg1,
			user:           user,
			os:             os2,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case",
			reqOrgName:         tnOrg1,
			user:               user,
			os:                 os1,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
		},
		{
			name:                   "success when both infrastructure and tenant relations are specified",
			reqOrgName:             tnOrg1,
			user:                   user,
			os:                     os1,
			expectedErr:            false,
			expectedStatus:         http.StatusOK,
			queryIncludeRelations1: cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2: cutil.GetPtr(cdbm.TenantRelationName),
			expectedTenantOrg:      cutil.GetPtr(tenant1.Org),
		},
		{
			name:               "success case with image based os",
			reqOrgName:         tnOrg2,
			user:               user,
			os:                 os3,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			verifyChildSpanner: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)

			q := req.URL.Query()
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}

			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.os.ID.String()}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := GetOperatingSystemHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			if tc.expectedStatus != rec.Code {
				t.Errorf("response: %v\n", rec.Body.String())
			}

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APIOperatingSystem{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.os.Name, rsp.Name)
				assert.Equal(t, tc.os.TenantID.String(), *rsp.TenantID)

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}

					if tc.expectedTenantOrg != nil {
						assert.Equal(t, *tc.expectedTenantOrg, rsp.Tenant.Org)
					}
				} else {
					assert.Nil(t, rsp.Tenant)
					assert.Nil(t, rsp.InfrastructureProvider)
				}

				if len(rsp.SiteAssociations) > 0 {
					assert.Equal(t, *rsp.Type, cdbm.OperatingSystemTypeImage)
				} else {
					assert.Equal(t, *rsp.Type, cdbm.OperatingSystemTypeIPXE)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	orgRoles := []string{authz.TenantAdminRole}
	user := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3, tnOrg1, tnOrg2}, orgRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	assert.NotNil(t, ip)

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site)

	site1 := testMachineBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered)
	assert.NotNil(t, site1)

	tenant1 := testMachineBuildTenant(t, dbSession, ipOrg1, "t1")

	tenant2 := testMachineBuildTenant(t, dbSession, ipOrg2, "t2")

	assert.NotNil(t, tenant2)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, user.ID)
	assert.NotNil(t, ts2)

	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant2.ID, site1.ID, user.ID)
	assert.NotNil(t, ts3)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	osDAO := cdbm.NewOperatingSystemDAO(dbSession)
	os1, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-1",
			Description:        cutil.GetPtr("Test Description 1"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusPending,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os1)

	os2, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-2",
			Description:        cutil.GetPtr("Test Description 2"),
			Org:                ipOrg2,
			TenantID:           &tenant2.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusPending,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os2)

	os3, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-3",
			Description:        cutil.GetPtr("Test Description 3"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusPending,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os3)

	os4, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-4",
			Description:        cutil.GetPtr("Test Description 4"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeIPXE,
			IpxeScript:         cutil.GetPtr("ipxe"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os4)

	os5, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-5",
			Description:        cutil.GetPtr("Test Description 5"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			ImageSHA:           cutil.GetPtr("10886660c5b2746ff48224646c5094ebcf88c889"),
			AllowOverride:      false,
			EnableBlockStorage: true,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os5)

	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
	ossaDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os5.ID,
			SiteID:            site.ID,
			Status:            cdbm.OperatingSystemSiteAssociationStatusSynced,
			CreatedBy:         user.ID,
		},
	)

	os6, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-6",
			Description:        cutil.GetPtr("Test Description 6"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsId:           cutil.GetPtr("fsID"),
			AllowOverride:      false,
			EnableBlockStorage: true,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os6)

	os7, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-7",
			Description:        cutil.GetPtr("Test Description 7"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os7)

	os8, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-8",
			Description:        cutil.GetPtr("Test Description 8"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os8)

	os9, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-9",
			Description:        cutil.GetPtr("Test Description 9"),
			Org:                ipOrg2,
			TenantID:           &tenant2.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			ImageSHA:           cutil.GetPtr("10886660c5b2746ff48224646c5094ebcf88c889"),
			AllowOverride:      false,
			EnableBlockStorage: true,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os9)

	ossaDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os9.ID,
			SiteID:            site1.ID,
			Status:            cdbm.OperatingSystemSiteAssociationStatusSyncing,
			CreatedBy:         user.ID,
		},
	)

	// OS to be deactivated (1)
	os10, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-10",
			Description:        cutil.GetPtr("Test Description 10"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os10)

	// OS to be deactivated (2)
	os11, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-11",
			Description:        cutil.GetPtr("Test Description 11"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os11)

	// deactivated OS (2)
	os12, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-12",
			Description:        cutil.GetPtr("Test Description 12"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os12)
	os12, err = osDAO.Update(ctx, nil, cdbm.OperatingSystemUpdateInput{OperatingSystemId: os12.ID, IsActive: cutil.GetPtr(false)})
	assert.Nil(t, err)
	assert.NotNil(t, os12)

	// deactivated OS (2)
	os13, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-13",
			Description:        cutil.GetPtr("Test Description 13"),
			Org:                ipOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			RootFsLabel:        cutil.GetPtr("fs-label"),
			AllowOverride:      false,
			EnableBlockStorage: false,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os13)
	os13, err = osDAO.Update(ctx, nil, cdbm.OperatingSystemUpdateInput{OperatingSystemId: os13.ID, IsActive: cutil.GetPtr(false)})
	assert.Nil(t, err)
	assert.NotNil(t, os13)

	updReq := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated"), Description: cutil.GetPtr("Updated Description"), IpxeScript: cutil.GetPtr("updatedIpxe"), UserData: cutil.GetPtr(cdmu.TestCommonCloudInit), IsCloudInit: cutil.GetPtr(false), AllowOverride: cutil.GetPtr(true), PhoneHomeEnabled: cutil.GetPtr(false)}
	okBody, err := json.Marshal(updReq)
	assert.Nil(t, err)

	updReqNameClash := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-operating-system-3")}
	errBodyNameClash, err := json.Marshal(updReqNameClash)
	assert.Nil(t, err)

	updReq2 := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-operating-system-2")}
	okBody2, err := json.Marshal(updReq2)
	assert.Nil(t, err)

	updReqImageUrl := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-2"), Description: cutil.GetPtr("Updated Description"), ImageURL: cutil.GetPtr("http://imagepath.iso")}
	errBodyImageUrlIpxe, err := json.Marshal(updReqImageUrl)
	assert.Nil(t, err)

	// imageUrl/imageSha are immutable, so a valid update re-sends the
	// existing imageUrl (matching the os5/os9 fixtures) unchanged while
	// updating mutable image attributes such as the root filesystem and
	// image disk.
	updReqValidImageUrl := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-3"), Description: cutil.GetPtr("Updated Description"), ImageURL: cutil.GetPtr("https://oldimagepath.iso"), ImageSHA: cutil.GetPtr("10886660c5b2746ff48224646c5094ebcf88c889"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), ImageDisk: cutil.GetPtr("/dev/nvme2n1")}
	okBodyImageUrl, err := json.Marshal(updReqValidImageUrl)
	assert.Nil(t, err)

	// Changing imageUrl is rejected up front because the underlying image
	// content is immutable after creation.
	updReqChangeImageUrl := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-change-url"), ImageURL: cutil.GetPtr("http://newimagepath.iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980")}
	errBodyChangeImageUrl, err := json.Marshal(updReqChangeImageUrl)
	assert.Nil(t, err)

	updReqDeactivate := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-deactivate"), Description: cutil.GetPtr("Updated Description for deactivation"), IsActive: cutil.GetPtr(false), DeactivationNote: cutil.GetPtr("Deactivated for a valid reason")}
	okBodyDeactivate, err := json.Marshal(updReqDeactivate)
	assert.Nil(t, err)

	updReqDeactivateNoNote := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-deactivate-no-note"), Description: cutil.GetPtr("Updated Description for deactivation"), IsActive: cutil.GetPtr(false)}
	okBodyDeactivateNoNote, err := json.Marshal(updReqDeactivateNoNote)
	assert.Nil(t, err)

	updReqChangeNote := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-change-note"), DeactivationNote: cutil.GetPtr("Changed Note")}
	okBodyChangeNote, err := json.Marshal(updReqChangeNote)
	assert.Nil(t, err)

	updReqActivate := model.APIOperatingSystemUpdateRequest{Name: cutil.GetPtr("test-os-updated-activate"), Description: cutil.GetPtr("Updated Description for activation"), IsActive: cutil.GetPtr(true)}
	okBodyActivate, err := json.Marshal(updReqActivate)
	assert.Nil(t, err)

	errBodyDoesntValidate, err := json.Marshal(struct{ Name string }{Name: "t"})
	assert.Nil(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tempClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateOsImage", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	tsc1 := &tmocks.Client{}
	scp.IDClientMap[site1.ID.String()] = tsc1

	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateOsImage", mock.Anything).Return(wruntimeout, nil)

	tsc1.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		reqBody        string
		reqUpdateModel *model.APIOperatingSystemUpdateRequest
		osID           string
		expectedErr    bool
		expectedStatus int

		expectedName             *string
		expectedDesc             *string
		expectedIpxeScript       *string
		expectedImageURL         *string
		expectedUserData         *string
		expectedIsCloudInit      *bool
		expectedAllowOverride    *bool
		expectedPhoneHomeEnabled *bool
		expectedIsActive         *bool
		expectedDeactivationNote *string
		verifyChildSpanner       bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     ipOrg3,
			user:           user,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when os id doesnt exist",
			reqOrgName:     ipOrg1,
			user:           user,
			osID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when os tenant doesnt match tenant in org",
			reqOrgName:     ipOrg1,
			user:           user,
			osID:           os2.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when req body doesnt bind",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        "bad-body",
			osID:           os1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when req body doesnt validate",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(errBodyDoesntValidate),
			osID:           os1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when updated name clashes",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(errBodyNameClash),
			osID:           os1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusConflict,
		},
		{
			name:           "error when os created with ipxe but request to update imageURL ",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(errBodyImageUrlIpxe),
			osID:           os4.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "success when updated with same name",
			reqOrgName:     ipOrg2,
			user:           user,
			reqBody:        string(okBody2),
			osID:           os2.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName: cutil.GetPtr("test-operating-system-2"),
		},
		{
			name:           "success case - can update all fields",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBody),
			osID:           os1.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName:             cutil.GetPtr("test-os-updated"),
			expectedDesc:             cutil.GetPtr("Updated Description"),
			expectedIpxeScript:       cutil.GetPtr("updatedIpxe"),
			expectedUserData:         cutil.GetPtr(cdmu.TestCommonPhoneHomeCloudInit),
			expectedIsCloudInit:      cutil.GetPtr(true),
			expectedAllowOverride:    cutil.GetPtr(true),
			expectedPhoneHomeEnabled: cutil.GetPtr(true),
			verifyChildSpanner:       true,
		},
		{
			name:           "should succeed to deactivate active OS",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBodyDeactivate),
			osID:           os10.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName:             updReqDeactivate.Name,
			expectedDesc:             updReqDeactivate.Description,
			expectedIsActive:         cutil.GetPtr(false),
			expectedDeactivationNote: updReqDeactivate.DeactivationNote,
		},
		{
			name:           "should succeed to deactivate active OS without Deactivation Note",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBodyDeactivateNoNote),
			osID:           os11.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName:             updReqDeactivateNoNote.Name,
			expectedDesc:             updReqDeactivateNoNote.Description,
			expectedIsActive:         cutil.GetPtr(false),
			expectedDeactivationNote: updReqDeactivateNoNote.DeactivationNote,
		},
		{
			name:           "should fail to change Deactivation Note for an active OS",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBodyChangeNote),
			osID:           os1.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "should succeed to change Deactivation Note on deactivated OS",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBodyChangeNote),
			osID:           os12.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName:             updReqChangeNote.Name,
			expectedDesc:             updReqChangeNote.Description,
			expectedIsActive:         cutil.GetPtr(false),
			expectedDeactivationNote: updReqChangeNote.DeactivationNote,
		},
		{
			name:           "should succeed to activate deactivated OS (3/4)",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(okBodyActivate),
			osID:           os13.ID.String(),
			expectedErr:    false,
			expectedStatus: http.StatusOK,

			expectedName:             updReqActivate.Name,
			expectedDesc:             updReqActivate.Description,
			expectedIsActive:         cutil.GetPtr(false),
			expectedDeactivationNote: nil,
		},
		{
			name:             "success when image OS updated with unchanged imageURL and mutable attributes",
			reqOrgName:       ipOrg1,
			user:             user,
			reqBody:          string(okBodyImageUrl),
			reqUpdateModel:   &updReqValidImageUrl,
			osID:             os5.ID.String(),
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedImageURL: cutil.GetPtr("https://oldimagepath.iso"),
		},
		{
			name:           "error when image OS update tries to change imageURL",
			reqOrgName:     ipOrg1,
			user:           user,
			reqBody:        string(errBodyChangeImageUrl),
			osID:           os5.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when image OS update workflow fails with context deadline error",
			reqOrgName:     ipOrg2,
			user:           user,
			reqBody:        string(okBodyImageUrl),
			reqUpdateModel: &updReqValidImageUrl,
			osID:           os9.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(tc.reqBody))

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.osID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tah := UpdateOperatingSystemHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
				scp:       scp,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK)

			if !tc.expectedErr {
				rsp := &model.APIOperatingSystem{}
				err := json.Unmarshal(rec.Body.Bytes(), rsp)
				assert.Nil(t, err)

				if tc.expectedName != nil {
					assert.Equal(t, *tc.expectedName, rsp.Name)
				}

				if tc.expectedDesc != nil {
					assert.Equal(t, *tc.expectedDesc, *rsp.Description)
				}

				if tc.expectedIpxeScript != nil {
					assert.Equal(t, *tc.expectedIpxeScript, *rsp.IpxeScript)
					assert.Equal(t, "iPXE", *rsp.Type)
				}

				if tc.expectedUserData != nil && tc.expectedPhoneHomeEnabled != nil && !*tc.expectedPhoneHomeEnabled {
					assert.Equal(t, *tc.expectedUserData, *rsp.UserData)
				}

				if tc.expectedIsCloudInit != nil {
					assert.Equal(t, *tc.expectedIsCloudInit, rsp.IsCloudInit)
				}

				if tc.expectedAllowOverride != nil {
					assert.Equal(t, *tc.expectedAllowOverride, rsp.AllowOverride)
				}

				if tc.expectedImageURL != nil {
					assert.Equal(t, *tc.expectedImageURL, *rsp.ImageURL)
					assert.Equal(t, cdbm.OperatingSystemTypeImage, *rsp.Type)
					if tc.reqUpdateModel != nil {
						if tc.reqUpdateModel.ImageSHA != nil {
							assert.Equal(t, *tc.reqUpdateModel.ImageSHA, *rsp.ImageSHA)
						}
						if tc.reqUpdateModel.ImageAuthType != nil {
							assert.Equal(t, *tc.reqUpdateModel.ImageAuthType, *rsp.ImageAuthType)
						}
						if tc.reqUpdateModel.ImageAuthToken != nil {
							assert.Equal(t, *tc.reqUpdateModel.ImageAuthToken, *rsp.ImageAuthToken)
						}
						if tc.reqUpdateModel.ImageDisk != nil {
							assert.Equal(t, *tc.reqUpdateModel.ImageDisk, *rsp.ImageDisk)
						}
						if tc.reqUpdateModel.RootFsID != nil {
							assert.Equal(t, *tc.reqUpdateModel.RootFsID, *rsp.RootFsID)
						}
						if tc.reqUpdateModel.RootFsLabel != nil {
							assert.Equal(t, *tc.reqUpdateModel.RootFsLabel, *rsp.RootFsLabel)
						}

					}
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestOperatingSystemHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnOrg4 := "test-tn-org-4"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1, tnOrg2, tnOrg3, tnOrg4}, tnRoles)

	ip3 := testIPBlockBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg3, ipu)
	site := testIPBlockBuildSite(t, dbSession, ip3, "testSite", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site)

	site2 := testIPBlockBuildSite(t, dbSession, ip3, "testSite2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, site2)

	tenant1 := testMachineBuildTenant(t, dbSession, tnOrg1, "t1")
	testMachineBuildTenant(t, dbSession, tnOrg2, "t2")
	tenant3 := testMachineBuildTenant(t, dbSession, tnOrg3, "t3")

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tenant1.ID, site.ID, tnu.ID)
	assert.NotNil(t, ts1)

	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg2, tenant3.ID, site2.ID, tnu.ID)
	assert.NotNil(t, ts2)

	vpc3 := testAllocationBuildVpc(t, dbSession, ip3, site, tenant3, ipOrg3, "testVPC")

	cfg := common.GetTestConfig()

	osDAO := cdbm.NewOperatingSystemDAO(dbSession)
	os1, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:               "test1",
		Description:        cutil.GetPtr("test"),
		Org:                ipOrg1,
		TenantID:           &tenant1.ID,
		OsType:             cdbm.OperatingSystemTypeIPXE,
		IpxeScript:         cutil.GetPtr("ipxe"),
		AllowOverride:      false,
		EnableBlockStorage: false,
		PhoneHomeEnabled:   false,
		Status:             cdbm.OperatingSystemStatusPending,
		CreatedBy:          tnu.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, os1)
	os3, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:               "test2",
		Description:        cutil.GetPtr("test"),
		Org:                ipOrg3,
		TenantID:           &tenant3.ID,
		OsType:             cdbm.OperatingSystemTypeIPXE,
		IpxeScript:         cutil.GetPtr("ipxe"),
		AllowOverride:      false,
		EnableBlockStorage: false,
		PhoneHomeEnabled:   false,
		Status:             cdbm.OperatingSystemStatusPending,
		CreatedBy:          tnu.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, os3)

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	it3 := testMachineBuildInstanceType(t, dbSession, ip3, site, "testIT")

	// build some machines, and map the machines to the instancetypes
	for i := 1; i <= 3; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip3.ID, site.ID, cutil.GetPtr(false), nil)
		assert.NotNil(t, mc)
		mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc, it3)
		assert.NotNil(t, mcinst1)
	}

	acGoodIT := model.APIAllocationConstraintCreateRequest{ResourceType: cdbm.AllocationResourceTypeInstanceType, ResourceTypeID: it3.ID.String(), ConstraintType: cdbm.AllocationConstraintTypeReserved, ConstraintValue: 2}
	okBodyIT, err := json.Marshal(model.APIAllocationCreateRequest{Name: "ok1", Description: cutil.GetPtr(""), TenantID: tenant3.ID.String(), SiteID: site.ID.String(), AllocationConstraints: []model.APIAllocationConstraintCreateRequest{acGoodIT}})
	assert.Nil(t, err)
	testCreateAllocation(t, dbSession, ipamStorage, ipu, ipOrg3, string(okBodyIT))
	instanceDAO := cdbm.NewInstanceDAO(dbSession)
	instance, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst",
			TenantID:                 tenant3.ID,
			InfrastructureProviderID: ip3.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &it3.ID,
			VpcID:                    vpc3.ID,
			OperatingSystemID:        &os3.ID,
			Status:                   cdbm.InstanceStatusReady,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, instance)

	os5, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-5",
			Description:        cutil.GetPtr("Test Description 5"),
			Org:                tnOrg1,
			TenantID:           &tenant1.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			AllowOverride:      false,
			EnableBlockStorage: true,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os5)

	// Add Operating System Site Association
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
	ossaDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os5.ID,
			SiteID:            site.ID,
			Version:           cutil.GetPtr("test"),
			Status:            cdbm.OperatingSystemSiteAssociationStatusSyncing,
			CreatedBy:         tnu.ID,
		},
	)

	os6, err := osDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemCreateInput{
			Name:               "test-operating-system-6",
			Description:        cutil.GetPtr("Test Description 6"),
			Org:                tnOrg3,
			TenantID:           &tenant3.ID,
			OsType:             cdbm.OperatingSystemTypeImage,
			ImageURL:           cutil.GetPtr("https://oldimagepath.iso"),
			AllowOverride:      false,
			EnableBlockStorage: true,
			PhoneHomeEnabled:   false,
			Status:             cdbm.OperatingSystemStatusReady,
			CreatedBy:          tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, os6)

	// Add Operating System Site Association
	ossaDAO.Create(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: os6.ID,
			SiteID:            site2.ID,
			Version:           cutil.GetPtr("test"),
			Status:            cdbm.OperatingSystemSiteAssociationStatusSyncing,
			CreatedBy:         tnu.ID,
		},
	)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()

	// Normal mocks
	tsc := &tmocks.Client{}
	tempClient := &tmocks.Client{}
	tempScp := sc.NewClientPool(tcfg)
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site2.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tempClient.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID"),
		mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteOsImage", mock.Anything).Return(wrun, nil)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[site.ID.String()] = tscWithTimeout
	scpWithTimeout.IDClientMap[site2.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteOsImage", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[site.ID.String()] = tscWithNICoNotFound
	scpWithNICoNotFound.IDClientMap[site2.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteOsImage", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		osID               string
		expectedErr        bool
		expectedStatus     int
		verifyChildSpanner bool
		tClient            *tmocks.Client
		clientPool         *sc.ClientPool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     tnOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when tenant doesnt exist for org",
			reqOrgName:     tnOrg4,
			user:           tnu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when os id doesnt exist",
			reqOrgName:     tnOrg1,
			user:           tnu,
			osID:           uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when os tenant doesnt match tenant in org",
			reqOrgName:     tnOrg1,
			user:           tnu,
			osID:           os3.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when instance present for os",
			reqOrgName:     tnOrg3,
			user:           tnu,
			osID:           os3.ID.String(),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:               "success case",
			reqOrgName:         tnOrg1,
			user:               tnu,
			osID:               os1.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:               "success case with OS image type",
			reqOrgName:         tnOrg1,
			user:               tnu,
			osID:               os5.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
			clientPool:         scp,
			tClient:            tsc,
		},
		{
			name:               "workflow timeout failure",
			reqOrgName:         tnOrg3,
			user:               tnu,
			osID:               os6.ID.String(),
			expectedErr:        true,
			expectedStatus:     http.StatusInternalServerError,
			verifyChildSpanner: true,
			clientPool:         scpWithTimeout,
			tClient:            tscWithTimeout,
		},
		{
			name:               "nico not-found success",
			reqOrgName:         tnOrg1,
			user:               tnu,
			osID:               os5.ID.String(),
			expectedErr:        false,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
			clientPool:         scpWithNICoNotFound,
			tClient:            tscWithNICoNotFound,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")

			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", nil)

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.osID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			tClient := tempClient
			if tc.tClient != nil {
				tClient = tc.tClient
			}

			clientPool := tempScp
			if tc.clientPool != nil {
				clientPool = tc.clientPool
			}

			tah := DeleteOperatingSystemHandler{
				dbSession: dbSession,
				tc:        tClient,
				scp:       clientPool,
				cfg:       cfg,
			}
			err := tah.Handle(ec)
			assert.Nil(t, err)

			assert.Equal(t, tc.expectedStatus, rec.Code)
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusAccepted)

			if !tc.expectedErr {
				// check that os does not exist
				osDAO := cdbm.NewOperatingSystemDAO(dbSession)
				id1, err := uuid.Parse(tc.osID)
				assert.Nil(t, err)

				os, err := osDAO.GetByID(ctx, nil, id1, nil)
				if os != nil {
					assert.Equal(t, os.Type, cdbm.OperatingSystemTypeImage)
					assert.Equal(t, os.Status, cdbm.OperatingSystemStatusDeleting)
				} else {
					assert.NotNil(t, err)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

// buildRawIpxeProviderOS creates a provider-owned raw iPXE OS (no site
// associations) via the DAO. Raw iPXE avoids any post-commit site sync, so the
// write handlers exercise ownership enforcement without Temporal/proxy mocks.
func buildRawIpxeProviderOS(t *testing.T, ctx context.Context, osDAO cdbm.OperatingSystemDAO, org string, providerID uuid.UUID, name string, createdBy uuid.UUID) *cdbm.OperatingSystem {
	os, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("test"),
		Org:                      org,
		InfrastructureProviderID: &providerID,
		OsType:                   cdbm.OperatingSystemTypeIPXE,
		IpxeScript:               cutil.GetPtr("ipxe"),
		Status:                   cdbm.OperatingSystemStatusReady,
		CreatedBy:                createdBy,
	})
	require.NoError(t, err)
	require.NotNil(t, os)
	return os
}

func buildRawIpxeTenantOS(t *testing.T, ctx context.Context, osDAO cdbm.OperatingSystemDAO, org string, tenantID uuid.UUID, name string, createdBy uuid.UUID) *cdbm.OperatingSystem {
	os, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:        name,
		Description: cutil.GetPtr("test"),
		Org:         org,
		TenantID:    &tenantID,
		OsType:      cdbm.OperatingSystemTypeIPXE,
		IpxeScript:  cutil.GetPtr("ipxe"),
		Status:      cdbm.OperatingSystemStatusReady,
		CreatedBy:   createdBy,
	})
	require.NoError(t, err)
	require.NotNil(t, os)
	return os
}

// TestOperatingSystemHandler_Create_Ownership asserts that a Provider Admin may
// only create iPXE Template-based Operating Systems.
func TestOperatingSystemHandler_Create_Ownership(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	tempClient := &tmocks.Client{}

	provOrg := "own-provider-org"
	provUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{provOrg}, []string{authz.ProviderAdminRole})
	testMachineBuildInfrastructureProvider(t, dbSession, provOrg, "own-ip")

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		reqBody        model.APIOperatingSystemCreateRequest
		expectedStatus int
	}{
		{
			name:           "provider admin cannot create image OS",
			reqBody:        model.APIOperatingSystemCreateRequest{Name: "prov-image", Description: cutil.GetPtr("test"), ImageURL: cutil.GetPtr("https://example.com/img.iso")},
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin cannot create raw ipxe OS",
			reqBody:        model.APIOperatingSystemCreateRequest{Name: "prov-ipxe", Description: cutil.GetPtr("test"), IpxeScript: cutil.GetPtr("ipxe")},
			expectedStatus: http.StatusForbidden,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			body, err := json.Marshal(tc.reqBody)
			require.NoError(t, err)

			e := echo.New()
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(provOrg)
			ec.Set("user", provUser)
			ec.SetRequest(ec.Request().WithContext(context.WithValue(ctx, otelecho.TracerKey, tracer)))

			ch := CreateOperatingSystemHandler{dbSession: dbSession, tc: tempClient, cfg: cfg, scp: scp}
			require.NoError(t, ch.Handle(ec))
			require.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

// TestOperatingSystemHandler_Update_Ownership asserts ownership enforcement for
// updates across provider and tenant roles.
func TestOperatingSystemHandler_Update_Ownership(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	tempClient := &tmocks.Client{}
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	provOrg := "own-provider-org"
	provUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{provOrg}, []string{authz.ProviderAdminRole})
	ip := testMachineBuildInfrastructureProvider(t, dbSession, provOrg, "own-ip")
	provOS := buildRawIpxeProviderOS(t, ctx, osDAO, provOrg, ip.ID, "prov-os-update", provUser.ID)

	sharedOrg := "own-shared-org"
	tnUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{sharedOrg}, []string{authz.TenantAdminRole})
	sharedProvUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{sharedOrg}, []string{authz.ProviderAdminRole})
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, sharedOrg, "own-ip-2")
	tn := testMachineBuildTenant(t, dbSession, sharedOrg, "own-tenant")
	provOSShared := buildRawIpxeProviderOS(t, ctx, osDAO, sharedOrg, ip2.ID, "prov-os-shared-update", sharedProvUser.ID)
	tnOS := buildRawIpxeTenantOS(t, ctx, osDAO, sharedOrg, tn.ID, "tenant-os-update", tnUser.ID)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		os             *cdbm.OperatingSystem
		expectedStatus int
	}{
		{
			name:           "provider admin updates own provider OS",
			reqOrgName:     provOrg,
			user:           provUser,
			os:             provOS,
			expectedStatus: http.StatusOK,
		},
		{
			name:           "tenant admin cannot update provider-owned OS",
			reqOrgName:     sharedOrg,
			user:           tnUser,
			os:             provOSShared,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin cannot update tenant-owned OS",
			reqOrgName:     sharedOrg,
			user:           sharedProvUser,
			os:             tnOS,
			expectedStatus: http.StatusForbidden,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			body, err := json.Marshal(model.APIOperatingSystemUpdateRequest{Description: cutil.GetPtr("updated description")})
			require.NoError(t, err)

			e := echo.New()
			req := httptest.NewRequest(http.MethodPut, "/", strings.NewReader(string(body)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.os.ID.String())
			ec.Set("user", tc.user)
			ec.SetRequest(ec.Request().WithContext(context.WithValue(ctx, otelecho.TracerKey, tracer)))

			uh := UpdateOperatingSystemHandler{dbSession: dbSession, tc: tempClient, cfg: cfg, scp: scp}
			require.NoError(t, uh.Handle(ec))
			require.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

// TestOperatingSystemHandler_Delete_Ownership asserts ownership enforcement for
// deletes across provider and tenant roles.
func TestOperatingSystemHandler_Delete_Ownership(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	tempClient := &tmocks.Client{}
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	provOrg := "own-provider-org"
	provUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{provOrg}, []string{authz.ProviderAdminRole})
	ip := testMachineBuildInfrastructureProvider(t, dbSession, provOrg, "own-ip")
	provOS := buildRawIpxeProviderOS(t, ctx, osDAO, provOrg, ip.ID, "prov-os-delete", provUser.ID)

	sharedOrg := "own-shared-org"
	tnUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{sharedOrg}, []string{authz.TenantAdminRole})
	sharedProvUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{sharedOrg}, []string{authz.ProviderAdminRole})
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, sharedOrg, "own-ip-2")
	tn := testMachineBuildTenant(t, dbSession, sharedOrg, "own-tenant")
	provOSShared := buildRawIpxeProviderOS(t, ctx, osDAO, sharedOrg, ip2.ID, "prov-os-shared-delete", sharedProvUser.ID)
	tnOS := buildRawIpxeTenantOS(t, ctx, osDAO, sharedOrg, tn.ID, "tenant-os-delete", tnUser.ID)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		os             *cdbm.OperatingSystem
		expectedStatus int
	}{
		{
			name:           "tenant admin cannot delete provider-owned OS",
			reqOrgName:     sharedOrg,
			user:           tnUser,
			os:             provOSShared,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin cannot delete tenant-owned OS",
			reqOrgName:     sharedOrg,
			user:           sharedProvUser,
			os:             tnOS,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin deletes own provider OS",
			reqOrgName:     provOrg,
			user:           provUser,
			os:             provOS,
			expectedStatus: http.StatusAccepted,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.os.ID.String())
			ec.Set("user", tc.user)
			ec.SetRequest(ec.Request().WithContext(context.WithValue(ctx, otelecho.TracerKey, tracer)))

			dh := DeleteOperatingSystemHandler{dbSession: dbSession, tc: tempClient, cfg: cfg, scp: scp}
			require.NoError(t, dh.Handle(ec))
			require.Equal(t, tc.expectedStatus, rec.Code)
			assert.NotEqual(t, "", tc.name)
		})
	}
}

// buildProviderOS creates a provider-owned OS (no tenant) for the given provider.
func buildProviderOS(t *testing.T, ctx context.Context, osDAO cdbm.OperatingSystemDAO, org string, providerID uuid.UUID, name string, createdBy uuid.UUID) *cdbm.OperatingSystem {
	os, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("test"),
		Org:                      org,
		InfrastructureProviderID: &providerID,
		OsType:                   cdbm.OperatingSystemTypeIPXE,
		IpxeScript:               cutil.GetPtr("ipxe"),
		Status:                   cdbm.OperatingSystemStatusReady,
		CreatedBy:                createdBy,
	})
	require.NoError(t, err)
	require.NotNil(t, os)
	return os
}

// buildTenantOS creates a tenant-owned OS for the given tenant.
func buildTenantOS(t *testing.T, ctx context.Context, osDAO cdbm.OperatingSystemDAO, org string, tenantID uuid.UUID, name string, createdBy uuid.UUID) *cdbm.OperatingSystem {
	os, err := osDAO.Create(ctx, nil, cdbm.OperatingSystemCreateInput{
		Name:          name,
		Description:   cutil.GetPtr("test"),
		Org:           org,
		TenantID:      &tenantID,
		OsType:        cdbm.OperatingSystemTypeIPXE,
		IpxeScript:    cutil.GetPtr("ipxe"),
		Status:        cdbm.OperatingSystemStatusReady,
		CreatedBy:     createdBy,
		AllowOverride: false,
	})
	require.NoError(t, err)
	require.NotNil(t, os)
	return os
}

// TestOperatingSystemHandler_GetAll_Visibility exercises provider-admin listing
// and tenant cross-visibility of provider-owned OSes at accessible sites.
func TestOperatingSystemHandler_GetAll_Visibility(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	// Provider-only org.
	provOrg := "vis-provider-org"
	provUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{provOrg}, []string{authz.ProviderAdminRole})
	ip := testMachineBuildInfrastructureProvider(t, dbSession, provOrg, "vis-ip")
	siteA := testMachineBuildSite(t, dbSession, ip, "vis-site-a", cdbm.SiteStatusRegistered)
	siteB := testMachineBuildSite(t, dbSession, ip, "vis-site-b", cdbm.SiteStatusRegistered)

	provOSA := buildProviderOS(t, ctx, osDAO, provOrg, ip.ID, "prov-os-a", provUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provOSA.ID, siteA.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, provUser)
	provOSB := buildProviderOS(t, ctx, osDAO, provOrg, ip.ID, "prov-os-b", provUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provOSB.ID, siteB.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, provUser)

	// The tenant can see provider-owned entries from a different org only when
	// they are associated with a Site the tenant can access.
	tenantOrg := "vis-tenant-org"
	servingProviderOrg := "vis-serving-provider-org"
	tnUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tenantOrg}, []string{authz.TenantAdminRole})
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, servingProviderOrg, "vis-ip-2")
	siteC := testMachineBuildSite(t, dbSession, ip2, "vis-site-c", cdbm.SiteStatusRegistered)
	siteD := testMachineBuildSite(t, dbSession, ip2, "vis-site-d", cdbm.SiteStatusRegistered)
	tn := testMachineBuildTenant(t, dbSession, tenantOrg, "vis-tenant")
	tsC := testBuildTenantSiteAssociation(t, dbSession, tenantOrg, tn.ID, siteC.ID, tnUser.ID)
	assert.NotNil(t, tsC)

	buildTenantOS(t, ctx, osDAO, tenantOrg, tn.ID, "tenant-os-1", tnUser.ID)
	buildTenantOS(t, ctx, osDAO, tenantOrg, tn.ID, "tenant-os-2", tnUser.ID)
	provC := buildProviderOS(t, ctx, osDAO, servingProviderOrg, ip2.ID, "prov-os-c", tnUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provC.ID, siteC.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provC.ID, siteD.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	provD := buildProviderOS(t, ctx, osDAO, servingProviderOrg, ip2.ID, "prov-os-d", tnUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provD.ID, siteD.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	buildProviderOS(t, ctx, osDAO, servingProviderOrg, ip2.ID, "prov-os-without-site", tnUser.ID)
	privOrg := "vis-privileged-org"
	privUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{privOrg}, []string{authz.TenantAdminRole})
	privTenant := testMachineBuildTenant(t, dbSession, privOrg, "vis-privileged-tenant")
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip2, &privTenant.ID, privOrg, cdbm.TenantAccountStatusReady, privUser)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name          string
		reqOrgName    string
		user          *cdbm.User
		target        string
		expectedNames []string
		expectedSites map[string][]string
	}{
		{
			name:          "provider admin sees only provider-owned OSes",
			reqOrgName:    provOrg,
			user:          provUser,
			expectedNames: []string{"prov-os-a", "prov-os-b"},
		},
		{
			name:          "tenant admin sees own OSes plus provider OSes at accessible sites",
			reqOrgName:    tenantOrg,
			user:          tnUser,
			expectedNames: []string{"tenant-os-1", "tenant-os-2", "prov-os-c"},
			expectedSites: map[string][]string{"prov-os-c": {siteC.ID.String()}},
		},
		{
			name:          "tenant visibility union is paginated after merging",
			reqOrgName:    tenantOrg,
			user:          tnUser,
			target:        "/?pageSize=2&orderBy=NAME_ASC",
			expectedNames: []string{"prov-os-c", "tenant-os-1"},
		},
		{
			name:          "privileged tenant lists only OSes at its provider sites without memberships",
			reqOrgName:    privOrg,
			user:          privUser,
			expectedNames: []string{"prov-os-c", "prov-os-d"},
			expectedSites: map[string][]string{"prov-os-c": {siteC.ID.String(), siteD.ID.String()}},
		},
		{
			name:          "privileged tenant can filter by an implicitly authorized site",
			reqOrgName:    privOrg,
			user:          privUser,
			target:        "/?siteId=" + siteC.ID.String(),
			expectedNames: []string{"prov-os-c"},
			expectedSites: map[string][]string{"prov-os-c": {siteC.ID.String()}},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			target := tc.target
			if target == "" {
				target = "/"
			}
			req := httptest.NewRequest(http.MethodGet, target, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			ec.Set("user", tc.user)

			reqCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(reqCtx))

			mh := GetAllOperatingSystemHandler{dbSession: dbSession, tc: tempClient, cfg: cfg}
			err := mh.Handle(ec)
			assert.Nil(t, err)
			require.Equal(t, http.StatusOK, rec.Code)

			rsp := []model.APIOperatingSystem{}
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &rsp))
			gotNames := make([]string, len(rsp))
			for i, os := range rsp {
				gotNames[i] = os.Name
				wantSites, checkSites := tc.expectedSites[os.Name]
				if checkSites {
					gotSites := make([]string, 0, len(os.SiteAssociations))
					for _, association := range os.SiteAssociations {
						require.NotNil(t, association.Site)
						gotSites = append(gotSites, association.Site.ID)
					}
					assert.ElementsMatch(t, wantSites, gotSites)
				}
			}
			assert.ElementsMatch(t, tc.expectedNames, gotNames)
		})
	}
}

// TestOperatingSystemHandler_GetByID_Visibility exercises role-based access to a
// single OS: provider admins may only read provider-owned entries, tenant admins
// may read own entries plus provider entries at accessible sites.
func TestOperatingSystemHandler_GetByID_Visibility(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}
	osDAO := cdbm.NewOperatingSystemDAO(dbSession)

	tenantOrg := "vis-tenant-org"
	servingProviderOrg := "vis-serving-provider-org"
	tnUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tenantOrg}, []string{authz.TenantAdminRole})
	servingProviderUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{servingProviderOrg}, []string{authz.ProviderAdminRole})
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, servingProviderOrg, "vis-ip-2")
	siteC := testMachineBuildSite(t, dbSession, ip2, "vis-site-c", cdbm.SiteStatusRegistered)
	siteD := testMachineBuildSite(t, dbSession, ip2, "vis-site-d", cdbm.SiteStatusRegistered)
	tn := testMachineBuildTenant(t, dbSession, tenantOrg, "vis-tenant")
	testBuildTenantSiteAssociation(t, dbSession, tenantOrg, tn.ID, siteC.ID, tnUser.ID)

	tnOS := buildTenantOS(t, ctx, osDAO, tenantOrg, tn.ID, "tenant-os-1", tnUser.ID)
	provC := buildProviderOS(t, ctx, osDAO, servingProviderOrg, ip2.ID, "prov-os-c", tnUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provC.ID, siteC.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provC.ID, siteD.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	provD := buildProviderOS(t, ctx, osDAO, servingProviderOrg, ip2.ID, "prov-os-d", tnUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, provD.ID, siteD.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, tnUser)
	noSitesOrg := "vis-no-sites-org"
	noSitesUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{noSitesOrg}, []string{authz.TenantAdminRole})
	noSitesTenant := testMachineBuildTenant(t, dbSession, noSitesOrg, "vis-no-sites-tenant")
	noSitesOS := buildTenantOS(t, ctx, osDAO, noSitesOrg, noSitesTenant.ID, "tenant-os-no-sites", noSitesUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, noSitesOS.ID, siteD.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, noSitesUser)
	otherProvider := testMachineBuildInfrastructureProvider(t, dbSession, "vis-other-provider-org", "vis-other-provider")
	otherSite := testMachineBuildSite(t, dbSession, otherProvider, "vis-other-site", cdbm.SiteStatusRegistered)
	otherOS := buildProviderOS(t, ctx, osDAO, otherProvider.Org, otherProvider.ID, "other-provider-os", servingProviderUser.ID)
	common.TestBuildOperatingSystemSiteAssociation(t, dbSession, otherOS.ID, otherSite.ID, cutil.GetPtr("test"), cdbm.OperatingSystemSiteAssociationStatusSynced, servingProviderUser)
	for _, os := range []*cdbm.OperatingSystem{provC, provD, noSitesOS, otherOS} {
		// Exercise the site-associated template-based definitions.
		_, err := osDAO.Update(ctx, nil, cdbm.OperatingSystemUpdateInput{
			OperatingSystemId: os.ID,
			OsType:            cutil.GetPtr(cdbm.OperatingSystemTypeTemplatedIPXE),
		})
		require.NoError(t, err)
	}
	dualUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{servingProviderOrg}, []string{authz.ProviderAdminRole, authz.TenantAdminRole})
	dualTenant := testMachineBuildTenant(t, dbSession, servingProviderOrg, "vis-dual-tenant")
	testBuildTenantSiteAssociation(t, dbSession, servingProviderOrg, dualTenant.ID, siteC.ID, dualUser.ID)
	privOrg := "vis-privileged-org"
	privUser := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{privOrg}, []string{authz.TenantAdminRole})
	privTenant := testMachineBuildTenant(t, dbSession, privOrg, "vis-privileged-tenant")
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip2, &privTenant.ID, privOrg, cdbm.TenantAccountStatusReady, privUser)
	// Disabling targeted creation at C does not revoke ordinary membership.
	// D has no membership and is accessible only through the account default.
	cdbm.TestBuildTenantSite(t, dbSession, privTenant, siteC, &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)}, privUser)

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name           string
		reqOrgName     string
		user           *cdbm.User
		os             *cdbm.OperatingSystem
		expectedStatus int
		expectedSites  []string
	}{
		{
			name:           "provider admin sees all associations of provider-owned OS",
			reqOrgName:     servingProviderOrg,
			user:           servingProviderUser,
			os:             provC,
			expectedStatus: http.StatusOK,
			expectedSites:  []string{siteC.ID.String(), siteD.ID.String()},
		},
		{
			name:           "tenant admin sees only accessible associations of provider OS",
			reqOrgName:     tenantOrg,
			user:           tnUser,
			os:             provC,
			expectedStatus: http.StatusOK,
			expectedSites:  []string{siteC.ID.String()},
		},
		{
			name:           "dual-role admin retains provider access to all associations",
			reqOrgName:     servingProviderOrg,
			user:           dualUser,
			os:             provC,
			expectedStatus: http.StatusOK,
			expectedSites:  []string{siteC.ID.String(), siteD.ID.String()},
		},
		{
			name:           "tenant admin cannot read provider OS at inaccessible site",
			reqOrgName:     tenantOrg,
			user:           tnUser,
			os:             provD,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "privileged tenant combines ordinary membership and implicit site access",
			reqOrgName:     privOrg,
			user:           privUser,
			os:             provC,
			expectedStatus: http.StatusOK,
			expectedSites:  []string{siteC.ID.String(), siteD.ID.String()},
		},
		{
			name:           "privileged tenant can read OS only at an implicit site",
			reqOrgName:     privOrg,
			user:           privUser,
			os:             provD,
			expectedStatus: http.StatusOK,
			expectedSites:  []string{siteD.ID.String()},
		},
		{
			name:           "privileged tenant cannot read OS at another provider",
			reqOrgName:     privOrg,
			user:           privUser,
			os:             otherOS,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "tenant admin can read own OS",
			reqOrgName:     tenantOrg,
			user:           tnUser,
			os:             tnOS,
			expectedStatus: http.StatusOK,
		},
		{
			name:           "tenant with no authorized sites sees no associations on own OS",
			reqOrgName:     noSitesOrg,
			user:           noSitesUser,
			os:             noSitesOS,
			expectedStatus: http.StatusOK,
		},
		{
			name:           "provider admin cannot read tenant-owned OS",
			reqOrgName:     servingProviderOrg,
			user:           servingProviderUser,
			os:             tnOS,
			expectedStatus: http.StatusForbidden,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.os.ID.String())
			ec.Set("user", tc.user)

			reqCtx := context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(reqCtx))

			gh := GetOperatingSystemHandler{dbSession: dbSession, tc: tempClient, cfg: cfg}
			err := gh.Handle(ec)
			assert.Nil(t, err)
			require.Equal(t, tc.expectedStatus, rec.Code)
			if tc.expectedStatus == http.StatusOK {
				var response model.APIOperatingSystem
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				gotSites := make([]string, 0, len(response.SiteAssociations))
				for _, association := range response.SiteAssociations {
					require.NotNil(t, association.Site)
					gotSites = append(gotSites, association.Site.ID)
					assert.Equal(t, cdbm.OperatingSystemSiteAssociationStatusSynced, association.Status)
				}
				assert.ElementsMatch(t, tc.expectedSites, gotSites)
			}
		})
	}
}
