// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
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
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/api/serviceerror"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	temporalclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
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
		name       string
		createErr  error
		code       int
		compensate bool
	}{
		{name: "canceled save compensates confirmed DPS creation", code: http.StatusInternalServerError, compensate: true},
		{name: "existing DPS group is never compensated", createErr: dpsclient.ErrResourceGroupAlreadyExists, code: http.StatusConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newVpcCreationFixture(t)
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
				for i, request := range requests {
					body, err := json.Marshal(request)
					require.NoError(t, err)
					name := "winner-" + id.String()
					if i == 1 {
						name = "loser-" + id.String()
					}
					requestCtx := context.WithValue(ctx, vpcCreationRequestKey{}, name)
					go func() {
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
			name            string
			startErr        error
			cancelAfterSave bool
			rejectSaveFails bool
			mismatch        bool
			code            int
			state           string
			cancelAtStart   bool
			cancelAtGet     bool
		}{
			{name: "ambiguous start", startErr: errors.New("transport lost start reply"), code: 500, state: cdbm.VpcStatusPending},
			{name: "start deadline", startErr: serviceerror.NewDeadlineExceeded("start reply timed out"), code: 504, state: cdbm.VpcStatusPending},
			{name: "existing execution", startErr: serviceerror.NewWorkflowExecutionAlreadyStarted("exists", "request", "run"), code: 409, state: cdbm.VpcStatusPending},
			{name: "known not dispatched", cancelAfterSave: true, code: 504, state: cdbm.VpcStatusError},
			{name: "non-dispatch decision cannot be saved", cancelAfterSave: true, rejectSaveFails: true, code: 504, state: cdbm.VpcStatusPending},
			{name: "completion conflicts", mismatch: true, code: 409, state: cdbm.VpcStatusPending},
			{name: "expired start context maps a different error to timeout", startErr: errors.New("start transport stopped"), cancelAtStart: true, code: 504, state: cdbm.VpcStatusPending},
			{name: "expired result context retains Pending", cancelAtGet: true, code: 504, state: cdbm.VpcStatusPending},
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
						run.On("Get", mock.Anything, mock.Anything).Run(func(mock.Arguments) { cancel() }).Return(errors.New("result transport stopped")).Once()
					}
					if tc.mismatch {
						run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
							saved, err := cdbm.NewVpcDAO(f.db).GetByID(args.Get(0).(context.Context), nil, id, nil)
							require.NoError(t, err)
							result := args.Get(1).(*corev1.Vpc)
							proto.Merge(result, saved.ToProto())
							result.Metadata.Name = "different original input"
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
				saved, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
				require.NoError(t, err)
				assert.Equal(t, tc.state, saved.Status)
				assert.Equal(t, tc.name, saved.Name)
				assert.Equal(t, tc.cancelAfterSave && !tc.rejectSaveFails, saved.CreationRejectedAt != nil)
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
	t.Run("commit failure retains DPS and the original identity", func(t *testing.T) {
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
		assert.Contains(t, response.Body.String(), id.String())
		assert.Contains(t, response.Body.String(), "confirm persistence")
		assert.Equal(t, []string{"commit-group"}, dps.created)
		assert.Empty(t, dps.deleted)
		assert.Empty(t, f.client.Calls)
	})
	t.Run("history presentation failure preserves committed success", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		id := uuid.New()
		failedRead := false
		f.db.DB.AddQueryHook(&vpcCreationQueryHook{contextForQuery: func(ctx context.Context, event *bun.QueryEvent) context.Context {
			if !failedRead && event.Operation() == "SELECT" && strings.Contains(event.Query, `"status_detail"`) && strings.Contains(event.Query, id.String()) {
				failedRead = true
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
		assert.True(t, failedRead)
		assert.Equal(t, http.StatusCreated, response.Code, response.Body.String())
		var returned model.APIVpc
		require.NoError(t, json.Unmarshal(response.Body.Bytes(), &returned))
		assert.Equal(t, id.String(), returned.ID)
		assert.NotNil(t, returned.StatusHistory)
		assert.Empty(t, returned.StatusHistory)
		persisted, err := cdbm.NewVpcDAO(f.db).GetByID(context.Background(), nil, id, nil)
		require.NoError(t, err)
		assert.Equal(t, cdbm.VpcStatusReady, persisted.Status)
		history, _, err := cdbm.NewStatusDetailDAO(f.db).GetAll(context.Background(), nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{id.String()}}, cdbp.PageInput{})
		require.NoError(t, err)
		assert.Len(t, history, 2)
		f.client.AssertExpectations(t)
		run.AssertExpectations(t)
	})
	t.Run("workflow wait clock starts after initial persistence", func(t *testing.T) {
		f := newVpcCreationFixture(t)
		id := uuid.New()
		initialSaved := false
		var committedAt time.Time
		f.db.DB.AddQueryHook(&vpcCreationQueryHook{before: func(ctx context.Context, event *bun.QueryEvent) {
			if strings.HasPrefix(event.Query, `INSERT INTO "vpc"`) && strings.Contains(event.Query, id.String()) {
				_, hasDeadline := ctx.Deadline()
				assert.False(t, hasDeadline)
				initialSaved = true
			}
		}, after: func(ctx context.Context, event *bun.QueryEvent) {
			if event.Query == "COMMIT" && event.Err == nil {
				committedAt = time.Now()
			}
		}})
		run := &tmocks.WorkflowRun{}
		var dispatchDeadline time.Time
		f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateVPCV2", mock.Anything).Run(func(args mock.Arguments) {
			assert.True(t, initialSaved)
			deadline, hasDeadline := args.Get(0).(context.Context).Deadline()
			require.True(t, hasDeadline)
			dispatchDeadline = deadline
			require.False(t, committedAt.IsZero())
			assert.False(t, deadline.Add(-cutil.WorkflowContextTimeout).Before(committedAt))
			assert.InDelta(t, cutil.WorkflowContextTimeout.Seconds(), time.Until(deadline).Seconds(), 1)
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

func testVpcDeletionCreationDecisions(t *testing.T) {
	f := newVpcCreationFixture(t)
	cases := []struct {
		name                string
		status              string
		rejectFirst         bool
		unknown             bool
		observe             bool
		rejection           bool
		failHistory         bool
		running             bool
		outage              error
		wantCode            int
		originalUnavailable bool
	}{
		{name: "persisted rejection survives missing history", status: cdbm.VpcStatusPending, rejectFirst: true, wantCode: 202},
		{name: "imported Error needs no creation history", status: cdbm.VpcStatusError, wantCode: 202},
		{name: "unresolved Pending cannot release dependencies", status: cdbm.VpcStatusPending, unknown: true, wantCode: 409},
		{name: "running creation cannot release dependencies", status: cdbm.VpcStatusPending, running: true, wantCode: 409},
		{name: "Temporal observation outage is unavailable", status: cdbm.VpcStatusPending, outage: serviceerror.NewUnavailable("Temporal unavailable"), wantCode: 503},
		{name: "Temporal observation deadline is a timeout", status: cdbm.VpcStatusPending, outage: serviceerror.NewDeadlineExceeded("Temporal read expired"), wantCode: 504},
		{name: "original Core unavailable remains uncertain", status: cdbm.VpcStatusPending, observe: true, originalUnavailable: true, wantCode: 409},
		{name: "successful original result continues deletion", status: cdbm.VpcStatusPending, observe: true, wantCode: 202},
		{name: "observed rejection permits only local deletion", status: cdbm.VpcStatusPending, observe: true, rejection: true, wantCode: 202},
		{name: "rejection must persist before cleanup", status: cdbm.VpcStatusPending, observe: true, rejection: true, failHistory: true, wantCode: 500},
		{name: "completion database failure is not a conflict", status: cdbm.VpcStatusPending, observe: true, failHistory: true, wantCode: 500},
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
			if tc.observe {
				failureType := ""
				if tc.rejection {
					failureType = swe.ErrTypeNICoInvalidArgument
				} else if tc.originalUnavailable {
					failureType = swe.ErrTypeNICoUnavailable
				}
				mockVpcCreationForDeletion(t, client, saved, failureType)
			} else if tc.unknown {
				client.On("DescribeWorkflowExecution", mock.Anything, "vpc-create-"+saved.ID.String(), "").Return(nil, serviceerror.NewNotFound("expired history")).Once()
			} else if tc.outage != nil {
				client.On("DescribeWorkflowExecution", mock.Anything, "vpc-create-"+saved.ID.String(), "").Return(nil, tc.outage).Once()
			} else if tc.running {
				client.On("DescribeWorkflowExecution", mock.Anything, "vpc-create-"+saved.ID.String(), "").Return(&workflowservice.DescribeWorkflowExecutionResponse{
					WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Type: &commonpb.WorkflowType{Name: "CreateVPCV2"}, Status: enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING,
						Execution: &commonpb.WorkflowExecution{WorkflowId: "vpc-create-" + saved.ID.String(), RunId: "original-run"}},
				}, nil).Once()
			}
			if tc.failHistory {
				_, err := f.db.DB.ExecContext(ctx, `ALTER TABLE status_detail ADD CONSTRAINT vpc_delete_history_failure CHECK (entity_id <> ?)`, saved.ID.String())
				require.NoError(t, err)
				t.Cleanup(func() {
					_, err := f.db.DB.ExecContext(ctx, `ALTER TABLE status_detail DROP CONSTRAINT vpc_delete_history_failure`)
					assert.NoError(t, err)
				})
			}
			if !tc.unknown && !tc.running && tc.outage == nil && !tc.failHistory && !tc.rejectFirst && !tc.rejection && !tc.originalUnavailable {
				run := &tmocks.WorkflowRun{}
				run.On("GetID").Return("vpc-delete-" + saved.ID.String()).Once()
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVPCV2", mock.Anything).Return(run, nil).Once()
				run.On("Get", mock.Anything, mock.Anything).Return(temporal.NewNonRetryableApplicationError("already absent", swe.ErrTypeNICoObjectNotFound, nil)).Once()
				t.Cleanup(func() { run.AssertExpectations(t) })
			}
			f.pool.IDClientMap[f.site.ID.String()] = client
			dps := &recordingVpcDPS{}
			deleteCtx := ctx
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
			c, response := f.context(deleteCtx, http.MethodDelete, saved.ID.String(), nil)
			require.NoError(t, NewDeleteVPCHandler(f.db, client, f.pool, f.cfg, dps).Handle(c))
			assert.Equal(t, tc.wantCode, response.Code, response.Body.String())
			if tc.failHistory {
				assert.Equal(t, http.StatusInternalServerError, response.Code, response.Body.String())
				assert.Empty(t, dps.deleted)
			} else if tc.unknown || tc.running || tc.outage != nil || tc.originalUnavailable {
				assert.Empty(t, dps.deleted)
				client.AssertCalled(t, "DescribeWorkflowExecution", mock.Anything, "vpc-create-"+saved.ID.String(), "")
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
				if !tc.observe {
					client.AssertNotCalled(t, "DescribeWorkflowExecution", mock.Anything, mock.Anything, mock.Anything)
				}
			}
			rows, _, err := cdbm.NewVpcDAO(f.db).GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{saved.ID}, IncludeDeleted: true}, cdbp.PageInput{}, nil)
			require.NoError(t, err)
			require.Len(t, rows, 1)
			actual := rows[0]
			if tc.rejectFirst || (tc.rejection && !tc.failHistory) {
				assert.NotNil(t, actual.Deleted)
				assert.NotNil(t, actual.CreationRejectedAt)
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, "DeleteVPCV2", mock.Anything)
			} else if tc.unknown || tc.running || tc.outage != nil || tc.failHistory || tc.originalUnavailable {
				assert.Equal(t, cdbm.VpcStatusPending, actual.Status)
				assert.Nil(t, actual.CreationRejectedAt)
			} else {
				assert.Equal(t, cdbm.VpcStatusDeleting, actual.Status)
				assert.Nil(t, actual.CreationRejectedAt)
			}
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
	defer releaseWinner()
	defer releaseStale()
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
		go func() {
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

func mockVpcCreationForDeletion(t *testing.T, client *tmocks.Client, saved *cdbm.Vpc, failureType string) {
	t.Helper()
	workflowID, runID := "vpc-create-"+saved.ID.String(), "original-run"
	status := enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED
	if failureType != "" {
		status = enumspb.WORKFLOW_EXECUTION_STATUS_FAILED
	}
	client.On("DescribeWorkflowExecution", mock.Anything, workflowID, "").Return(&workflowservice.DescribeWorkflowExecutionResponse{
		WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Type: &commonpb.WorkflowType{Name: "CreateVPCV2"}, Status: status,
			Execution: &commonpb.WorkflowExecution{WorkflowId: workflowID, RunId: runID}},
	}, nil).Once()
	run := &tmocks.WorkflowRun{}
	client.On("GetWorkflow", mock.Anything, workflowID, runID).Return(run).Once()
	var resultErr error
	if failureType != "" {
		converter := temporal.GetDefaultFailureConverter()
		resultErr = converter.FailureToError(&failurepb.Failure{
			Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("original create failed", failureType, nil)),
			FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{
				ActivityType: &commonpb.ActivityType{Name: "CreateVpcOnSite"}, ActivityId: "create", ScheduledEventId: 2, StartedEventId: 3,
			}},
		})
	}
	if failureType == swe.ErrTypeNICoInvalidArgument {
		events := []*historypb.HistoryEvent{
			{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{
				WorkflowType: &commonpb.WorkflowType{Name: "CreateVPCV2"}, Attempt: 1, OriginalExecutionRunId: runID, FirstExecutionRunId: runID,
			}}},
			{EventId: 2, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{
				ActivityType: &commonpb.ActivityType{Name: "CreateVpcOnSite"}, ActivityId: "create",
			}}},
			{EventId: 3, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{
				ScheduledEventId: 2, Attempt: 1,
			}}},
		}
		history := &tmocks.HistoryEventIterator{}
		for _, event := range events {
			history.On("HasNext").Return(true).Once()
			history.On("Next").Return(event, nil).Once()
		}
		client.On("GetWorkflowHistory", mock.Anything, workflowID, runID, false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(history).Once()
		t.Cleanup(func() { history.AssertExpectations(t) })
	}
	run.On("GetWithOptions", mock.Anything, mock.Anything, temporalclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
		proto.Merge(args.Get(1).(*corev1.Vpc), saved.ToProto())
	}).Return(resultErr).Once()
	t.Cleanup(func() { run.AssertExpectations(t) })
}

func testVpcVirtualizationPending(t *testing.T) {
	f := newVpcCreationFixture(t)
	vpc := testVPCBuildVPC(t, f.db, "pending virtualization", f.provider, f.tenant, f.site, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, nil, cdbm.VpcStatusPending, f.user)
	c, response := f.context(context.Background(), http.MethodPatch, vpc.ID.String(), []byte(`{"networkVirtualizationType":"FNN"}`))
	require.NoError(t, NewUpdateVPCVirtualizationHandler(f.db, f.client, f.pool, f.cfg).Handle(c))
	assert.Equal(t, http.StatusConflict, response.Code, response.Body.String())
	assert.Contains(t, response.Body.String(), "creation is pending")
	assert.Empty(t, f.client.Calls)
}
