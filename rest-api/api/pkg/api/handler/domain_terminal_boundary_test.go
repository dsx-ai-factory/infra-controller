// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"net/http"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// Site/Temporal RPC is substituted; actual handler authorization, persistent
// reservation and HTTP projection use the disposable PostgreSQL fixture.
func TestDomainCreateTerminalCoreReply_RetainsOneReservedIdentity(t *testing.T) {
	for _, tc := range []struct {
		name, errorType string
		httpStatus      int
	}{
		{"invalid argument", swe.ErrTypeNICoInvalidArgument, http.StatusBadRequest},
		{"failed precondition", swe.ErrTypeNICoFailedPrecondition, http.StatusPreconditionFailed},
		{"already exists", swe.ErrTypeNICoAlreadyExists, http.StatusConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newDomainHandlerFixture(t, nil)
			// ResetModel drops migration indexes; restore the actual production
			// reservation uniqueness invariant before testing HTTP retries.
			_, err := f.dbSession.DB.ExecContext(t.Context(), `CREATE UNIQUE INDEX domain_owned_name_idx
                ON domain (tenant_id, site_id, lower(rtrim(hostname, '.')))
                WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
                AND controller_domain_id IS NOT NULL`)
			require.NoError(t, err)
			forwarded := f.expectCore(t, corev1.Forge_CreateDomain_FullMethodName, nil,
				tp.NewNonRetryableApplicationError("Core rejected reserved name", tc.errorType, errors.New("Core rejected reserved name")))
			request := model.APIDomainCreateRequest{Name: "reserved.example.com", SiteID: f.site.ID.String()}
			response := f.request(t, NewCreateDomainHandler(f.dbSession, f.scp).Handle, http.MethodPost, "/", "", request)
			require.Equal(t, tc.httpStatus, response.Code, response.Body.String())
			var coreRequest corev1.CreateDomainRequest
			require.NoError(t, protojson.Unmarshal(forwarded.RequestJSON, &coreRequest))
			require.NotEmpty(t, coreRequest.GetReservedId().GetValue())
			storedRows, _, err := cdbm.NewDomainDAO(f.dbSession).GetAll(t.Context(), nil, cdbm.DomainFilterInput{TenantIDs: []uuid.UUID{f.tenant.ID}, SiteIDs: []uuid.UUID{f.site.ID}}, cdbp.PageInput{Limit: util.GetPtr(cdbp.TotalLimit)}, nil)
			require.NoError(t, err)
			require.Len(t, storedRows, 1)
			stored := storedRows[0]
			require.Equal(t, cdbm.DomainStatusError, stored.Status)
			require.Equal(t, coreRequest.GetReservedId().GetValue(), stored.ControllerDomainID.String())
			retry := f.request(t, NewCreateDomainHandler(f.dbSession, f.scp).Handle, http.MethodPost, "/", "", request)
			require.Equal(t, http.StatusConflict, retry.Code, retry.Body.String())
			after, err := cdbm.NewDomainDAO(f.dbSession).GetByID(t.Context(), nil, stored.ID, nil)
			require.NoError(t, err)
			require.Equal(t, stored.ControllerDomainID, after.ControllerDomainID)
			f.siteClient.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
		})
	}
}
