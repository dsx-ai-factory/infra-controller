// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestCreateOrUpdateHostFirmwareConfigHandler_returns201OnCreate(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{
		responseProto: hostFirmwareConfigProtoResponse(t, true),
	})

	rec, proxiedReq := fixture.put(t, validHostFirmwareUpsertRequest(fixture.siteID))
	assert.Equal(t, http.StatusCreated, rec.Code)

	var resp model.APIHostFirmwareConfig
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))

	assert.Equal(t, upsertHostFirmwareConfigMethod, proxiedReq.FullMethod)
	var coreReq corev1.UpsertHostFirmwareConfigRequest
	require.NoError(t, protojson.Unmarshal(proxiedReq.RequestJSON, &coreReq))
}

func TestCreateOrUpdateHostFirmwareConfigHandler_returns200OnUpdate(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{
		responseProto: hostFirmwareConfigProtoResponse(t, false),
	})

	rec, _ := fixture.put(t, validHostFirmwareUpsertRequest(fixture.siteID))
	assert.Equal(t, http.StatusOK, rec.Code)
}

func TestCreateOrUpdateHostFirmwareConfigHandler_rejectsInvalidRequest(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{})

	rec, _ := fixture.put(t, model.APIHostFirmwareConfigCreateOrUpdateRequest{
		Vendor: "Nvidia",
		Model:  "DGXH100",
	})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
}

func TestCreateOrUpdateHostFirmwareConfigHandler_rejectsNonProviderAdmin(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{
		roles: []string{authz.TenantAdminRole},
	})

	rec, _ := fixture.put(t, validHostFirmwareUpsertRequest(fixture.siteID))
	assert.Equal(t, http.StatusForbidden, rec.Code)
}

func TestCreateOrUpdateHostFirmwareConfigHandler_surfacesCoreGRPCError(t *testing.T) {
	grpcErr := status.Error(codes.InvalidArgument, "model is required")
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{
		getErr: tp.NewApplicationErrorWithCause("model is required", swe.ErrTypeNICoInvalidArgument, grpcErr),
	})

	rec, _ := fixture.put(t, validHostFirmwareUpsertRequest(fixture.siteID))
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Contains(t, rec.Body.String(), "model is required")
}

func TestDeleteHostFirmwareConfigHandler_success(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{})

	rec, proxiedReq := fixture.delete(t, model.APIHostFirmwareConfigDeleteRequest{
		SiteID: fixture.siteID,
		Vendor: "Nvidia",
		Model:  "DGXH100",
	})
	assert.Equal(t, http.StatusNoContent, rec.Code)
	assert.Empty(t, rec.Body.String())

	var coreReq corev1.DeleteHostFirmwareConfigRequest
	require.NoError(t, protojson.Unmarshal(proxiedReq.RequestJSON, &coreReq))
}

func TestDeleteHostFirmwareConfigHandler_rejectsInvalidRequest(t *testing.T) {
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{})

	rec, _ := fixture.delete(t, model.APIHostFirmwareConfigDeleteRequest{
		SiteID: fixture.siteID,
		Vendor: "Nvidia",
	})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
}

func TestDeleteHostFirmwareConfigHandler_surfacesCoreGRPCError(t *testing.T) {
	grpcErr := status.Error(codes.NotFound, "host firmware config not found")
	fixture := newHostFirmwareConfigHandlerFixture(t, hostFirmwareConfigHandlerFixtureOptions{
		getErr: tp.NewApplicationErrorWithCause("host firmware config not found", swe.ErrTypeNICoObjectNotFound, grpcErr),
	})

	rec, _ := fixture.delete(t, model.APIHostFirmwareConfigDeleteRequest{
		SiteID: fixture.siteID,
		Vendor: "Nvidia",
		Model:  "DGXH100",
	})
	assert.Equal(t, http.StatusNotFound, rec.Code)
	assert.Contains(t, rec.Body.String(), "host firmware config not found")
}

type hostFirmwareConfigHandlerFixtureOptions struct {
	roles         []string
	user          *cdbm.User
	getErr        error
	responseProto *corev1.HostFirmwareConfigResponse
}

type hostFirmwareConfigHandlerFixture struct {
	org                 string
	siteID              string
	user                *cdbm.User
	createUpdateHandler CreateOrUpdateHostFirmwareConfigHandler
	deleteHandler       DeleteHostFirmwareConfigHandler
	proxiedReq          *grpcproxy.Request
}

func newHostFirmwareConfigHandlerFixture(t *testing.T, opts hostFirmwareConfigHandlerFixtureOptions) hostFirmwareConfigHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	user := opts.user
	if user == nil {
		roles := opts.roles
		if roles == nil {
			roles = []string{authz.ProviderAdminRole}
		}
		user = common.TestBuildUser(t, dbSession, uuid.NewString(), org, roles)
	}
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	siteID := site.ID.String()

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	if opts.getErr != nil {
		wrun.On("Get", mock.Anything, mock.Anything).Return(opts.getErr)
	} else {
		var responseJSON []byte
		if opts.responseProto != nil {
			var err error
			responseJSON, err = protojson.Marshal(opts.responseProto)
			require.NoError(t, err)
		}
		wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
			out, ok := args.Get(1).(*grpcproxy.Response)
			require.True(t, ok)
			out.ResponseJSON = responseJSON
		}).Return(nil)
	}

	tsc := &tmocks.Client{}
	tsc.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			*proxiedReq = req
			return true
		}),
	).Return(wrun, nil)

	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc

	return hostFirmwareConfigHandlerFixture{
		org:                 org,
		siteID:              siteID,
		user:                user,
		createUpdateHandler: NewCreateOrUpdateHostFirmwareConfigHandler(dbSession, scp),
		deleteHandler:       NewDeleteHostFirmwareConfigHandler(dbSession, scp),
		proxiedReq:          proxiedReq,
	}
}

func (f hostFirmwareConfigHandlerFixture) put(t *testing.T, apiReq model.APIHostFirmwareConfigCreateOrUpdateRequest) (*httptest.ResponseRecorder, grpcproxy.Request) {
	t.Helper()
	rec := f.doRequest(t, http.MethodPut, apiReq, f.createUpdateHandler.Handle)
	return rec, *f.proxiedReq
}

func (f hostFirmwareConfigHandlerFixture) delete(t *testing.T, apiReq model.APIHostFirmwareConfigDeleteRequest) (*httptest.ResponseRecorder, grpcproxy.Request) {
	t.Helper()
	rec := f.doRequest(t, http.MethodDelete, apiReq, f.deleteHandler.Handle)
	return rec, *f.proxiedReq
}

func (f hostFirmwareConfigHandlerFixture) doRequest(t *testing.T, method string, apiReq any, handler func(echo.Context) error) *httptest.ResponseRecorder {
	t.Helper()

	body, err := json.Marshal(apiReq)
	require.NoError(t, err)

	e := echo.New()
	req := httptest.NewRequest(method, "/", strings.NewReader(string(body)))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, handler(ec))
	return rec
}

func validHostFirmwareUpsertRequest(siteID string) model.APIHostFirmwareConfigCreateOrUpdateRequest {
	explicitStart := true
	return model.APIHostFirmwareConfigCreateOrUpdateRequest{
		SiteID:              siteID,
		Vendor:              "Nvidia",
		Model:               "DGXH100",
		ExplicitStartNeeded: &explicitStart,
		Ordering:            []model.HostFirmwareComponentType{model.HostFirmwareComponentTypeCx7},
		Components: []model.APIHostFirmwareComponentConfig{{
			Type: model.HostFirmwareComponentTypeCx7,
			Firmware: []model.APIHostFirmwareVersionConfig{{
				Version: "28.47.2682",
				Default: true,
				Artifacts: []model.APIHostFirmwareArtifact{{
					URL: "https://firmware.example.invalid/28.47.2682/fw.bin",
				}},
			}},
		}},
	}
}

func hostFirmwareConfigProtoResponse(t *testing.T, created bool) *corev1.HostFirmwareConfigResponse {
	t.Helper()

	createdAt := time.Date(2026, 1, 1, 12, 0, 0, 0, time.UTC)
	updatedAt := createdAt
	if !created {
		updatedAt = createdAt.Add(time.Hour)
	}

	return &corev1.HostFirmwareConfigResponse{
		Vendor:              "Nvidia",
		Model:               "DGXH100",
		ExplicitStartNeeded: true,
		Ordering:            []corev1.HostFirmwareComponentType{corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7},
		CreatedAt:           timestamppb.New(createdAt),
		UpdatedAt:           timestamppb.New(updatedAt),
		Components: []*corev1.HostFirmwareComponentConfigResponse{{
			Type: corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7,
			Firmware: []*corev1.HostFirmwareVersionConfig{{
				Version: "28.47.2682",
				Default: true,
				Artifacts: []*corev1.HostFirmwareArtifact{{
					Url: "https://firmware.example.invalid/28.47.2682/fw.bin",
				}},
			}},
		}},
	}
}
