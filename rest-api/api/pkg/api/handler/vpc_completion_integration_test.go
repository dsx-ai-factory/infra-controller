// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	apisc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	ctemporal "github.com/NVIDIA/infra-controller/rest-api/common/pkg/temporal"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	coreclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	vpcactivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/vpc"
	wfsc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// completionFailure records the injected database rejection so the test proves
// recovery attempted the write, rather than merely leaving the row unchanged.
type completionFailure struct {
	rejections atomic.Int64
}

func (f *completionFailure) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}

// AfterQuery counts only the database rejection injected by this test.
func (f *completionFailure) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if event.Err != nil && strings.Contains(event.Err.Error(), "completion_test_reject_write") {
		f.rejections.Add(1)
	}
}

// testVPCCompletionIntegration crosses the actual REST, Temporal, Site and Core
// boundaries. Its Rust caller drops Core's reply after committing the VPC and
// later checks that recovery made no second creation or provider allocation.
// Ordinary Go tests skip it because the Rust caller owns the live Core fixture.
func testVPCCompletionIntegration(t *testing.T) {
	endpoint := os.Getenv("NICO_TEST_CORE_ENDPOINT")
	if endpoint == "" {
		t.Skip("run the ignored rest_vpc_completion Core test with the isolated test helper")
	}
	vpcID, err := uuid.Parse(os.Getenv("NICO_TEST_VPC_ID"))
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	// Exercise the real Site transport with an isolated Temporal server.
	// Inventory recovery below uses Core state, not this server's history.
	server, err := testsuite.StartDevServer(ctx, testsuite.DevServerOptions{
		CachedDownload: testsuite.CachedDownload{DestDir: t.TempDir()},
		DBFilename:     filepath.Join(t.TempDir(), "temporal.sqlite"),
		ClientOptions:  &client.Options{DataConverter: ctemporal.DataConverter()},
	})
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, server.Stop()) })
	tc := server.Client()
	core, err := coreclient.NewCoreGrpcClient(&coreclient.CoreGrpcClientConfig{
		Address: endpoint,
		Secure:  coreclient.InsecureGrpc,
	})
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, core.Close()) })
	atomicClient := coreclient.NewCoreGrpcAtomicClient(&coreclient.CoreGrpcClientConfig{})
	atomicClient.SwapClient(core)
	siteActivity := swa.NewManageVPC(atomicClient)
	siteWorker := worker.New(tc, queue.SiteTaskQueue, worker.Options{})
	siteWorker.RegisterWorkflow(sww.CreateVPCV2)
	siteWorker.RegisterActivity(siteActivity.CreateVpcOnSite)
	proxyActivity := swa.NewManageCoreProxy(atomicClient, "")
	siteWorker.RegisterWorkflow(sww.InvokeCoreGRPC)
	siteWorker.RegisterActivity(proxyActivity.InvokeCoreGRPCOnSite)
	require.NoError(t, siteWorker.Start())
	t.Cleanup(siteWorker.Stop)

	// Build only the existing cloud records needed by the public handler. Core's
	// tenant and provider-backed VNI pool are owned by the Rust fixture.
	dbSession := testVPCInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	common.TestSetupSchema(t, dbSession)
	providerUser := testVPCBuildUser(t, dbSession, "completion-provider", "completion-provider", []string{authz.ProviderAdminRole})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "completion-provider", "completion-provider", providerUser)
	user := testVPCBuildUser(t, dbSession, "completion-caller", "rest-completion-tenant", []string{authz.TenantAdminRole})
	tenant := testVPCBuildTenant(t, dbSession, "completion-tenant", "rest-completion-tenant", user)
	site := testVPCBuildSite(t, dbSession, provider, "completion-site", true, false, cdbm.SiteStatusRegistered, providerUser)
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, provider, &tenant.ID, tenant.Org, cdbm.TenantAccountStatusReady, user)
	testVPCSiteBuildAllocation(t, dbSession, site, tenant, "completion-allocation", providerUser)
	testBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, site.ID, user.ID)
	cfg := common.GetTestConfig()
	temporalConfig := &cconfig.TemporalConfig{}
	apiPool := apisc.NewClientPool(temporalConfig)
	apiPool.IDClientMap[site.ID.String()] = tc

	// The real Site activity receives Unavailable after Core commits. REST must
	// return an error while leaving this exact caller record available to recover.
	request := model.APIVpcCreateRequest{
		ID:          &vpcID,
		Name:        "lost-reply-vpc",
		Description: cutil.GetPtr("keep the original caller's description"),
		SiteID:      site.ID.String(),
		Labels:      map[string]string{"purpose": "completion-proof"},
	}
	body, err := json.Marshal(request)
	require.NoError(t, err)
	httpRequest := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body))).WithContext(ctx)
	httpRequest.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	response := httptest.NewRecorder()
	echoContext := echo.New().NewContext(httpRequest, response)
	echoContext.SetParamNames("orgName")
	echoContext.SetParamValues(tenant.Org)
	echoContext.Set("user", user)
	handler := NewCreateVPCHandler(dbSession, tc, apiPool, cfg, nil)
	require.NoError(t, handler.Handle(echoContext))
	require.Equal(t, http.StatusServiceUnavailable, response.Code, response.Body.String())
	require.Contains(t, response.Body.String(), vpcID.String())
	vpcDAO := cdbm.NewVpcDAO(dbSession)
	saved, err := vpcDAO.GetByID(ctx, nil, vpcID, nil)
	require.NoError(t, err)
	require.Equal(t, cdbm.VpcStatusPending, saved.Status)
	require.Equal(t, user.ID, saved.CreatedBy)
	require.Equal(t, vpcID, *saved.ControllerVpcID)

	// Inventory reads the committed Core entity. Inject a local completion
	// failure so merely finding Core's row cannot accidentally count as repair.
	committed, err := core.GrpcServiceClient().FindVpcsByIds(ctx, &corev1.VpcsByIdsRequest{
		VpcIds: []*corev1.VpcId{{Value: vpcID.String()}},
	})
	require.NoError(t, err)
	require.Len(t, committed.Vpcs, 1)
	require.NotNil(t, committed.Vpcs[0].GetStatus().Vni)
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE vpc ADD CONSTRAINT completion_test_reject_write CHECK (status = 'Pending')`)
	require.NoError(t, err)
	failure := &completionFailure{}
	dbSession.DB.AddQueryHook(failure)
	workflowPool := wfsc.NewClientPool(temporalConfig)
	workflowPool.IDClientMap[site.ID.String()] = tc
	inventory := &corev1.VPCInventory{
		Vpcs:            committed.Vpcs,
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Timestamp:       timestamppb.Now(),
	}
	recovery := vpcactivity.NewManageVpc(dbSession, workflowPool, tc)
	_, recoveryErr := recovery.UpdateVpcsInDB(ctx, site.ID, inventory)
	if recoveryErr != nil {
		t.Logf("injected completion failure: %v", recoveryErr)
	}
	require.Positive(t, failure.rejections.Load(), "the injected completion write was not attempted")
	failedCompletion, err := vpcDAO.GetByID(ctx, nil, vpcID, nil)
	require.NoError(t, err)
	require.Equal(t, cdbm.VpcStatusPending, failedCompletion.Status)
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE vpc DROP CONSTRAINT completion_test_reject_write`)
	require.NoError(t, err)

	// Reconstruct the inventory activity and database connection. The same Core
	// VPC must complete the saved request without changing its caller or metadata.
	dbSession.Close()
	restartedDB := cdbu.GetTestDBSession(t, false)
	t.Cleanup(func() { restartedDB.Close() })
	restartedClient, err := client.Dial(client.Options{HostPort: server.FrontendHostPort(), DataConverter: ctemporal.DataConverter()})
	require.NoError(t, err)
	t.Cleanup(restartedClient.Close)
	restartedPool := wfsc.NewClientPool(temporalConfig)
	restartedPool.IDClientMap[site.ID.String()] = restartedClient
	restarted := vpcactivity.NewManageVpc(restartedDB, restartedPool, restartedClient)
	_, err = restarted.UpdateVpcsInDB(ctx, site.ID, inventory)
	require.NoError(t, err)
	completed, err := cdbm.NewVpcDAO(restartedDB).GetByID(ctx, nil, vpcID, nil)
	require.NoError(t, err)
	require.Equal(t, cdbm.VpcStatusReady, completed.Status)
	require.Equal(t, saved.ID, completed.ID)
	require.Equal(t, saved.Created, completed.Created)
	require.Equal(t, user.ID, completed.CreatedBy)
	require.Equal(t, tenant.ID, completed.TenantID)
	require.Equal(t, site.ID, completed.SiteID)
	require.Equal(t, provider.ID, completed.InfrastructureProviderID)
	require.Equal(t, saved.Name, completed.Name)
	require.Equal(t, saved.Description, completed.Description)
	require.Equal(t, saved.Labels, completed.Labels)
	require.Equal(t, saved.Vni, completed.Vni)
	require.NotNil(t, completed.ActiveVni)
	require.Equal(t, int(*committed.Vpcs[0].GetStatus().Vni), *completed.ActiveVni)

	t.Run("synchronous creation returns the committed VPC", func(t *testing.T) {
		id := uuid.New()
		request.ID = &id
		request.Name = "synchronous-vpc"
		body, err := json.Marshal(request)
		require.NoError(t, err)
		httpRequest := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(body))).WithContext(ctx)
		httpRequest.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		response := httptest.NewRecorder()
		c := echo.New().NewContext(httpRequest, response)
		c.SetParamNames("orgName")
		c.SetParamValues(tenant.Org)
		c.Set("user", user)
		handler := NewCreateVPCHandler(restartedDB, tc, apiPool, cfg, nil)
		require.NoError(t, handler.Handle(c))
		require.Equal(t, http.StatusCreated, response.Code, response.Body.String())
		var returned model.APIVpc
		require.NoError(t, json.Unmarshal(response.Body.Bytes(), &returned))
		require.Equal(t, id.String(), returned.ID)
		require.Equal(t, cdbm.VpcStatusReady, returned.Status)
		persisted, err := cdbm.NewVpcDAO(restartedDB).GetByID(ctx, nil, id, nil)
		require.NoError(t, err)
		require.Equal(t, user.ID, persisted.CreatedBy)
		require.Equal(t, request.Description, persisted.Description)
		require.Equal(t, request.Labels, map[string]string(persisted.Labels))
		require.Equal(t, cdbm.VpcStatusReady, persisted.Status)
		require.NotNil(t, persisted.ActiveVni)
	})

	t.Run("saved intent without dispatch can be cancelled after a lost cleanup reply", func(t *testing.T) {
		// Simulate process loss immediately after the REST intent committed. No
		// creation workflow exists, so retained Temporal history cannot help.
		id := uuid.New()
		dao := cdbm.NewVpcDAO(restartedDB)
		pending, err := dao.Create(ctx, nil, cdbm.VpcCreateInput{
			ID: &id, ControllerVpcID: &id, Name: "never-dispatched-vpc", Org: tenant.Org,
			TenantID: tenant.ID, SiteID: site.ID, InfrastructureProviderID: provider.ID,
			NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcFNN),
			Status:                    cdbm.VpcStatusPending, CreatedBy: *user,
		})
		require.NoError(t, err)
		for attempt, wantCode := range []int{http.StatusServiceUnavailable, http.StatusAccepted} {
			// Core completes cancellation, then the fixture loses its first reply.
			// The retry reconstructs the handler and repeats the same Core identity.
			req := httptest.NewRequest(http.MethodDelete, "/", nil).WithContext(ctx)
			rec := httptest.NewRecorder()
			c := echo.New().NewContext(req, rec)
			c.SetParamNames("orgName", "id")
			c.SetParamValues(tenant.Org, id.String())
			c.Set("user", user)
			require.NoError(t, NewDeleteVPCHandler(restartedDB, tc, apiPool, cfg, nil).Handle(c))
			require.Equal(t, wantCode, rec.Code, rec.Body.String())
			if attempt == 0 {
				retained, err := dao.GetByID(ctx, nil, id, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.VpcStatusPending, retained.Status)
				require.Equal(t, user.ID, retained.CreatedBy)
				require.Nil(t, retained.CreationRejectedAt)
			}
		}
		rows, _, err := dao.GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{id}, IncludeDeleted: true}, cdbp.PageInput{}, nil)
		require.NoError(t, err)
		require.Len(t, rows, 1)
		require.NotNil(t, rows[0].Deleted)
		require.NotNil(t, rows[0].CreationRejectedAt)

		// Even a late first Core RPC cannot revive the locally deleted request.
		late := model.APIVpcCreateRequest{ID: &id, Name: pending.Name, SiteID: site.ID.String()}
		_, err = core.GrpcServiceClient().CreateVpc(ctx, late.ToProto(pending))
		require.Equal(t, codes.FailedPrecondition, status.Code(err))
		require.Contains(t, status.Convert(err).Message(), "cancelled")
		found, err := core.GrpcServiceClient().FindVpcsByIds(ctx, &corev1.VpcsByIdsRequest{VpcIds: []*corev1.VpcId{{Value: id.String()}}})
		require.NoError(t, err)
		require.Empty(t, found.Vpcs)
	})

}
