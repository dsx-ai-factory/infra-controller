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

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestCreateUEFICredentialHandlerProxiesCredential(t *testing.T) {
	for _, tc := range []struct {
		kind           model.UEFICredentialKind
		credentialType corev1.CredentialType
	}{
		{model.UEFICredentialKindHost, corev1.CredentialType_HostUefi},
		{model.UEFICredentialKindDPU, corev1.CredentialType_DpuUefi},
	} {
		t.Run(string(tc.kind), func(t *testing.T) {
			fixture := newUEFICredentialHandlerFixture(t)
			rec, proxiedReq := fixture.request(t, model.APIUEFICredentialRequest{
				SiteID:   fixture.siteID,
				Kind:     tc.kind,
				Password: "secret-password",
			})
			assert.Equal(t, http.StatusCreated, rec.Code)
			assert.Equal(t, createCredentialMethod, proxiedReq.FullMethod)
			assert.NotContains(t, string(proxiedReq.RequestJSON), "secret-password")
			assert.NotEmpty(t, proxiedReq.EncryptedSecrets)

			var coreReq corev1.CredentialCreationRequest
			require.NoError(t, protojson.Unmarshal(proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, tc.credentialType, coreReq.GetCredentialType())
			assert.Equal(t, grpcproxy.RedactedPlaceholder, coreReq.GetPassword())

			var resp model.APIUEFICredential
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			assert.Equal(t, fixture.siteID, resp.SiteID)
			assert.Equal(t, tc.kind, resp.Kind)
			assert.NotContains(t, rec.Body.String(), "password")
		})
	}
}

func TestCreateUEFICredentialHandlerRejectsInvalidRequest(t *testing.T) {
	fixture := newUEFICredentialHandlerFixture(t)
	rec, _ := fixture.request(t, model.APIUEFICredentialRequest{
		SiteID:   fixture.siteID,
		Kind:     model.UEFICredentialKind("invalid"),
		Password: "secret-password",
	})
	assert.Equal(t, http.StatusBadRequest, rec.Code)
}

type uefiCredentialHandlerFixture struct {
	org        string
	siteID     string
	user       interface{}
	handler    CreateUEFICredentialHandler
	proxiedReq *grpcproxy.Request
}

func newUEFICredentialHandlerFixture(t *testing.T) uefiCredentialHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, []string{authz.ProviderAdminRole})
	ip := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Return(nil)

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

	return uefiCredentialHandlerFixture{
		org:        org,
		siteID:     site.ID.String(),
		user:       user,
		handler:    NewCreateUEFICredentialHandler(dbSession, scp),
		proxiedReq: proxiedReq,
	}
}

func (f uefiCredentialHandlerFixture) request(t *testing.T, apiReq model.APIUEFICredentialRequest) (*httptest.ResponseRecorder, grpcproxy.Request) {
	t.Helper()

	body, err := json.Marshal(apiReq)
	require.NoError(t, err)

	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body)))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, f.handler.Handle(ec))
	return rec, *f.proxiedReq
}
