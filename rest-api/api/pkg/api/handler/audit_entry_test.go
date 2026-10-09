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
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestGetAllAuditEntryHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// orgs
	org1 := "test-org-1"
	org2 := "test-org-2"

	// build users to execute GetAll
	adminUser1 := common.TestBuildUser(t, dbSession, "admin-1", org1, []string{authz.ProviderAdminRole})
	nonAdminUser1 := common.TestBuildUser(t, dbSession, "view-1", org1, []string{authz.ProviderViewerRole})
	adminUser2 := common.TestBuildUser(t, dbSession, "admin-2", org2, []string{authz.TenantAdminRole})
	nonAdminUser2 := common.TestBuildUser(t, dbSession, "user-2", org2, []string{"NICO_TENANT_USER"})

	// build users for audit entries
	org1user := common.TestBuildUser(t, dbSession, "org-1-user", org1, []string{authz.ProviderAdminRole})
	org2user := common.TestBuildUser(t, dbSession, "org-2-user", org2, []string{authz.TenantAdminRole})

	// build audit entries
	totalCount := 50
	var auditEntries []cdbm.AuditEntry
	for i := 0; i < totalCount; i++ {
		var ae *cdbm.AuditEntry
		if i%2 == 0 {
			if i%4 == 0 {
				ae = common.TestBuildAuditEntry(t, dbSession, org1, &org1user.ID, http.StatusCreated)
			} else {
				ae = common.TestBuildAuditEntry(t, dbSession, org1, &org1user.ID, http.StatusBadRequest)
			}
		} else {
			ae = common.TestBuildAuditEntry(t, dbSession, org2, &org2user.ID, http.StatusCreated)
		}
		auditEntries = append(auditEntries, *ae)
	}

	// Setup echo server/context
	e := echo.New()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
	}

	type args struct {
		org   string
		query url.Values
		user  *cdbm.User
	}

	tests := []struct {
		name           string
		fields         fields
		args           args
		wantCount      int
		wantTotalCount int
		wantRespCode   int
	}{
		{
			name: "get all for provider org using admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: adminUser1,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 25,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all for tenant org using admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				user: adminUser2,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 25,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all failed for provider org using admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: adminUser1,
				query: url.Values{
					"failedOnly": {"true"},
				},
			},
			wantCount:      12,
			wantTotalCount: 12,
			wantRespCode:   http.StatusOK,
		},
		{
			name: "get all for provider org using wrong admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: adminUser2,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 0,
			wantRespCode:   http.StatusForbidden,
		},
		{
			name: "get all for tenant org using wrong admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				user: adminUser1,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 0,
			wantRespCode:   http.StatusForbidden,
		},
		{
			name: "get all for provider org using non-admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: nonAdminUser1,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 0,
			wantRespCode:   http.StatusForbidden,
		},
		{
			name: "get all for tenant org using non-admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				user: nonAdminUser2,
			},
			wantCount:      paginator.DefaultLimit,
			wantTotalCount: 0,
			wantRespCode:   http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gaaeh := GetAllAuditEntryHandler{
				dbSession: tt.fields.dbSession,
			}

			path := ""
			if tt.args.query != nil {
				path = fmt.Sprintf("/v2/org/%s/nico/audit?%s", tt.args.org, tt.args.query.Encode())
			} else {
				path = fmt.Sprintf("/v2/org/%s/nico/audit", tt.args.org)
			}

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)
			ec.Set("user", tt.args.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err := gaaeh.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, tt.wantRespCode, rec.Code)

			if tt.wantRespCode != http.StatusOK {
				return
			}

			var resp []model.APIAuditEntry

			if err = json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
				t.Fatal(err)
			}

			assert.Equal(t, tt.wantCount, len(resp))

			for _, ae := range resp {
				assert.Equal(t, tt.args.org, ae.OrgName)
			}
		})
	}
}

func TestGetAuditEntryHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := common.TestInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// orgs
	org1 := "test-org-1"
	org2 := "test-org-2"

	// build users to execute GetAll
	adminUser1 := common.TestBuildUser(t, dbSession, "admin-1", org1, []string{authz.ProviderAdminRole})
	nonAdminUser1 := common.TestBuildUser(t, dbSession, "view-1", org1, []string{authz.ProviderViewerRole})
	adminUser2 := common.TestBuildUser(t, dbSession, "admin-2", org2, []string{authz.TenantAdminRole})
	nonAdminUser2 := common.TestBuildUser(t, dbSession, "user-2", org2, []string{"NICO_TENANT_USER"})

	// build users for audit entries
	org1user := common.TestBuildUser(t, dbSession, "org-1-user", org1, []string{authz.ProviderAdminRole})
	org2user := common.TestBuildUser(t, dbSession, "org-2-user", org2, []string{authz.TenantAdminRole})

	// build audit entries
	totalCount := 10
	var auditEntries []cdbm.AuditEntry
	for i := 0; i < totalCount; i++ {
		var ae *cdbm.AuditEntry
		if i%2 == 0 {
			ae = common.TestBuildAuditEntry(t, dbSession, org1, &org1user.ID, http.StatusCreated)
		} else {
			ae = common.TestBuildAuditEntry(t, dbSession, org2, &org2user.ID, http.StatusCreated)
		}
		auditEntries = append(auditEntries, *ae)
	}

	// Setup echo server/context
	e := echo.New()

	// OTEL Spanner configuration
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
	}

	type args struct {
		org  string
		id   string
		user *cdbm.User
	}

	tests := []struct {
		name         string
		fields       fields
		args         args
		wantID       string
		wantRespCode int
	}{
		{
			name: "get from provider org using admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				id:   auditEntries[0].ID.String(),
				user: adminUser1,
			},
			wantID:       auditEntries[0].ID.String(),
			wantRespCode: http.StatusOK,
		},
		{
			name: "get from tenant org using admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				id:   auditEntries[1].ID.String(),
				user: adminUser2,
			},
			wantID:       auditEntries[1].ID.String(),
			wantRespCode: http.StatusOK,
		},
		{
			name: "get from provider org using wrong admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: adminUser2,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get from tenant org using wrong admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				user: adminUser1,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get from provider org using non-admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org1,
				user: nonAdminUser1,
			},
			wantRespCode: http.StatusForbidden,
		},
		{
			name: "get from tenant org using non-admin user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				org:  org2,
				user: nonAdminUser2,
			},
			wantRespCode: http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			handler := GetAuditEntryHandler{
				dbSession: tt.fields.dbSession,
			}

			path := fmt.Sprintf("/v2/org/%s/nico/audit/%s", tt.args.org, tt.args.id)

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.org, tt.args.id)
			ec.Set("user", tt.args.user)

			ec.SetRequest(ec.Request().WithContext(ctx))

			err := handler.Handle(ec)
			require.NoError(t, err)
			require.Equal(t, tt.wantRespCode, rec.Code)

			if tt.wantRespCode != http.StatusOK {
				return
			}

			var resp model.APIAuditEntry
			if err = json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
				t.Fatal(err)
			}

			assert.Equal(t, tt.wantID, resp.ID)
		})
	}
}
