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
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	apim "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	enums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
)

func rackGroupValidationContext(method, body string) (echo.Context, *httptest.ResponseRecorder) {
	req := httptest.NewRequest(method, "/v2/org/test/nico/expected-rack-group", strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ctx := echo.New().NewContext(req, rec)
	ctx.Set("user", &cdbm.User{ID: uuid.New()})
	ctx.SetParamNames("orgName", "id")
	ctx.SetParamValues("test", "550e8400-e29b-41d4-a716-446655440000")
	return ctx, rec
}

func TestReplaceAllExpectedRackGroupsHandler_Handle(t *testing.T) {
	testRackGroupMutation(t, "ReplaceAllExpectedRackGroups")
	// Nil dependencies prove malformed replacement requests cannot reach DB or workflows.
	handler := NewReplaceAllExpectedRackGroupsHandler(nil, nil, nil)
	for _, body := range []string{
		`{"siteId":"550e8400-e29b-41d4-a716-446655440000"}`,
		`{"siteId":"550e8400-e29b-41d4-a716-446655440000","expectedRackGroups":null}`,
	} {
		t.Run(body, func(t *testing.T) {
			ctx, rec := rackGroupValidationContext(http.MethodPut, body)
			require.NoError(t, handler.Handle(ctx))
			require.Equal(t, http.StatusBadRequest, rec.Code)
			require.Contains(t, rec.Body.String(), "expectedRackGroups")
		})
	}
}

func TestCreateExpectedRackGroupHandler_Handle(t *testing.T) {
	testRackGroupMutation(t, "CreateExpectedRackGroup")
	ctx, rec := rackGroupValidationContext(http.MethodPost,
		`{"siteId":"550e8400-e29b-41d4-a716-446655440000","rackGroupId":"group","topology":"t","name":"机架"}`)
	require.NoError(t, NewCreateExpectedRackGroupHandler(nil, nil, nil).Handle(ctx))
	require.Equal(t, http.StatusBadRequest, rec.Code)
	require.Contains(t, rec.Body.String(), "name")
}

func TestUpdateExpectedRackGroupHandler_Handle(t *testing.T) {
	testRackGroupMutation(t, "UpdateExpectedRackGroup")
	for _, tc := range []struct {
		name, body, field string
	}{
		{"description too long", `{"description":"` + strings.Repeat("d", 1025) + `"}`, "description"},
		{"null labels alone", `{"labels":null}`, "body"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, rec := rackGroupValidationContext(http.MethodPatch, tc.body)
			require.NoError(t, NewUpdateExpectedRackGroupHandler(nil, nil, nil).Handle(ctx))
			require.Equal(t, http.StatusBadRequest, rec.Code)
			require.Contains(t, rec.Body.String(), tc.field)
		})
	}
}

func TestDeleteExpectedRackGroupHandler_Handle(t *testing.T) {
	testRackGroupMutation(t, "DeleteExpectedRackGroup")
}

func TestDeleteAllExpectedRackGroupsHandler_Handle(t *testing.T) {
	testRackGroupMutation(t, "DeleteAllExpectedRackGroups")
}

type rackGroupConcurrentInsertHook struct {
	insert func()
}

func (hook *rackGroupConcurrentInsertHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if hook.insert != nil && strings.HasPrefix(event.Query, "INSERT INTO \"expected_rack_group\"") {
		insert := hook.insert
		hook.insert = nil
		insert()
	}
	return ctx
}

func (*rackGroupConcurrentInsertHook) AfterQuery(context.Context, *bun.QueryEvent) {}

// Exercise each public mutation through its DB transaction and Temporal boundary.
func testRackGroupMutation(t *testing.T, workflowName string) {
	t.Helper()
	scenarios := []string{"success", "workflow failure", "request canceled", "deadline", "workflow timeout", "foreign site", "commit conflict"}
	if workflowName == "UpdateExpectedRackGroup" {
		scenarios = append(scenarios, "null labels")
	}
	for _, scenario := range scenarios {
		if scenario == "commit conflict" && workflowName != "CreateExpectedRackGroup" {
			continue
		}
		t.Run(scenario, func(t *testing.T) {
			session := testExpectedRackInitDB(t)
			t.Cleanup(func() { session.Close() })
			ctx := context.Background()
			require.NoError(t, session.DB.ResetModel(ctx, (*cdbm.ExpectedRackGroup)(nil)))
			_, site, _ := testExpectedRackSetupTestData(t, session, "test")
			user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("rack-group-user"), OrgData: cdbm.OrgData{"test": cdbm.Org{Name: "test", Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
			_, err := session.DB.NewInsert().Model(user).Exec(ctx)
			require.NoError(t, err)
			dao := cdbm.NewExpectedRackGroupDAO(session)
			original, err := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "original", Topology: "old", Racks: []cdbm.ExpectedRackGroupRack{{RackID: "rack", Members: []cdbm.ExpectedRackGroupMember{}}}, Name: "old-name", Labels: map[string]string{"key": "value"}, CreatedBy: user.ID})
			require.NoError(t, err)
			if scenario == "commit conflict" {
				_, err = session.DB.ExecContext(ctx, "ALTER TABLE expected_rack_group ADD CONSTRAINT expected_rack_group_group_id_site_id_key UNIQUE (rack_group_id, site_id) DEFERRABLE INITIALLY DEFERRED")
				require.NoError(t, err)
				session.DB.AddQueryHook(&rackGroupConcurrentInsertHook{insert: func() {
					_, insertErr := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "new", Topology: "concurrent", CreatedBy: user.ID})
					require.NoError(t, insertErr)
				}})
			}
			assertRolledBack := func() {
				var rows []cdbm.ExpectedRackGroup
				require.NoError(t, session.DB.NewSelect().Model(&rows).Scan(ctx))
				require.Len(t, rows, 1)
				require.Equal(t, original.ID, rows[0].ID)
				require.Equal(t, original.Topology, rows[0].Topology)
				require.Equal(t, original.Name, rows[0].Name)
				require.Equal(t, original.Racks, rows[0].Racks)
			}
			cfg := common.GetTestConfig()
			// The injected client needs no live Temporal TLS configuration.
			pool := sc.NewClientPool(nil)
			client := &tmocks.Client{}
			pool.IDClientMap[site.ID.String()] = client
			requestCtx, cancel := context.WithCancel(ctx)
			defer cancel()
			if scenario != "foreign site" {
				run := &tmocks.WorkflowRun{}
				var resultErr error
				switch scenario {
				case "workflow failure":
					resultErr = errors.New("injected workflow failure")
				case "request canceled":
					resultErr = context.Canceled
				case "deadline":
					resultErr = context.DeadlineExceeded
				case "workflow timeout":
					resultErr = temporal.NewTimeoutError(enums.TIMEOUT_TYPE_START_TO_CLOSE, nil)
				}
				run.On("Get", mock.Anything, mock.Anything).Run(func(mock.Arguments) {
					if scenario == "request canceled" {
						cancel()
					}
				}).Return(resultErr).Once()
				args := []interface{}{mock.Anything, mock.MatchedBy(func(options tclient.StartWorkflowOptions) bool {
					return options.WorkflowExecutionTimeout == grpcproxy.WorkflowExecutionTimeout && strings.HasPrefix(options.ID, "core-grpc-")
				}), "InvokeCoreGRPC", mock.Anything}
				client.On("ExecuteWorkflow", args...).Run(func(args mock.Arguments) {
					request := args.Get(3).(grpcproxy.Request)
					method := workflowName
					if method == "CreateExpectedRackGroup" {
						method = "AddExpectedRackGroup"
					}
					require.Equal(t, "/forge.Forge/"+method, request.FullMethod)
					require.Empty(t, request.EncryptedSecrets)
					if workflowName == "CreateExpectedRackGroup" {
						wire := &corev1.ExpectedRackGroup{}
						require.NoError(t, protojson.Unmarshal(request.RequestJSON, wire))
						require.Len(t, wire.Racks, 2)
						require.Equal(t, "rack-01", wire.Racks[0].GetRackId().GetId())
						require.Equal(t, "Switch", wire.Racks[0].Members[0].Type)
						require.Empty(t, wire.Racks[1].Members)
					}
					if workflowName == "UpdateExpectedRackGroup" {
						wire := &corev1.ExpectedRackGroup{}
						require.NoError(t, protojson.Unmarshal(request.RequestJSON, wire))
						require.Equal(t, "old", wire.Topology, "omitted topology must survive PATCH")
						require.Empty(t, wire.Metadata.Name)
						require.Empty(t, wire.Racks)
						if scenario == "null labels" {
							require.Len(t, wire.Metadata.Labels, 1)
							require.Equal(t, "key", wire.Metadata.Labels[0].GetKey())
							require.Equal(t, "value", wire.Metadata.Labels[0].GetValue())
						} else {
							require.Empty(t, wire.Metadata.Labels)
						}
					}
					if workflowName == "DeleteExpectedRackGroup" {
						wire := &corev1.ExpectedRackGroupRequest{}
						require.NoError(t, protojson.Unmarshal(request.RequestJSON, wire))
						require.Equal(t, original.RackGroupID, wire.RackGroupId)
					}
					if workflowName == "ReplaceAllExpectedRackGroups" {
						wire := &corev1.ExpectedRackGroupList{}
						require.NoError(t, protojson.Unmarshal(request.RequestJSON, wire))
						require.Empty(t, wire.ExpectedRackGroups)
					}
				}).Return(run, nil).Once()
				t.Cleanup(func() { client.AssertExpectations(t); run.AssertExpectations(t) })
			} else {
				user.OrgData = cdbm.OrgData{"other": cdbm.Org{Name: "other", Roles: []string{"FORGE_PROVIDER_ADMIN"}}}
				_, _, _ = testExpectedRackSetupTestData(t, session, "other")
			}
			method, body := http.MethodPost, fmt.Sprintf(`{"siteId":%q,"rackGroupId":"new","topology":"new","racks":[{"rackId":"rack-01","members":[{"type":"NVSwitch","manufacturer":"NVIDIA","id":"switch-01"}]},{"rackId":"rack-02","members":[]}]}`, site.ID.String())
			status := http.StatusCreated
			var handle func(echo.Context) error
			switch workflowName {
			case "CreateExpectedRackGroup":
				handle = NewCreateExpectedRackGroupHandler(session, pool, cfg).Handle
			case "UpdateExpectedRackGroup":
				method, body, status = http.MethodPatch, `{"name":"","racks":[],"labels":{}}`, http.StatusOK
				if scenario == "null labels" {
					body = `{"name":"","racks":[],"labels":null}`
				}
				handle = NewUpdateExpectedRackGroupHandler(session, pool, cfg).Handle
			case "DeleteExpectedRackGroup":
				method, body, status = http.MethodDelete, "", http.StatusNoContent
				handle = NewDeleteExpectedRackGroupHandler(session, pool, cfg).Handle
			case "ReplaceAllExpectedRackGroups":
				method, body, status = http.MethodPut, fmt.Sprintf(`{"siteId":%q,"expectedRackGroups":[]}`, site.ID.String()), http.StatusOK
				handle = NewReplaceAllExpectedRackGroupsHandler(session, pool, cfg).Handle
			case "DeleteAllExpectedRackGroups":
				method, body, status = http.MethodDelete, "", http.StatusNoContent
				handle = NewDeleteAllExpectedRackGroupsHandler(session, pool, cfg).Handle
			}
			c, rec := rackGroupValidationContext(method, body)
			c.SetRequest(c.Request().WithContext(requestCtx))
			c.Set("user", user)
			org := "test"
			if scenario == "foreign site" {
				org = "other"
			}
			c.SetParamValues(org, original.ID.String())
			query := c.Request().URL.Query()
			query.Set("siteId", site.ID.String())
			c.Request().URL.RawQuery = query.Encode()
			require.NoError(t, handle(c))
			if scenario == "foreign site" {
				require.Equal(t, http.StatusForbidden, rec.Code, rec.Body.String())
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
				assertRolledBack()
			} else if scenario == "commit conflict" {
				require.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
				rows, count, readErr := dao.GetAll(ctx, nil, cdbm.ExpectedRackGroupFilterInput{SiteIDs: []uuid.UUID{site.ID}, RackGroupIDs: []string{"new"}}, paginator.PageInput{}, nil)
				require.NoError(t, readErr)
				require.Equal(t, 1, count)
				require.Equal(t, "concurrent", rows[0].Topology)
			} else if scenario != "success" && scenario != "null labels" {
				wantStatus := http.StatusInternalServerError
				if scenario == "request canceled" || scenario == "workflow timeout" {
					wantStatus = http.StatusGatewayTimeout
				}
				require.Equal(t, wantStatus, rec.Code, rec.Body.String())
				assertRolledBack()
			} else {
				require.Equal(t, status, rec.Code, rec.Body.String())
				var rows []cdbm.ExpectedRackGroup
				require.NoError(t, session.DB.NewSelect().Model(&rows).Scan(ctx))
				switch workflowName {
				case "CreateExpectedRackGroup", "UpdateExpectedRackGroup":
					var response apim.APIExpectedRackGroup
					require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
					stored, err := dao.Get(ctx, nil, response.ID, nil, false)
					require.NoError(t, err)
					require.Equal(t, response.Topology, stored.Topology)
					require.Equal(t, response.Name, stored.Name)
					require.Equal(t, response.Racks, apim.NewAPIExpectedRackGroup(stored).Racks)
					if workflowName == "CreateExpectedRackGroup" {
						require.Len(t, rows, 2)
						require.Equal(t, "new", response.RackGroupID)
						require.Equal(t, "new", response.Topology)
						require.Len(t, response.Racks, 2)
						require.Equal(t, "NVSwitch", response.Racks[0].Members[0].Type)
						require.NotNil(t, response.Racks[1].Members)
						for _, list := range []bool{false, true} {
							readCtx, readRec := rackGroupValidationContext(http.MethodGet, "")
							readCtx.Set("user", user)
							readCtx.SetParamValues("test", response.ID.String())
							query := readCtx.Request().URL.Query()
							query.Set("siteId", site.ID.String())
							readCtx.Request().URL.RawQuery = query.Encode()
							var readErr error
							if list {
								readErr = NewGetAllExpectedRackGroupHandler(session, cfg).Handle(readCtx)
							} else {
								readErr = NewGetExpectedRackGroupHandler(session, cfg).Handle(readCtx)
							}
							require.NoError(t, readErr)
							require.Equal(t, http.StatusOK, readRec.Code, readRec.Body.String())
							require.NotContains(t, readRec.Body.String(), "\"rackIds\"")
							var readResponse apim.APIExpectedRackGroup
							if list {
								var items []apim.APIExpectedRackGroup
								require.NoError(t, json.Unmarshal(readRec.Body.Bytes(), &items))
								require.Len(t, items, 2)
								require.NotEmpty(t, readRec.Header().Get("X-Pagination"))
								for _, item := range items {
									if item.ID == response.ID {
										readResponse = item
									}
								}
							} else {
								require.NoError(t, json.Unmarshal(readRec.Body.Bytes(), &readResponse))
							}
							require.Equal(t, response.Racks, readResponse.Racks)
						}
					} else {
						require.Len(t, rows, 1)
						require.Equal(t, "original", response.RackGroupID)
						require.Equal(t, "old", response.Topology)
						require.Empty(t, response.Name)
						require.Empty(t, response.Racks)
						if scenario == "null labels" {
							require.Equal(t, apim.APILabels(original.Labels), response.Labels)
							require.Equal(t, original.Labels, stored.Labels)
						} else {
							require.Empty(t, response.Labels)
							require.Empty(t, stored.Labels)
						}
					}
				default:
					require.Empty(t, rows)
				}
			}
		})
	}
}

func TestGetAllExpectedRackGroupHandler_Handle(t *testing.T) {
	testRackGroupRead(t, true)
	ctx, rec := rackGroupValidationContext(http.MethodGet, "")
	query := ctx.Request().URL.Query()
	query.Set("siteId", "invalid-uuid")
	ctx.Request().URL.RawQuery = query.Encode()
	require.NoError(t, NewGetAllExpectedRackGroupHandler(nil, nil).Handle(ctx))
	require.Equal(t, http.StatusBadRequest, rec.Code)
	require.Contains(t, rec.Body.String(), "siteId")
}

func TestGetExpectedRackGroupHandler_Handle(t *testing.T) {
	testRackGroupRead(t, false)
}

func testRackGroupRead(t *testing.T, list bool) {
	t.Helper()
	session := testExpectedRackInitDB(t)
	t.Cleanup(func() { session.Close() })
	ctx := context.Background()
	require.NoError(t, session.DB.ResetModel(ctx, (*cdbm.ExpectedRackGroup)(nil)))
	_, site, _ := testExpectedRackSetupTestData(t, session, "test")
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("rack-group-reader"), OrgData: cdbm.OrgData{"test": cdbm.Org{Name: "test", Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := session.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	racks := []cdbm.ExpectedRackGroupRack{{RackID: "rack-01", Members: []cdbm.ExpectedRackGroupMember{{Type: cdbm.ExpectedRackGroupMemberTypeNVSwitch, Manufacturer: "NVIDIA", ID: "switch-01"}}}}
	dao := cdbm.NewExpectedRackGroupDAO(session)
	first, err := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-a", Topology: "topology", Racks: racks, CreatedBy: user.ID})
	require.NoError(t, err)
	_, err = dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-b", Topology: "topology", CreatedBy: user.ID})
	require.NoError(t, err)
	for _, tc := range []struct {
		name        string
		page        string
		missingUser bool
		wantStatus  int
		wantCount   int
	}{
		{name: "nested response", page: "1", wantStatus: http.StatusOK, wantCount: 1},
		{name: "empty page", page: "3", wantStatus: http.StatusOK, wantCount: 0},
		{name: "missing user", missingUser: true, wantStatus: http.StatusInternalServerError},
	} {
		if !list && tc.name == "empty page" {
			continue
		}
		t.Run(tc.name, func(t *testing.T) {
			request, rec := rackGroupValidationContext(http.MethodGet, "")
			request.SetParamValues("test", first.ID.String())
			if tc.missingUser {
				request.Set("user", nil)
			} else {
				request.Set("user", user)
			}
			if list {
				query := request.Request().URL.Query()
				query.Set("siteId", site.ID.String())
				query.Set("pageNumber", tc.page)
				query.Set("pageSize", "1")
				request.Request().URL.RawQuery = query.Encode()
				require.NoError(t, NewGetAllExpectedRackGroupHandler(session, common.GetTestConfig()).Handle(request))
			} else {
				require.NoError(t, NewGetExpectedRackGroupHandler(session, common.GetTestConfig()).Handle(request))
			}
			require.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantStatus != http.StatusOK {
				return
			}
			if list {
				var rows []apim.APIExpectedRackGroup
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &rows))
				require.Len(t, rows, tc.wantCount)
				require.NotEmpty(t, rec.Header().Get("X-Pagination"))
				if tc.wantCount == 0 {
					require.JSONEq(t, "[]", rec.Body.String())
					return
				}
				require.Equal(t, "group-a", rows[0].RackGroupID)
				require.Equal(t, apim.NewAPIExpectedRackGroup(first).Racks, rows[0].Racks)
			} else {
				var row apim.APIExpectedRackGroup
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &row))
				require.Equal(t, "group-a", row.RackGroupID)
				require.Equal(t, apim.NewAPIExpectedRackGroup(first).Racks, row.Racks)
			}
		})
	}
}
