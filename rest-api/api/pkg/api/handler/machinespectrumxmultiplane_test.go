// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestGetMachineSpectrumXMultiplaneHandler_Handle(t *testing.T) {
	for _, tc := range []struct {
		name  string
		setup func(*testing.T, *common.TestSetupProviderMachineHandlerFixture)
		code  int
	}{
		{"provider viewer", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			f.User = &cdbm.User{OrgData: cdbm.OrgData{f.Org: cdbm.Org{Name: f.Org, Roles: []string{authz.ProviderViewerRole}}}}
		}, http.StatusOK},
		{"missing user", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) { f.User = nil }, http.StatusInternalServerError},
		{"unknown machine", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) { f.MachineID = "missing" }, http.StatusNotFound},
		{"other provider", func(t *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			f.Org = "other-org"
			user := common.TestBuildUser(t, f.DBSession, "other-user", f.Org, []string{authz.ProviderAdminRole})
			common.TestBuildInfrastructureProvider(t, f.DBSession, "Other Provider", f.Org, user)
			f.User = user
		}, http.StatusNotFound},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.MachineSpxMultiplane{
				MachineId:  &corev1.MachineId{Id: "authoritative-machine"},
				Interfaces: []*corev1.MachineSpxMultiplaneInterface{{LogicalIfname: "eth_r0_p0", Revision: "site-revision"}},
			})
			tc.setup(t, &fixture)
			h := NewGetMachineSpectrumXMultiplaneHandler(fixture.DBSession, fixture.SiteClientPool)
			rec := fixture.Request(t, h.Handle, http.MethodGet, "/", nil, "")
			assert.Equal(t, tc.code, rec.Code, rec.Body.String())
			if tc.code != http.StatusOK {
				assert.Empty(t, fixture.ProxiedReq.FullMethod)
				return
			}
			assert.Equal(t, corev1.Forge_GetMachineSpxMultiplane_FullMethodName, fixture.ProxiedReq.FullMethod)
			assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)
			var request corev1.MachineSpxMultiplaneRequest
			require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &request))
			assert.Equal(t, fixture.MachineID, request.GetMachineId().GetId())
			var response model.APIMachineSpectrumXMultiplane
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			assert.Equal(t, "authoritative-machine", response.MachineID)
			assert.Equal(t, "site-revision", response.Interfaces[0].Revision)
			assert.Contains(t, rec.Body.String(), `"configuredMaintenanceMode":null`)
			assert.Contains(t, rec.Body.String(), `"dpuStatuses":[]`)
		})
	}
}

func TestUpdateMachineSpectrumXMultiplaneHandler_Handle(t *testing.T) {
	for _, tc := range []struct {
		name  string
		setup func(*testing.T, *common.TestSetupProviderMachineHandlerFixture)
		body  string
		code  int
	}{
		{"assigned machine with inclusive ranges", func(t *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			_, err := cdbm.NewMachineDAO(f.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{MachineID: f.MachineID, IsAssigned: cutil.GetPtr(true)})
			require.NoError(t, err)
		}, "", http.StatusOK},
		{"viewer cannot update", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			f.User = &cdbm.User{OrgData: cdbm.OrgData{f.Org: cdbm.Org{Name: f.Org, Roles: []string{authz.ProviderViewerRole}}}}
		}, "", http.StatusForbidden},
		{"foreign org", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) { f.Org = "other-org" }, "", http.StatusForbidden},
		{"unregistered site", func(t *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			_, err := cdbm.NewSiteDAO(f.DBSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: uuid.MustParse(f.SiteID), Status: cutil.GetPtr(cdbm.SiteStatusPending)})
			require.NoError(t, err)
		}, "", http.StatusBadRequest},
		{"invalid request never proxies", nil, `{"rail":0,"allRails":true,"plane":0,"maintenanceMode":true}`, http.StatusBadRequest},
		{"site feature disabled", func(_ *testing.T, f *common.TestSetupProviderMachineHandlerFixture) {
			run := &tmocks.WorkflowRun{}
			run.On("Get", mock.Anything, mock.Anything).Return(status.Error(codes.FailedPrecondition, "partial-multiplane maintenance is disabled for this site"))
			client := &tmocks.Client{}
			client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName,
				mock.MatchedBy(func(req grpcproxy.Request) bool { *f.ProxiedReq = req; return true })).Return(run, nil)
			f.SiteClientPool.IDClientMap[f.SiteID] = client
		}, "", http.StatusPreconditionFailed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.MachineSpxMultiplane{
				MachineId:  &corev1.MachineId{Id: "authoritative-machine"},
				Interfaces: []*corev1.MachineSpxMultiplaneInterface{{LogicalIfname: "eth_r0_p2", Revision: "saved-revision", CfgMaintenanceMode: cutil.GetPtr(false)}},
			})
			if tc.setup != nil {
				tc.setup(t, &fixture)
			}
			body := tc.body
			if body == "" {
				body = `{"railRange":{"start":0,"end":3},"planeRange":{"start":2,"end":7},"maintenanceMode":false,"reference":""}`
			}
			h := NewUpdateMachineSpectrumXMultiplaneHandler(fixture.DBSession, fixture.SiteClientPool)
			rec := fixture.Request(t, h.Handle, http.MethodPatch, "/", json.RawMessage(body), "")
			assert.Equal(t, tc.code, rec.Code, rec.Body.String())
			if tc.code != http.StatusOK && tc.code != http.StatusPreconditionFailed {
				assert.Empty(t, fixture.ProxiedReq.FullMethod)
				return
			}
			assert.Equal(t, corev1.Forge_UpdateMachineSpxMultiplaneMaintenance_FullMethodName, fixture.ProxiedReq.FullMethod)
			assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)
			var request corev1.MachineSpxMultiplaneMaintenanceUpdateRequest
			require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &request))
			assert.Equal(t, fixture.MachineID, request.GetMachineId().GetId())
			assert.Equal(t, uint32(0), request.GetRailRange().GetStart())
			assert.Equal(t, uint32(3), request.GetRailRange().GetEnd())
			assert.Equal(t, uint32(2), request.GetPlaneRange().GetStart())
			assert.Equal(t, uint32(7), request.GetPlaneRange().GetEnd())
			assert.Equal(t, cutil.GetPtr(false), request.MaintenanceMode)
			assert.Equal(t, cutil.GetPtr(""), request.Reference)
			if tc.code == http.StatusPreconditionFailed {
				assert.Contains(t, rec.Body.String(), "disabled for this site")
				return
			}
			var response model.APIMachineSpectrumXMultiplane
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			assert.Equal(t, "saved-revision", response.Interfaces[0].Revision)
			assert.Equal(t, cutil.GetPtr(false), response.Interfaces[0].ConfiguredMaintenanceMode)
		})
	}
}
