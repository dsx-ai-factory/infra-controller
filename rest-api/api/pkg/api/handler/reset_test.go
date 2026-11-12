// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"fmt"
	"net/http"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestFactoryResetHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	for _, tc := range []struct {
		name         string
		scope        resetScope
		resetTarget  string
		invalidBody  bool
		omitTarget   bool
		foreignSite  bool
		missingUser  bool
		invalidTasks bool
		tenant       bool
		siteStatus   string
		status       int
	}{
		{name: "domain switches only", scope: resetDomain, status: http.StatusAccepted},
		{name: "domain SDN scope forwarded", scope: resetDomain, resetTarget: "sdn", status: http.StatusAccepted},
		{name: "tray external ID", scope: resetTray, status: http.StatusAccepted},
		{name: "foreign provider site", scope: resetTray, foreignSite: true, status: http.StatusForbidden},
		{name: "missing user", scope: resetTray, missingUser: true, status: http.StatusInternalServerError},
		{name: "invalid task reference", scope: resetTray, invalidTasks: true, status: http.StatusBadGateway},
		{name: "missing reset target", scope: resetTray, omitTarget: true, status: http.StatusBadRequest},
		{name: "missing site", scope: resetTray, invalidBody: true, status: http.StatusBadRequest},
		{name: "tenant forbidden", scope: resetDomain, tenant: true, status: http.StatusForbidden},
		{name: "unregistered site", scope: resetDomain, siteStatus: cdbm.SiteStatusPending, status: http.StatusBadRequest},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fixture.setSiteStatus(t, cdbm.SiteStatusRegistered)
			if tc.siteStatus != "" {
				fixture.setSiteStatus(t, tc.siteStatus)
			}
			if tc.foreignSite {
				otherProvider := &cdbm.InfrastructureProvider{ID: uuid.New(), Name: "other-provider", Org: "other-org"}
				_, err := fixture.dbSession.DB.NewInsert().Model(otherProvider).Exec(context.Background())
				require.NoError(t, err)
				_, err = fixture.dbSession.DB.NewUpdate().Model((*cdbm.Site)(nil)).Set("infrastructure_provider_id = ?", otherProvider.ID).Where("id = ?", fixture.site.ID).Exec(context.Background())
				require.NoError(t, err)
				defer func() {
					_, err := fixture.dbSession.DB.NewUpdate().Model((*cdbm.Site)(nil)).Set("infrastructure_provider_id = ?", fixture.site.InfrastructureProviderID).Where("id = ?", fixture.site.ID).Exec(context.Background())
					require.NoError(t, err)
				}()
			}
			user := fixture.providerUser
			if tc.tenant {
				user = fixture.tenantUser
			}
			resetTarget := tc.resetTarget
			if resetTarget == "" {
				resetTarget = "nvos"
			}
			body := fmt.Sprintf(`{"siteId":%q,"target":%q}`, fixture.site.ID.String(), resetTarget)
			if tc.omitTarget {
				body = fmt.Sprintf(`{"siteId":%q}`, fixture.site.ID.String())
			}
			if tc.invalidBody {
				body = `{"target":"nvos"}`
			}
			if tc.missingUser {
				user = nil
			}
			taskID := uuid.NewString()
			if tc.invalidTasks {
				taskID = "invalid"
			}
			captured := fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{TaskIds: []*flowv1.UUID{{Id: taskID}}})
			ec, rec := fixture.echoContext(t, "/factory-reset", body, user, "target-1")
			ec.Request().Method = http.MethodPost
			handler := FactoryResetHandler{dbSession: fixture.dbSession, scp: fixture.scp, scope: tc.scope}
			require.NoError(t, handler.Handle(ec))
			require.Equal(t, tc.status, rec.Code, rec.Body.String())
			if tc.status != http.StatusAccepted && !tc.invalidTasks {
				require.Empty(t, captured.request.FullMethod)
				return
			}
			if tc.invalidTasks {
				require.NotEmpty(t, captured.request.FullMethod)
				return
			}
			require.JSONEq(t, fmt.Sprintf(`{"taskIds":[%q]}`, taskID), rec.Body.String())
			require.Equal(t, flowv1.Flow_FactoryResetSwitches_FullMethodName, captured.request.FullMethod)
			var request flowv1.FactoryResetSwitchesRequest
			require.NoError(t, protojson.Unmarshal(captured.request.RequestJSON, &request))
			require.Equal(t, resetTarget, request.GetResetTarget())
			if tc.scope == resetDomain {
				require.Len(t, request.TargetSpec.GetNvlDomains().Targets[0].ComponentTypes, 1)
			} else {
				require.Equal(t, "target-1", request.TargetSpec.GetComponents().Targets[0].GetExternal().Id)
			}
		})
	}
}
