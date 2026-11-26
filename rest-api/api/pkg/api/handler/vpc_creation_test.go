// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"go.temporal.io/api/serviceerror"
	temporalclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

type vpcCreationQueryHook struct {
	before          func(context.Context, *bun.QueryEvent)
	after           func(context.Context, *bun.QueryEvent)
	contextForQuery func(context.Context, *bun.QueryEvent) context.Context
}

func (h *vpcCreationQueryHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if h.before != nil {
		h.before(ctx, event)
	}
	if h.contextForQuery != nil {
		return h.contextForQuery(ctx, event)
	}
	return ctx
}

func (h *vpcCreationQueryHook) AfterQuery(ctx context.Context, event *bun.QueryEvent) {
	if h.after != nil {
		h.after(ctx, event)
	}
}

type recordingVpcDPS struct {
	dpsclient.PowerProvisioner
	mu                 sync.Mutex
	created, deleted   []string
	onCreate           func()
	createErr          error
	cleanupErr         error
	deleteErr          error
	cleanupHasDeadline bool
	owners             map[string]int64
}

func (d *recordingVpcDPS) CreateResourceGroup(_ context.Context, name string, externalID int64) error {
	d.mu.Lock()
	d.created = append(d.created, name)
	if d.owners != nil && d.createErr == nil {
		d.owners[name] = externalID
	}
	d.mu.Unlock()
	if d.createErr != nil {
		return d.createErr
	}
	if d.onCreate != nil {
		d.onCreate()
	}
	return nil
}

func (d *recordingVpcDPS) DeleteResourceGroup(ctx context.Context, name string) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.deleted = append(d.deleted, name)
	d.cleanupErr = ctx.Err()
	_, d.cleanupHasDeadline = ctx.Deadline()
	if d.cleanupErr == nil {
		delete(d.owners, name)
	}
	if d.deleteErr != nil {
		return d.deleteErr
	}
	return d.cleanupErr
}

type vpcCreationFixture struct {
	db       *cdb.Session
	user     *cdbm.User
	tenant   *cdbm.Tenant
	provider *cdbm.InfrastructureProvider
	site     *cdbm.Site
	client   *tmocks.Client
	pool     *sc.ClientPool
	cfg      *config.Config
}

func newVpcCreationFixture(t *testing.T) *vpcCreationFixture {
	t.Helper()
	session := testVPCInitDB(t)
	t.Cleanup(session.Close)
	common.TestSetupSchema(t, session)
	owner := testVPCBuildUser(t, session, uuid.NewString(), "correction-provider", []string{authz.ProviderAdminRole})
	provider := testVPCSiteBuildInfrastructureProvider(t, session, "correction-provider", "correction-provider", owner)
	user := testVPCBuildUser(t, session, uuid.NewString(), "correction-tenant", []string{authz.TenantAdminRole})
	tenant := testVPCBuildTenant(t, session, "correction-tenant", "correction-tenant", user)
	site := testVPCBuildSite(t, session, provider, "correction-site", true, false, cdbm.SiteStatusRegistered, owner)
	_, err := cdbm.NewSiteDAO(session).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: site.ID, Config: &cdbm.SiteConfigUpdateInput{DPSPowerManagement: cutil.GetPtr(true)}})
	require.NoError(t, err)
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, session, provider, &tenant.ID, tenant.Org, cdbm.TenantAccountStatusReady, user)
	testVPCSiteBuildAllocation(t, session, site, tenant, "correction-allocation", owner)
	testBuildTenantSiteAssociation(t, session, tenant.Org, tenant.ID, site.ID, user.ID)
	client := &tmocks.Client{}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	t.Setenv(strings.ToUpper(config.ConfigDPSEnabled), "true")
	cfg := common.GetTestConfig()
	require.True(t, cfg.GetDPSEnabled())
	return &vpcCreationFixture{db: session, user: user, tenant: tenant, provider: provider, site: site, client: client, pool: pool, cfg: cfg}
}

func (f *vpcCreationFixture) context(ctx context.Context, method, id string, body []byte) (echo.Context, *httptest.ResponseRecorder) {
	request := httptest.NewRequest(method, "/", strings.NewReader(string(body))).WithContext(ctx)
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	response := httptest.NewRecorder()
	c := echo.New().NewContext(request, response)
	c.SetParamNames("orgName", "id")
	c.SetParamValues(f.tenant.Org, id)
	c.Set("user", f.user)
	return c, response
}

type vpcCreationRequestKey struct{}

func testVpcCreationLifecycle(t *testing.T) {
	for _, tc := range []struct {
		name        string
		createErr   error
		code        int
		compensate  bool
		dpsDisabled bool
	}{
		{name: "canceled save compensates confirmed DPS creation", code: http.StatusInternalServerError, compensate: true},
		{name: "existing DPS group is never compensated", createErr: dpsclient.ErrResourceGroupAlreadyExists, code: http.StatusConflict},
		// A saved group name must mean this request actually provisioned it.
		{name: "disabled REST DPS rejects the group before persistence", dpsDisabled: true, code: http.StatusBadRequest},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newVpcCreationFixture(t)
			if tc.dpsDisabled {
				t.Setenv(strings.ToUpper(config.ConfigDPSEnabled), "false")
				require.False(t, f.cfg.GetDPSEnabled())
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			dps := &recordingVpcDPS{onCreate: cancel, createErr: tc.createErr}
			id := uuid.New()
			body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: "canceled-save", SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("canceled-group")})
			require.NoError(t, err)
			c, response := f.context(ctx, http.MethodPost, "", body)
			require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c))
			assert.Equal(t, tc.code, response.Code)
			if tc.compensate {
				assert.Equal(t, []string{"canceled-group"}, dps.deleted)
				assert.NoError(t, dps.cleanupErr)
				assert.True(t, dps.cleanupHasDeadline)
			} else {
				assert.Empty(t, dps.deleted)
			}
			_, err = cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			assert.Empty(t, f.client.Calls)
			if tc.dpsDisabled {
				assert.Empty(t, dps.created)
			}
		})
	}
	t.Run("competing inserts preserve the winner's dependency", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		for _, tc := range []struct {
			name            string
			sharedGroup     bool
			winnerReadFails bool
		}{
			{name: "loser group is unowned"},
			{name: "winner owns the same group", sharedGroup: true},
			{name: "unreadable winner retains the group", winnerReadFails: true},
		} {
			t.Run(tc.name, func(t *testing.T) {
				id := uuid.New()
				ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
				defer cancel()
				arrived := make(chan struct{})
				committed := make(chan struct{})
				var mu sync.Mutex
				count := 0
				insertFailed := false
				f.db.DB.AddQueryHook(&vpcCreationQueryHook{
					before: func(ctx context.Context, event *bun.QueryEvent) {
						if !strings.HasPrefix(event.Query, `INSERT INTO "vpc"`) || !strings.Contains(event.Query, id.String()) {
							return
						}
						mu.Lock()
						count++
						if count == 2 {
							close(arrived)
						}
						mu.Unlock()
						select {
						case <-arrived:
						case <-ctx.Done():
							return
						}
						if ctx.Value(vpcCreationRequestKey{}) == "loser-"+id.String() {
							select {
							case <-committed:
							case <-ctx.Done():
							}
						}
					},
					after: func(ctx context.Context, event *bun.QueryEvent) {
						if strings.HasPrefix(event.Query, `INSERT INTO "vpc"`) && strings.Contains(event.Query, id.String()) && event.Err != nil {
							mu.Lock()
							insertFailed = true
							mu.Unlock()
						}
						if event.Query == "COMMIT" && event.Err == nil && ctx.Value(vpcCreationRequestKey{}) == "winner-"+id.String() {
							close(committed)
						}
					},
					contextForQuery: func(ctx context.Context, event *bun.QueryEvent) context.Context {
						mu.Lock()
						failed := insertFailed
						mu.Unlock()
						if tc.winnerReadFails && failed && ctx.Value(vpcCreationRequestKey{}) == "loser-"+id.String() && event.Operation() == "SELECT" && strings.Contains(event.Query, `"vpc"`) {
							readCtx, cancel := context.WithCancel(ctx)
							cancel()
							return readCtx
						}
						return ctx
					},
				})
				dps := &recordingVpcDPS{}
				client := &tmocks.Client{}
				run := &tmocks.WorkflowRun{}
				run.On("Get", mock.Anything, mock.Anything).Return(temporal.NewNonRetryableApplicationError("reply lost", swe.ErrTypeNICoUnavailable, nil)).Once()
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVPCV2", mock.Anything).Return(run, nil).Once()
				f.pool.IDClientMap[f.site.ID.String()] = client
				winner := model.APIVpcCreateRequest{ID: &id, Name: "owner-" + id.String(), SiteID: f.site.ID.String()}
				loser := model.APIVpcCreateRequest{ID: &id, Name: "contender-" + id.String(), SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("group-" + id.String())}
				if tc.sharedGroup {
					winner.PowerResourceGroup = loser.PowerResourceGroup
				}
				requests := []model.APIVpcCreateRequest{winner, loser}
				type result struct {
					code int
					err  error
					body string
				}
				results := make(chan result, 2)
				var workers sync.WaitGroup
				defer func() {
					// Failed assertions must stop and join both handlers before the
					// next case adds hooks to their shared database.
					cancel()
					workers.Wait()
				}()
				for i, request := range requests {
					body, err := json.Marshal(request)
					require.NoError(t, err)
					name := "winner-" + id.String()
					if i == 1 {
						name = "loser-" + id.String()
					}
					requestCtx := context.WithValue(ctx, vpcCreationRequestKey{}, name)
					workers.Add(1)
					go func() {
						defer workers.Done()
						c, response := f.context(requestCtx, http.MethodPost, "", body)
						err := NewCreateVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c)
						results <- result{response.Code, err, response.Body.String()}
					}()
				}
				codes := []int{}
				for range 2 {
					select {
					case r := <-results:
						require.NoError(t, r.err)
						codes = append(codes, r.code)
						if r.code == http.StatusConflict {
							assert.Contains(t, r.body, "A VPC with specified ID already exists")
							assert.Contains(t, r.body, `"id":"`+id.String()+`"`)
						}
					case <-ctx.Done():
						t.Fatal("competing inserts did not finish")
					}
				}
				assert.ElementsMatch(t, []int{http.StatusConflict, http.StatusServiceUnavailable}, codes)
				saved, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
				require.NoError(t, err)
				assert.Equal(t, winner.Name, saved.Name)
				assert.Equal(t, winner.PowerResourceGroup, saved.PowerResourceGroup)
				if tc.sharedGroup || tc.winnerReadFails {
					assert.Empty(t, dps.deleted)
				} else {
					assert.Equal(t, []string{*loser.PowerResourceGroup}, dps.deleted)
				}
				client.AssertExpectations(t)
				run.AssertExpectations(t)
			})
		}
	})
	t.Run("start failures and positive non-dispatch retain their distinct decisions", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		cases := []struct {
			name                   string
			startErr               error
			cancelAfterSave        bool
			rejectSaveFails        bool
			mismatch               bool
			code                   int
			state                  string
			cancelAtStart          bool
			cancelAtGet            bool
			completedBeforeTimeout bool
			rejectedBeforeResult   bool
		}{
			{name: "ambiguous start", startErr: errors.New("transport lost start reply"), code: 500, state: cdbm.VpcStatusPending},
			{name: "start deadline", startErr: serviceerror.NewDeadlineExceeded("start reply timed out"), code: 504, state: cdbm.VpcStatusPending},
			{name: "existing execution", startErr: serviceerror.NewWorkflowExecutionAlreadyStarted("exists", "request", "run"), code: 409, state: cdbm.VpcStatusPending},
			{name: "known not dispatched", cancelAfterSave: true, code: 504, state: cdbm.VpcStatusError},
			{name: "non-dispatch decision cannot be saved", cancelAfterSave: true, rejectSaveFails: true, code: 504, state: cdbm.VpcStatusPending},
			{name: "completion conflicts", mismatch: true, code: 409, state: cdbm.VpcStatusPending},
			{name: "expired start context maps a different error to timeout", startErr: errors.New("start transport stopped"), cancelAtStart: true, code: 504, state: cdbm.VpcStatusPending},
			{name: "expired result context retains Pending", cancelAtGet: true, code: 504, state: cdbm.VpcStatusPending},
			// Inventory can complete creation before the waiting POST learns the outcome.
			{name: "inventory completed before result timeout", cancelAtGet: true, completedBeforeTimeout: true, code: 504, state: cdbm.VpcStatusReady},
			// Cancellation can finish before a delayed successful create reply arrives.
			{name: "rejection precedes successful result", rejectedBeforeResult: true, code: 409, state: cdbm.VpcStatusError},
		}
		for _, tc := range cases {
			t.Run(tc.name, func(t *testing.T) {
				id := uuid.New()
				if tc.rejectSaveFails {
					_, err := f.db.DB.ExecContext(context.Background(), `ALTER TABLE status_detail ADD CONSTRAINT vpc_non_dispatch_history_failure CHECK (entity_id <> ? OR status <> 'Error')`, id.String())
					require.NoError(t, err)
					t.Cleanup(func() {
						_, err := f.db.DB.ExecContext(context.Background(), `ALTER TABLE status_detail DROP CONSTRAINT vpc_non_dispatch_history_failure`)
						assert.NoError(t, err)
					})
				}
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				if tc.cancelAfterSave {
					f.db.DB.AddQueryHook(&vpcCreationQueryHook{after: func(ctx context.Context, event *bun.QueryEvent) {
						if event.Query == "COMMIT" && event.Err == nil && ctx.Value(vpcCreationRequestKey{}) == id {
							cancel()
						}
					}})
				}
				ctx = context.WithValue(ctx, vpcCreationRequestKey{}, id)
				client := &tmocks.Client{}
				if !tc.cancelAfterSave {
					run := &tmocks.WorkflowRun{}
					client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVPCV2", mock.Anything).Run(func(mock.Arguments) {
						if tc.cancelAtStart {
							cancel()
						}
					}).Return(run, tc.startErr).Once()
					if tc.cancelAtGet {
						run.On("Get", mock.Anything, mock.Anything).Run(func(mock.Arguments) {
							if tc.completedBeforeTimeout {
								dao := cdbm.NewVpcDAO(f.db)
								saved, err := dao.GetByID(context.Background(), nil, id, nil)
								require.NoError(t, err)
								reported := saved.ToProto()
								reported.Status = &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(50003))}
								_, err = dao.CompleteCreation(context.Background(), id, reported)
								require.NoError(t, err)
							}
							cancel()
						}).Return(errors.New("result transport stopped")).Once()
					}
					if tc.mismatch || tc.rejectedBeforeResult {
						run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
							saved, err := cdbm.NewVpcDAO(f.db).GetByID(args.Get(0).(context.Context), nil, id, nil)
							require.NoError(t, err)
							result := args.Get(1).(*corev1.Vpc)
							proto.Merge(result, saved.ToProto())
							if tc.mismatch {
								result.Metadata.Name = "different original input"
							} else {
								_, err = cdbm.NewVpcDAO(f.db).RejectCreation(context.Background(), id)
								require.NoError(t, err)
							}
						}).Return(nil).Once()
					}
					t.Cleanup(func() { run.AssertExpectations(t) })
				}
				f.pool.IDClientMap[f.site.ID.String()] = client
				dps := &recordingVpcDPS{}
				body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: tc.name, SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("group-" + id.String())})
				require.NoError(t, err)
				c, response := f.context(ctx, http.MethodPost, "", body)
				require.NoError(t, NewCreateVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c))
				assert.Equal(t, tc.code, response.Code, response.Body.String())
				assert.Contains(t, response.Body.String(), id.String())
				if tc.completedBeforeTimeout {
					assert.Contains(t, response.Body.String(), "inspect VPC")
					assert.NotContains(t, response.Body.String(), "remains Pending")
				}
				saved, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
				require.NoError(t, err)
				assert.Equal(t, tc.state, saved.Status)
				assert.Equal(t, tc.name, saved.Name)
				assert.Equal(t, (tc.cancelAfterSave && !tc.rejectSaveFails) || tc.rejectedBeforeResult, saved.CreationRejectedAt != nil)
				assert.Empty(t, dps.deleted)
				if tc.cancelAfterSave && !tc.rejectSaveFails {
					c, deleted := f.context(context.Background(), http.MethodDelete, id.String(), nil)
					require.NoError(t, NewDeleteVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c))
					assert.Equal(t, http.StatusAccepted, deleted.Code, deleted.Body.String())
					assert.Equal(t, []string{"group-" + id.String()}, dps.deleted)
					assert.Empty(t, client.Calls)
				}
				client.AssertExpectations(t)
				client.AssertNotCalled(t, "TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			})
		}
	})
	t.Run("cancellation rolls back before commit and compensates DPS", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		id := uuid.New()
		ctx, cancel := context.WithCancel(context.WithValue(context.Background(), vpcCreationRequestKey{}, true))
		defer cancel()
		var commitErr error
		f.db.DB.AddQueryHook(&vpcCreationQueryHook{
			before: func(queryCtx context.Context, event *bun.QueryEvent) {
				if queryCtx.Value(vpcCreationRequestKey{}) != true || event.Operation() != "COMMIT" {
					return
				}
				// Let database/sql finish its automatic rollback before Commit is
				// called. This is a definite abort, not a lost COMMIT acknowledgement.
				cancel()
				require.Eventually(t, func() bool {
					var active int
					err := f.db.DB.NewRaw("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND state = 'idle in transaction'").Scan(context.Background(), &active)
					return err == nil && active == 0
				}, 5*time.Second, time.Millisecond)
			},
			after: func(queryCtx context.Context, event *bun.QueryEvent) {
				if queryCtx.Value(vpcCreationRequestKey{}) == true && event.Operation() == "COMMIT" {
					commitErr = event.Err
				}
			},
		})
		dps := &recordingVpcDPS{}
		body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: "rolled-back-save", SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("rolled-back-group")})
		require.NoError(t, err)
		c, response := f.context(ctx, http.MethodPost, "", body)
		require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c))
		require.ErrorIs(t, commitErr, sql.ErrTxDone)
		assert.Equal(t, http.StatusInternalServerError, response.Code)
		assert.Equal(t, []string{"rolled-back-group"}, dps.deleted)
		assert.NoError(t, dps.cleanupErr)
		assert.True(t, dps.cleanupHasDeadline)
		_, err = cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		assert.Empty(t, f.client.Calls)
	})
	t.Run("constraint-rejected commit compensates an unowned DPS group", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		id := uuid.New()
		// This deferred constraint reaches the COMMIT error boundary. It does not
		// simulate a database that committed before losing its acknowledgement.
		_, err := f.db.DB.ExecContext(context.Background(), `ALTER TABLE vpc ADD CONSTRAINT vpc_test_deferred_commit FOREIGN KEY (id) REFERENCES tenant (id) DEFERRABLE INITIALLY DEFERRED`)
		require.NoError(t, err)
		t.Cleanup(func() {
			_, err := f.db.DB.ExecContext(context.Background(), `ALTER TABLE vpc DROP CONSTRAINT vpc_test_deferred_commit`)
			assert.NoError(t, err)
		})
		dps := &recordingVpcDPS{}
		body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: "commit-failure", SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("commit-group")})
		require.NoError(t, err)
		c, response := f.context(context.Background(), http.MethodPost, "", body)
		require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c))
		assert.Equal(t, http.StatusInternalServerError, response.Code)
		assert.Equal(t, []string{"commit-group"}, dps.created)
		assert.Equal(t, []string{"commit-group"}, dps.deleted)
		_, err = cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		assert.Empty(t, f.client.Calls)
	})
	for _, deleteBeforeHistory := range []bool{
		false, // A presentation failure must not undo the committed creation.
		true,  // Deletion between response reads must not mix two database snapshots.
	} {
		name := "history presentation failure preserves committed success"
		if deleteBeforeHistory {
			name = "concurrent deletion preserves response snapshot"
		}
		t.Run(name, func(t *testing.T) {
			f := newVpcCreationFixture(t)
			id := uuid.New()
			historyReached := false
			f.db.DB.AddQueryHook(&vpcCreationQueryHook{contextForQuery: func(ctx context.Context, event *bun.QueryEvent) context.Context {
				if !historyReached && event.Operation() == "SELECT" && strings.Contains(event.Query, `"status_detail"`) && strings.Contains(event.Query, id.String()) {
					historyReached = true
					if deleteBeforeHistory {
						// The VPC read already established the response snapshot. Commit
						// the same row/history changes as deletion before history is read.
						err := cdb.WithTx(context.Background(), f.db, func(tx *cdb.Tx) error {
							_, err := cdbm.NewVpcDAO(f.db).Update(context.Background(), tx, cdbm.VpcUpdateInput{VpcID: id, Status: cutil.GetPtr(cdbm.VpcStatusDeleting)})
							if err != nil {
								return err
							}
							_, err = cdbm.NewStatusDetailDAO(f.db).Create(context.Background(), tx, cdbm.StatusDetailCreateInput{EntityID: id.String(), Status: cdbm.VpcStatusDeleting})
							return err
						})
						require.NoError(t, err)
						return ctx
					}
					readCtx, cancel := context.WithCancel(ctx)
					cancel()
					return readCtx
				}
				return ctx
			}})
			run := &tmocks.WorkflowRun{}
			f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVPCV2", mock.Anything).Return(run, nil).Once()
			run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
				saved, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
				require.NoError(t, err)
				proto.Merge(args.Get(1).(*corev1.Vpc), saved.ToProto())
				args.Get(1).(*corev1.Vpc).Status = &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(50001))}
			}).Return(nil).Once()
			body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: "history-presentation", SiteID: f.site.ID.String()})
			require.NoError(t, err)
			c, response := f.context(context.Background(), http.MethodPost, "", body)
			require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, nil).Handle(c))
			assert.True(t, historyReached)
			assert.Equal(t, http.StatusCreated, response.Code, response.Body.String())
			var returned model.APIVpc
			require.NoError(t, json.Unmarshal(response.Body.Bytes(), &returned))
			assert.Equal(t, id.String(), returned.ID)
			assert.NotNil(t, returned.StatusHistory)
			assert.Equal(t, cdbm.VpcStatusReady, returned.Status)
			if deleteBeforeHistory {
				require.Len(t, returned.StatusHistory, 2)
				for _, detail := range returned.StatusHistory {
					assert.NotEqual(t, cdbm.VpcStatusDeleting, detail.Status)
				}
			} else {
				assert.Empty(t, returned.StatusHistory)
			}
			persisted, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
			require.NoError(t, err)
			expectedStatus, historyCount := cdbm.VpcStatusReady, 2
			if deleteBeforeHistory {
				expectedStatus, historyCount = cdbm.VpcStatusDeleting, 3
			}
			assert.Equal(t, expectedStatus, persisted.Status)
			history, _, err := cdbm.NewStatusDetailDAO(f.db).GetAll(context.Background(), nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{id.String()}}, cdbp.PageInput{})
			require.NoError(t, err)
			assert.Len(t, history, historyCount)
			f.client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
	t.Run("persistence and workflow share the handler deadline", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		id := uuid.New()
		initialSaved := false
		var persistenceDeadline time.Time
		committed := false
		f.db.DB.AddQueryHook(&vpcCreationQueryHook{before: func(ctx context.Context, event *bun.QueryEvent) {
			if strings.HasPrefix(event.Query, `INSERT INTO "vpc"`) && strings.Contains(event.Query, id.String()) {
				deadline, hasDeadline := ctx.Deadline()
				require.True(t, hasDeadline)
				persistenceDeadline = deadline
				initialSaved = true
			}
		}, after: func(ctx context.Context, event *bun.QueryEvent) {
			if event.Query == "COMMIT" && event.Err == nil {
				committed = true
			}
		}})
		run := &tmocks.WorkflowRun{}
		var dispatchDeadline time.Time
		f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVPCV2", mock.Anything).Run(func(args mock.Arguments) {
			assert.True(t, initialSaved)
			deadline, hasDeadline := args.Get(0).(context.Context).Deadline()
			require.True(t, hasDeadline)
			dispatchDeadline = deadline
			require.True(t, committed)
			assert.Equal(t, persistenceDeadline, deadline)
			assert.Equal(t, cutil.WorkflowExecutionTimeout, args.Get(1).(temporalclient.StartWorkflowOptions).WorkflowExecutionTimeout)
		}).Return(run, nil).Once()
		run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
			deadline, _ := args.Get(0).(context.Context).Deadline()
			assert.Equal(t, dispatchDeadline, deadline)
		}).Return(temporal.NewNonRetryableApplicationError("reply lost", swe.ErrTypeNICoUnavailable, nil)).Once()
		body, err := json.Marshal(model.APIVpcCreateRequest{ID: &id, Name: "dispatch-clock", SiteID: f.site.ID.String()})
		require.NoError(t, err)
		c, response := f.context(context.Background(), http.MethodPost, "", body)
		require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, nil).Handle(c))
		assert.Equal(t, http.StatusServiceUnavailable, response.Code)
		f.client.AssertExpectations(t)
		run.AssertExpectations(t)
	})
	t.Run("soft-deleted explicit identity fails before DPS or dispatch", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		saved := testVPCBuildVPC(t, f.db, "deleted identity", f.provider, f.tenant, f.site, nil, nil, nil, cdbm.VpcStatusReady, f.user)
		_, err := f.db.DB.NewDelete().Model(saved).WherePK().Exec(context.Background())
		require.NoError(t, err)
		dps := &recordingVpcDPS{}
		body, err := json.Marshal(model.APIVpcCreateRequest{ID: &saved.ID, Name: "new name", SiteID: f.site.ID.String(), PowerResourceGroup: cutil.GetPtr("unused-group")})
		require.NoError(t, err)
		c, response := f.context(context.Background(), http.MethodPost, "", body)
		require.NoError(t, NewCreateVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c))
		assert.Equal(t, http.StatusConflict, response.Code)
		assert.Contains(t, response.Body.String(), "A VPC with specified ID already exists")
		assert.Contains(t, response.Body.String(), `"id":"`+saved.ID.String()+`"`)
		assert.Empty(t, dps.created)
		assert.Empty(t, f.client.Calls)
	})
}

// testVpcDeletionCreationDecisions keeps dependencies until Core confirms the
// committed VPC or completed cancellation, then checks the appropriate deletion path.
func testVpcDeletionCreationDecisions(t *testing.T) {
	f := newVpcCreationFixture(t)
	cases := []struct {
		name                   string
		status                 string
		rejectFirst            bool
		removedBeforeRejection bool
		cancelled              bool
		missingReply           bool
		mismatch               bool
		coreFailure            string
		outage                 error
		failHistory            bool
		siteReplyAtDeadline    bool
		wantCode               int
	}{
		// A saved cancellation decision no longer needs the creation workflow.
		{name: "persisted cancellation needs no Site lookup", status: cdbm.VpcStatusPending, rejectFirst: true, wantCode: http.StatusAccepted},
		// Imported errors still use the ordinary Core deletion path.
		{name: "imported Error needs no creation history", status: cdbm.VpcStatusError, wantCode: http.StatusAccepted},
		// Unfinished provider cleanup must keep the VPC and its dependencies.
		{name: "unfinished cancellation cannot release dependencies", status: cdbm.VpcStatusPending, coreFailure: swe.ErrTypeNICoFailedPrecondition, wantCode: http.StatusPreconditionFailed},
		// A transport outage is not confirmation that cancellation completed.
		{name: "Temporal outage retains Pending", status: cdbm.VpcStatusPending, outage: serviceerror.NewUnavailable("Temporal unavailable"), wantCode: http.StatusInternalServerError},
		// The proxy must preserve its existing timeout response.
		{name: "Temporal deadline retains Pending", status: cdbm.VpcStatusPending, outage: serviceerror.NewDeadlineExceeded("Temporal request expired"), wantCode: http.StatusGatewayTimeout},
		// A Core outage is equally inconclusive, even if creation history expired.
		{name: "Core unavailable retains Pending", status: cdbm.VpcStatusPending, coreFailure: swe.ErrTypeNICoUnavailable, wantCode: http.StatusServiceUnavailable},
		// A Core 404 is not the positive cancellation result required for cleanup.
		{name: "Core absence retains Pending", status: cdbm.VpcStatusPending, coreFailure: swe.ErrTypeNICoObjectNotFound, wantCode: http.StatusNotFound},
		// The Core timeout must retain dependencies while preserving HTTP 504.
		{name: "Core deadline retains Pending", status: cdbm.VpcStatusPending, coreFailure: swe.ErrTypeNICoDeadlineExceeded, wantCode: http.StatusGatewayTimeout},
		// A different VPC with this ID is not ours to adopt or delete.
		{name: "conflicting Core VPC requires operator repair", status: cdbm.VpcStatusPending, mismatch: true, wantCode: http.StatusConflict},
		// Creation can win the race; validate its result before ordinary deletion.
		{name: "committed Core VPC continues deletion", status: cdbm.VpcStatusPending, wantCode: http.StatusAccepted},
		// A confirmed Site result must still commit after the Site wait budget ends.
		{name: "confirmed Site deletion leaves time to commit", status: cdbm.VpcStatusPending, siteReplyAtDeadline: true, wantCode: http.StatusAccepted},
		// Core confirms absence and fences late creation before local cleanup.
		{name: "confirmed cancellation permits local deletion", status: cdbm.VpcStatusPending, cancelled: true, wantCode: http.StatusAccepted},
		// Another DELETE can finish while this request waits for Core's reply.
		{name: "concurrent cancellation already removed the request", status: cdbm.VpcStatusPending, cancelled: true, removedBeforeRejection: true, wantCode: http.StatusNotFound},
		// An empty proxy response cannot stand in for Core's cancellation acknowledgement.
		{name: "missing cancellation result retains dependencies", status: cdbm.VpcStatusPending, missingReply: true, wantCode: http.StatusInternalServerError},
		// Failed local persistence must prevent dependency cleanup in both branches.
		{name: "cancellation must persist before cleanup", status: cdbm.VpcStatusPending, cancelled: true, failHistory: true, wantCode: http.StatusInternalServerError},
		{name: "completion database failure is not a conflict", status: cdbm.VpcStatusPending, failHistory: true, wantCode: http.StatusInternalServerError},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			saved := testVPCBuildVPC(t, f.db, tc.name, f.provider, f.tenant, f.site, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, nil, tc.status, f.user)
			saved.ControllerVpcID = &saved.ID
			saved.PowerResourceGroup = cutil.GetPtr("delete-group-" + saved.ID.String())
			testUpdateVPC(t, f.db, saved)
			if tc.rejectFirst {
				var err error
				saved, err = cdbm.NewVpcDAO(f.db).RejectCreation(ctx, saved.ID)
				require.NoError(t, err)
				require.NotNil(t, saved.CreationRejectedAt)
				update, updateResponse := f.context(ctx, http.MethodPatch, saved.ID.String(), []byte(`{"name":"must not change Core"}`))
				require.NoError(t, NewUpdateVPCHandler(f.db, f.client, f.pool, f.cfg, nil).Handle(update))
				assert.Equal(t, http.StatusConflict, updateResponse.Code, updateResponse.Body.String())
				virtualization, virtualizationResponse := f.context(ctx, http.MethodPatch, saved.ID.String(), []byte(`{"networkVirtualizationType":"FNN"}`))
				require.NoError(t, NewUpdateVPCVirtualizationHandler(f.db, f.client, f.pool, f.cfg).Handle(virtualization))
				assert.Equal(t, http.StatusConflict, virtualizationResponse.Code, virtualizationResponse.Body.String())
				assert.Empty(t, f.client.Calls)
			}
			client := &tmocks.Client{}
			if saved.Status == cdbm.VpcStatusPending {
				if tc.outage != nil {
					client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).Return(nil, tc.outage).Once()
				} else {
					result := &corev1.CancelVpcCreationResponse{}
					if tc.cancelled {
						result.Result = &corev1.CancelVpcCreationResponse_Cancelled{Cancelled: &emptypb.Empty{}}
					} else if !tc.missingReply {
						reported := saved.ToProto()
						if tc.mismatch {
							reported.Metadata.Name = "another creation"
						}
						result.Result = &corev1.CancelVpcCreationResponse_CreatedVpc{CreatedVpc: reported}
					}
					mockVpcCreationCancellation(t, client, saved, result, tc.coreFailure)
				}
			}
			if tc.failHistory {
				_, err := f.db.DB.ExecContext(ctx, `ALTER TABLE status_detail ADD CONSTRAINT vpc_delete_history_failure CHECK (entity_id <> ?)`, saved.ID.String())
				require.NoError(t, err)
				t.Cleanup(func() {
					_, err := f.db.DB.ExecContext(ctx, `ALTER TABLE status_detail DROP CONSTRAINT vpc_delete_history_failure`)
					assert.NoError(t, err)
				})
			}
			if tc.outage == nil && tc.coreFailure == "" && !tc.failHistory && !tc.missingReply && !tc.mismatch && !tc.rejectFirst && !tc.cancelled {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("vpc-delete-" + saved.ID.String()).Once()
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVPCV2", mock.Anything).Return(run, nil).Once()
				if tc.siteReplyAtDeadline {
					run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
						// Deliver success at the Site deadline so sharing that context
						// with the DB transaction would roll back its Deleting state.
						<-args.Get(0).(context.Context).Done()
					}).Return(nil).Once()
				} else {
					run.On("Get", mock.Anything, mock.Anything).Return(temporal.NewNonRetryableApplicationError("already absent", swe.ErrTypeNICoObjectNotFound, nil)).Once()
				}
				t.Cleanup(func() { run.AssertExpectations(t) })
			}
			f.pool.IDClientMap[f.site.ID.String()] = client
			dps := &recordingVpcDPS{}
			deleteCtx := ctx
			if tc.siteReplyAtDeadline {
				var cancel context.CancelFunc
				deleteCtx, cancel = context.WithTimeout(ctx, 3*time.Second)
				defer cancel()
			}
			if tc.rejectFirst {
				_, err := f.db.DB.ExecContext(ctx, `ALTER TABLE vpc ADD CONSTRAINT vpc_rejected_delete_failure CHECK (id <> ? OR deleted IS NULL)`, saved.ID)
				require.NoError(t, err)
				c, dbFailure := f.context(ctx, http.MethodDelete, saved.ID.String(), nil)
				require.NoError(t, NewDeleteVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c))
				assert.Equal(t, http.StatusInternalServerError, dbFailure.Code)
				assert.Empty(t, dps.deleted)
				retained, err := cdbm.NewVpcDAO(f.db).GetByID(ctx, nil, saved.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, saved.CreationRejectedAt, retained.CreationRejectedAt)
				_, err = f.db.DB.ExecContext(ctx, `ALTER TABLE vpc DROP CONSTRAINT vpc_rejected_delete_failure`)
				require.NoError(t, err)
				// Confirmed cancellation can be cleaned up after the Site goes offline.
				_, err = f.db.DB.NewUpdate().Model(f.site).Set("status = ?", cdbm.SiteStatusPending).WherePK().Exec(ctx)
				require.NoError(t, err)
				delete(f.pool.IDClientMap, f.site.ID.String())
				t.Cleanup(func() {
					_, err := f.db.DB.NewUpdate().Model(f.site).Set("status = ?", cdbm.SiteStatusRegistered).WherePK().Exec(ctx)
					assert.NoError(t, err)
				})
				dps.deleteErr = errors.New("DPS cleanup reply unavailable")
				var cancelRequest context.CancelFunc
				deleteCtx, cancelRequest = context.WithCancel(ctx)
				t.Cleanup(cancelRequest)
				f.db.DB.AddQueryHook(&vpcCreationQueryHook{after: func(_ context.Context, event *bun.QueryEvent) {
					if event.Err == nil && strings.Contains(event.Query, "creation_rejected_at IS NOT NULL") && strings.Contains(event.Query, saved.ID.String()) {
						cancelRequest()
					}
				}})
			}
			removed := false
			if tc.removedBeforeRejection {
				// Advance the competing delete after our initial read, but before
				// RejectCreation locks the row. Its own queries bypass this hook.
				f.db.DB.AddQueryHook(&vpcCreationQueryHook{before: func(queryCtx context.Context, event *bun.QueryEvent) {
					if removed || !strings.Contains(event.Query, "FOR UPDATE") || !strings.Contains(event.Query, saved.ID.String()) {
						return
					}
					removed = true
					dao := cdbm.NewVpcDAO(f.db)
					_, err := dao.RejectCreation(queryCtx, saved.ID)
					require.NoError(t, err)
					deleted, err := dao.DeleteRejectedCreation(queryCtx, saved.ID)
					require.NoError(t, err)
					require.True(t, deleted)
				}})
			}
			c, response := f.context(deleteCtx, http.MethodDelete, saved.ID.String(), nil)
			require.NoError(t, NewDeleteVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c))
			assert.Equal(t, tc.wantCode, response.Code, response.Body.String())
			if tc.failHistory {
				assert.Equal(t, http.StatusInternalServerError, response.Code, response.Body.String())
				assert.Empty(t, dps.deleted)
			} else if tc.removedBeforeRejection {
				assert.True(t, removed, "the competing delete must precede rejection")
				assert.Empty(t, dps.deleted, "the losing request must not repeat dependency cleanup")
			} else if tc.coreFailure != "" || tc.outage != nil || tc.missingReply || tc.mismatch {
				assert.Empty(t, dps.deleted)
			} else {
				assert.Equal(t, http.StatusAccepted, response.Code, response.Body.String())
				if tc.rejectFirst {
					assert.Equal(t, []string{*saved.PowerResourceGroup}, dps.deleted)
					assert.True(t, dps.cleanupHasDeadline)
					assert.NoError(t, dps.cleanupErr)
					assert.Empty(t, client.Calls)
					retry, retryResponse := f.context(ctx, http.MethodDelete, saved.ID.String(), nil)
					require.NoError(t, NewDeleteVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(retry))
					assert.Equal(t, http.StatusNotFound, retryResponse.Code)
					assert.Len(t, dps.deleted, 1)
				} else {
					assert.Equal(t, []string{*saved.PowerResourceGroup}, dps.deleted)
				}

			}
			rows, _, err := cdbm.NewVpcDAO(f.db).GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{saved.ID}, IncludeDeleted: true}, cdbp.PageInput{}, nil)
			require.NoError(t, err)
			require.Len(t, rows, 1)
			actual := rows[0]
			if tc.rejectFirst || (tc.cancelled && !tc.failHistory) {
				assert.NotNil(t, actual.Deleted)
				assert.NotNil(t, actual.CreationRejectedAt)
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVPCV2", mock.Anything)
			} else if tc.coreFailure != "" || tc.outage != nil || tc.failHistory || tc.missingReply || tc.mismatch {
				assert.Equal(t, cdbm.VpcStatusPending, actual.Status)
				assert.Nil(t, actual.CreationRejectedAt)
				assert.Nil(t, actual.Deleted)
				assert.Equal(t, saved.PowerResourceGroup, actual.PowerResourceGroup)
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVPCV2", mock.Anything)
			} else {
				assert.Equal(t, cdbm.VpcStatusDeleting, actual.Status)
				assert.Nil(t, actual.CreationRejectedAt)
			}
			// When creation won, settling it and ordinary deletion must not each
			// receive a fresh handler budget. Inspect both actual dispatch contexts.
			if tc.name == "committed Core VPC continues deletion" {
				var deadlines []time.Time
				for _, call := range client.Calls {
					if call.Method == "ExecuteWorkflow" {
						deadline, ok := call.Arguments.Get(0).(context.Context).Deadline()
						require.True(t, ok)
						deadlines = append(deadlines, deadline)
					}
				}
				require.Len(t, deadlines, 2)
				assert.Equal(t, deadlines[0], deadlines[1])
			}
			client.AssertNotCalled(t, "DescribeWorkflowExecution", mock.Anything, mock.Anything, mock.Anything)
			client.AssertNotCalled(t, "GetWorkflowHistory", mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			client.AssertExpectations(t)
		})
	}
	t.Run("only the committed deletion winner can clean up DPS", testVpcRejectedCleanupWinner)
}

// testVpcRejectedCleanupWinner keeps a delayed DELETE from cleaning up a replacement DPS group.
func testVpcRejectedCleanupWinner(t *testing.T) {
	f := newVpcCreationFixture(t)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	saved := testVPCBuildVPC(t, f.db, "cleanup-owner", f.provider, f.tenant, f.site, nil, nil, nil, cdbm.VpcStatusPending, f.user)
	saved.PowerResourceGroup = cutil.GetPtr("cleanup-group")
	testUpdateVPC(t, f.db, saved)
	_, err := cdbm.NewVpcDAO(f.db).RejectCreation(ctx, saved.ID)
	require.NoError(t, err)
	dps := &recordingVpcDPS{owners: map[string]int64{*saved.PowerResourceGroup: 1}}
	reached := make(chan struct{}, 2)
	winnerRelease, staleRelease := make(chan struct{}), make(chan struct{})
	releaseWinner := sync.OnceFunc(func() { close(winnerRelease) })
	releaseStale := sync.OnceFunc(func() { close(staleRelease) })
	var workers sync.WaitGroup
	defer func() {
		// Join both handlers before fixture cleanup closes the database, even
		// when a failed assertion leaves one handler waiting at its barrier.
		cancel()
		releaseWinner()
		releaseStale()
		workers.Wait()
	}()
	f.db.DB.AddQueryHook(&vpcCreationQueryHook{before: func(ctx context.Context, event *bun.QueryEvent) {
		if !strings.HasPrefix(event.Query, "UPDATE") || !strings.Contains(event.Query, "creation_rejected_at IS NOT NULL") || !strings.Contains(event.Query, saved.ID.String()) {
			return
		}
		reached <- struct{}{}
		release := staleRelease
		if ctx.Value(vpcCreationRequestKey{}) == "winner" {
			release = winnerRelease
		}
		select {
		case <-release:
		case <-ctx.Done():
		}
	}})
	type result struct {
		code int
		err  error
	}
	winnerResult, staleResult := make(chan result, 1), make(chan result, 1)
	for _, attempt := range []struct {
		name   string
		result chan result
	}{{"winner", winnerResult}, {"stale", staleResult}} {
		c, response := f.context(context.WithValue(ctx, vpcCreationRequestKey{}, attempt.name), http.MethodDelete, saved.ID.String(), nil)
		workers.Add(1)
		go func() {
			defer workers.Done()
			err := NewDeleteVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c)
			attempt.result <- result{code: response.Code, err: err}
		}()
	}
	for range 2 {
		select {
		case <-reached:
		case <-ctx.Done():
			t.Fatal("both handlers did not reach conditional deletion")
		}
	}
	// Let the winner delete the original group before another owner reuses its name.
	releaseWinner()
	winner := <-winnerResult
	require.NoError(t, winner.err)
	require.Equal(t, http.StatusAccepted, winner.code)
	require.NoError(t, dps.CreateResourceGroup(ctx, *saved.PowerResourceGroup, 2))
	// The delayed caller sees the existing local deletion and must leave that group alone.
	releaseStale()
	stale := <-staleResult
	require.NoError(t, stale.err)
	assert.Equal(t, http.StatusAccepted, stale.code)
	assert.Equal(t, []string{*saved.PowerResourceGroup}, dps.deleted)
	assert.Equal(t, int64(2), dps.owners[*saved.PowerResourceGroup])
	c, response := f.context(ctx, http.MethodDelete, saved.ID.String(), nil)
	require.NoError(t, NewDeleteVPCHandler(f.db, f.client, f.pool, f.cfg, dps).Handle(c))
	assert.Equal(t, http.StatusNotFound, response.Code)
	assert.Len(t, dps.deleted, 1)
	assert.Equal(t, int64(2), dps.owners[*saved.PowerResourceGroup])
	assert.Empty(t, f.client.Calls)
}

// mockVpcCreationCancellation verifies the generic Core proxy request and returns
// either the committed VPC, confirmed cancellation, or an unsettled Core failure.
func mockVpcCreationCancellation(t *testing.T, client *tmocks.Client, saved *cdbm.Vpc, result *corev1.CancelVpcCreationResponse, failureType string) {
	t.Helper()
	run := &tmocks.WorkflowRun{}
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(request grpcproxy.Request) bool {
		var decoded corev1.CancelVpcCreationRequest
		err := protojson.Unmarshal(request.RequestJSON, &decoded)
		return err == nil && request.FullMethod == corev1.Forge_CancelVpcCreation_FullMethodName &&
			decoded.GetId().GetValue() == saved.GetSiteID().String() && decoded.TenantOrganizationId == saved.Org
	})).Return(run, nil).Once()
	encoded, err := protojson.Marshal(result)
	require.NoError(t, err)
	var resultErr error
	if failureType != "" {
		resultErr = temporal.NewNonRetryableApplicationError("creation cleanup is unresolved", failureType, nil)
	}
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		args.Get(1).(*grpcproxy.Response).ResponseJSON = encoded
	}).Return(resultErr).Once()
	t.Cleanup(func() { run.AssertExpectations(t) })
}

func testVpcVirtualizationPending(t *testing.T) {
	f := newVpcCreationFixture(t)
	// Pending state takes precedence even when the requested type is already
	// present in the saved input, because Core has not confirmed that input yet.
	for _, kind := range []string{cdbm.VpcEthernetVirtualizer, cdbm.VpcFNN} {
		t.Run(kind, func(t *testing.T) {
			vpc := testVPCBuildVPC(t, f.db, "pending virtualization "+kind, f.provider, f.tenant, f.site, &kind, nil, nil, cdbm.VpcStatusPending, f.user)
			c, response := f.context(context.Background(), http.MethodPatch, vpc.ID.String(), []byte(`{"networkVirtualizationType":"FNN"}`))
			require.NoError(t, NewUpdateVPCVirtualizationHandler(f.db, f.client, f.pool, f.cfg).Handle(c))
			assert.Equal(t, http.StatusConflict, response.Code, response.Body.String())
			assert.Contains(t, response.Body.String(), "creation is pending")
			assert.Empty(t, f.client.Calls)
		})
	}
}
