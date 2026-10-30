// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestPutSiteLeakagePolicyHandler_Handle(t *testing.T) {
	e := echo.New()
	dbSession := testRackInitDB(t)
	defer dbSession.Close()

	cfg := common.GetTestConfig()
	temporalConfig, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(temporalConfig)

	org := "test-org"
	_, site, _ := testRackSetupTestData(t, dbSession, org)
	providerUser := testRackBuildUser(
		t,
		dbSession,
		"provider-user-leakage-policy-put",
		org,
		[]string{authz.ProviderAdminRole},
	)

	mockTemporalClient := &tmocks.Client{}
	mockRun := &tmocks.WorkflowRun{}
	mockRun.On("GetID").Return("test-workflow-id")
	testFlowProxyReply(t, mockRun, &flowv1.EventRule{
		EventType: leakageEventType,
		Enabled:   true,
		Actions: (&model.APIPutLeakagePolicyRequest{
			Action: model.LeakageActionNoAction,
		}).EventRuleActions(),
	})
	testFlowProxyDispatch(
		t,
		mockTemporalClient,
		mockRun,
		flowv1.Flow_SetEventRuleOverride_FullMethodName,
		nil,
	)
	scp.IDClientMap[site.ID.String()] = mockTemporalClient

	handler := NewPutSiteLeakagePolicyHandler(dbSession, scp, cfg)
	request := httptest.NewRequest(
		http.MethodPut,
		"/v2/org/"+org+"/nico/leakage/policy?siteId="+site.ID.String(),
		strings.NewReader(`{"action":"NoAction","target":null,"onConflict":null}`),
	)
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	recorder := httptest.NewRecorder()
	echoContext := e.NewContext(request, recorder)
	echoContext.SetParamNames("orgName")
	echoContext.SetParamValues(org)
	echoContext.Set("user", providerUser)

	require.NoError(t, handler.Handle(echoContext))
	require.Equal(t, http.StatusOK, recorder.Code, "body=%s", recorder.Body.String())
}

func TestDecodeLeakagePolicyRequest(t *testing.T) {
	tests := []struct {
		name    string
		body    string
		want    model.APIPutLeakagePolicyRequest
		wantErr bool
	}{
		{
			name: "force power off",
			body: `{"action":"ForcePowerOff","target":"AffectedComponents","onConflict":"Queue"}`,
			want: model.APIPutLeakagePolicyRequest{
				Action:     model.LeakageActionForcePowerOff,
				Target:     stringPointer(model.LeakageTargetAffected),
				OnConflict: stringPointer(model.LeakageConflictQueue),
			},
		},
		{
			name: "explicit no action nulls",
			body: `{"action":"NoAction","target":null,"onConflict":null}`,
			want: model.APIPutLeakagePolicyRequest{Action: model.LeakageActionNoAction},
		},
		{
			name:    "omitted nullable field",
			body:    `{"action":"NoAction","target":null}`,
			wantErr: true,
		},
		{
			name:    "unknown field",
			body:    `{"action":"NoAction","target":null,"onConflict":null,"enabled":false}`,
			wantErr: true,
		},
		{
			name:    "trailing JSON",
			body:    `{"action":"NoAction","target":null,"onConflict":null} {}`,
			wantErr: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			e := echo.New()
			request := httptest.NewRequest("PUT", "/", strings.NewReader(test.body))
			var got model.APIPutLeakagePolicyRequest
			err := decodeLeakagePolicyRequest(e.NewContext(request, httptest.NewRecorder()), &got)
			if test.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.want, got)
		})
	}
}

func TestLeakageHandlingWorkflowID(t *testing.T) {
	defaultLimit := int32(1)
	differentLimit := int32(2)
	base := model.APILeakageHandlingQuery{
		SiteID:     "site-a",
		SourceType: model.LeakageHandlingSourceTypeComponent,
		SourceID:   "component-a",
	}
	baseWorkflowID := leakageHandlingWorkflowID(base)

	tests := []struct {
		name     string
		query    model.APILeakageHandlingQuery
		wantSame bool
	}{
		{
			name: "site is excluded",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-b",
				SourceType: model.LeakageHandlingSourceTypeComponent,
				SourceID:   "component-a",
			},
			wantSame: true,
		},
		{
			name: "default limit is normalized",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-a",
				SourceType: model.LeakageHandlingSourceTypeComponent,
				SourceID:   "component-a",
				Limit:      &defaultLimit,
			},
			wantSame: true,
		},
		{
			name: "resource type identifies request",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-a",
				SourceType: model.LeakageHandlingSourceTypeRack,
				SourceID:   "component-a",
			},
		},
		{
			name: "resource ID identifies request",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-a",
				SourceType: model.LeakageHandlingSourceTypeComponent,
				SourceID:   "component-b",
			},
		},
		{
			name: "limit identifies request",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-a",
				SourceType: model.LeakageHandlingSourceTypeComponent,
				SourceID:   "component-a",
				Limit:      &differentLimit,
			},
		},
		{
			name: "cursor identifies request",
			query: model.APILeakageHandlingQuery{
				SiteID:     "site-a",
				SourceType: model.LeakageHandlingSourceTypeComponent,
				SourceID:   "component-a",
				Cursor:     "next-page",
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			workflowID := leakageHandlingWorkflowID(test.query)
			if test.wantSame {
				require.Equal(t, baseWorkflowID, workflowID)
				return
			}
			require.NotEqual(t, baseWorkflowID, workflowID)
		})
	}
}

func stringPointer(value string) *string { return &value }
