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
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	apisc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	ctemporal "github.com/NVIDIA/infra-controller/rest-api/common/pkg/temporal"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	vpccommon "github.com/NVIDIA/infra-controller/rest-api/common/pkg/vpc"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
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

	// Keep the real Temporal history on disk, independently of each recovery
	// activity object. This test stops the server through the SDK in Cleanup.
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

	// Reconstruct the recovery owner and client pool, using the original durable
	// workflow and Core identity. Preserve the original caller's CreatedBy and metadata.
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

	t.Run("native Temporal result and rejection history", func(t *testing.T) {
		// This narrow encoding proof uses the same real Temporal server. Its local
		// activity does not make another Core call or provider allocation.
		const taskQueue = "vpc-completion-history"
		historyWorker := worker.New(tc, taskQueue, worker.Options{})
		historyWorker.RegisterWorkflow(sww.CreateVPCV2)
		historyWorker.RegisterActivityWithOptions(func(_ context.Context, req *corev1.VpcCreationRequest) (*corev1.Vpc, error) {
			if req.Name == "rejected" {
				return nil, temporal.NewNonRetryableApplicationError("invalid original request", swe.ErrTypeNICoInvalidArgument, nil)
			}
			return &corev1.Vpc{Id: req.Id, Config: &corev1.VpcConfig{TenantOrganizationId: req.TenantOrganizationId}}, nil
		}, activity.RegisterOptions{Name: "CreateVpcOnSite"})
		require.NoError(t, historyWorker.Start())
		t.Cleanup(historyWorker.Stop)
		for _, name := range []string{"created", "rejected"} {
			t.Run(name, func(t *testing.T) {
				id := uuid.NewString()
				workflowID := "vpc-create-" + id
				run, err := tc.ExecuteWorkflow(ctx, client.StartWorkflowOptions{ID: workflowID, TaskQueue: taskQueue, WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout}, "CreateVPCV2", &corev1.VpcCreationRequest{Id: &corev1.VpcId{Value: id}, Name: name, TenantOrganizationId: tenant.Org})
				require.NoError(t, err)
				var result corev1.Vpc
				err = run.Get(ctx, &result)
				if name == "rejected" {
					require.Error(t, err)
				} else {
					require.NoError(t, err)
				}
				created, rejected, err := vpccommon.ObserveCreation(ctx, tc, workflowID)
				require.NoError(t, err)
				require.Equal(t, name == "rejected", rejected)
				if name == "created" {
					require.NotNil(t, created)
					require.Equal(t, id, created.GetId().GetValue())
				} else {
					require.Nil(t, created)
				}
			})
		}
	})
}
