// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"fmt"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
	"net/http"
	"testing"
)

func TestNVOSFactoryResetHandler_Handle(t *testing.T) {
	fixture := newNVLinkDomainHandlerTestFixture(t)
	for _, tc := range []struct {
		name        string
		scope       resetScope
		invalidBody bool
		tenant      bool
		siteStatus  string
		status      int
	}{
		{name: "domain switches only", scope: resetDomain, status: http.StatusAccepted},
		{name: "tray external ID", scope: resetTray, status: http.StatusAccepted},
		{name: "missing site", scope: resetTray, invalidBody: true, status: http.StatusBadRequest},
		{name: "tenant forbidden", scope: resetDomain, tenant: true, status: http.StatusForbidden},
		{name: "unregistered site", scope: resetDomain, siteStatus: cdbm.SiteStatusPending, status: http.StatusBadRequest},
	} {
		t.Run(tc.name, func(t *testing.T) {
			fixture.setSiteStatus(t, cdbm.SiteStatusRegistered)
			if tc.siteStatus != "" {
				fixture.setSiteStatus(t, tc.siteStatus)
			}
			user := fixture.providerUser
			if tc.tenant {
				user = fixture.tenantUser
			}
			body := fmt.Sprintf(`{"siteId":%q}`, fixture.site.ID.String())
			if tc.invalidBody {
				body = `{}`
			}
			taskID := uuid.NewString()
			captured := fixture.installFlowReply(t, &flowv1.SubmitTaskResponse{TaskIds: []*flowv1.UUID{{Id: taskID}}})
			ec, rec := fixture.echoContext(t, "/reset", body, user, "target-1")
			ec.Request().Method = http.MethodPost
			handler := NVOSFactoryResetHandler{dbSession: fixture.dbSession, scp: fixture.scp, scope: tc.scope}
			require.NoError(t, handler.Handle(ec))
			require.Equal(t, tc.status, rec.Code, rec.Body.String())
			if tc.status != http.StatusAccepted {
				require.Empty(t, captured.request.FullMethod)
				return
			}
			require.JSONEq(t, fmt.Sprintf(`{"taskIds":[%q]}`, taskID), rec.Body.String())
			require.Equal(t, flowv1.Flow_ResetSwitches_FullMethodName, captured.request.FullMethod)
			var request flowv1.ResetSwitchesRequest
			require.NoError(t, protojson.Unmarshal(captured.request.RequestJSON, &request))
			if tc.scope == resetDomain {
				require.Len(t, request.TargetSpec.GetNvlDomains().Targets[0].ComponentTypes, 1)
			} else {
				require.Equal(t, "target-1", request.TargetSpec.GetComponents().Targets[0].GetExternal().Id)
			}
		})
	}
}
