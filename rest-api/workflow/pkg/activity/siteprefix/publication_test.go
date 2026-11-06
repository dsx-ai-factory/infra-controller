// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package siteprefix

import (
	"context"
	"errors"
	"fmt"
	"net"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/grpc"
)

// Exercise real gRPC pagination and PostgreSQL reconciliation. Temporal's test
// scheduler drives the discovery retry; the publisher substitutes only the
// Cloud transport. The failing page injects a workflow-result failure; other pages
// wait for the real receiver result.
func checkCollectionSchedule(t *testing.T, f testFixture) {
	t.Helper()
	server := &inventoryServer{prefixes: make(map[string]*corev1.SitePrefix)}
	for index := range 1001 {
		prefix := testPrefix()
		prefix.Id.Value = fmt.Sprintf("00000000-0000-0000-0000-%012d", index+1)
		prefix.Config.Prefix = fmt.Sprintf("10.%d.%d.0/24", index/256, index%256)
		prefix.Metadata.Name = prefix.Config.Prefix
		server.ids = append(server.ids, prefix.Id)
		server.prefixes[prefix.Id.Value] = prefix
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	grpcServer := grpc.NewServer()
	corev1.RegisterForgeServer(grpcServer, server)
	serveErr := make(chan error, 1)
	go func() { serveErr <- grpcServer.Serve(listener) }()
	t.Cleanup(func() {
		grpcServer.Stop()
		require.NoError(t, <-serveErr)
	})
	core, err := cClient.NewCoreGrpcClient(&cClient.CoreGrpcClientConfig{Address: listener.Addr().String(), Secure: cClient.InsecureGrpc})
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, core.Close()) })
	atomic := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	atomic.SwapClient(core)
	publisher := &inventoryPublisher{fixture: f, failPage: 2, workflowIDs: make(map[string]bool)}
	collector := swa.NewManageSitePrefixInventory(swa.ManageInventoryConfig{
		SiteID: f.site.ID, CoreGrpcAtomicClient: atomic, TemporalPublishClient: publisher,
		TemporalPublishQueue: "cloud", SitePageSize: 100, CloudPageSize: 25,
	})
	var suite testsuite.WorkflowTestSuite
	env := suite.NewTestWorkflowEnvironment()
	env.SetTestTimeout(cutil.DefaultInventoryReceiptInterval)
	env.RegisterActivity(collector.DiscoverSitePrefixInventory)
	started := time.Now()
	env.ExecuteWorkflow(sww.DiscoverSitePrefixInventory)
	elapsed := time.Since(started)
	require.NoError(t, publisher.validationErr)
	require.NoError(t, env.GetWorkflowError())
	require.True(t, publisher.failed)
	require.Equal(t, 43, publisher.pages) // two attempted pages, then all 41 pages on retry
	require.Len(t, f.blocks(t), 1001)
	require.Less(t, elapsed, cutil.DefaultInventoryReceiptInterval)
	t.Logf("1001 SitePrefixes, 41 pages, injected receiver failure and retry: %s", elapsed)
}

type inventoryServer struct {
	corev1.UnimplementedForgeServer
	ids      []*corev1.SitePrefixId
	prefixes map[string]*corev1.SitePrefix
}

func (s *inventoryServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{RuntimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 100}}, nil
}

func (s *inventoryServer) FindSitePrefixIds(context.Context, *corev1.SitePrefixSearchFilter) (*corev1.SitePrefixIdList, error) {
	return &corev1.SitePrefixIdList{SitePrefixIds: s.ids}, nil
}

func (s *inventoryServer) FindSitePrefixesByIds(_ context.Context, request *corev1.SitePrefixesByIdsRequest) (*corev1.SitePrefixList, error) {
	if len(request.SitePrefixIds) > 100 {
		return nil, errors.New("Core page limit exceeded")
	}
	response := &corev1.SitePrefixList{}
	for _, id := range request.SitePrefixIds {
		response.SitePrefixes = append(response.SitePrefixes, s.prefixes[id.Value])
	}
	return response, nil
}

type inventoryPublisher struct {
	client.Client
	validationErr error
	fixture       testFixture
	failPage      int32
	failed        bool
	pages         int
	workflowIDs   map[string]bool
}

func (p *inventoryPublisher) ExecuteWorkflow(ctx context.Context, options client.StartWorkflowOptions, workflow any, args ...any) (client.WorkflowRun, error) {
	if p.validationErr != nil {
		return nil, p.validationErr
	}
	inventory := args[1].(*corev1.SitePrefixInventory)
	switch {
	case workflow != "UpdateSitePrefixInventory":
		p.validationErr = fmt.Errorf("unexpected workflow: %v", workflow)
	case options.TaskQueue != "cloud":
		p.validationErr = fmt.Errorf("unexpected task queue: %s", options.TaskQueue)
	case p.workflowIDs[options.ID]:
		p.validationErr = fmt.Errorf("page or collection attempt reused workflow ID %s", options.ID)
	case inventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS:
		p.validationErr = fmt.Errorf("unexpected inventory status: %s", inventory.InventoryStatus)
	case inventory.InventoryPage.CurrentPage == inventory.InventoryPage.TotalPages && len(inventory.InventoryPage.ItemIds) != 1001:
		p.validationErr = fmt.Errorf("final page has %d IDs, want 1001", len(inventory.InventoryPage.ItemIds))
	case inventory.InventoryPage.CurrentPage != inventory.InventoryPage.TotalPages && len(inventory.InventoryPage.ItemIds) != 0:
		p.validationErr = fmt.Errorf("nonfinal page has %d IDs", len(inventory.InventoryPage.ItemIds))
	}
	if p.validationErr != nil {
		return nil, p.validationErr
	}
	p.workflowIDs[options.ID] = true
	p.pages++
	if !p.failed && inventory.InventoryPage.CurrentPage == p.failPage {
		p.failed = true
		return &inventoryRun{err: errors.New("injected receiver failure")}, nil
	}
	err := p.fixture.manager.UpdateSitePrefixesInDB(ctx, args[0].(uuid.UUID), inventory)
	return &inventoryRun{err: err}, nil
}

type inventoryRun struct {
	client.WorkflowRun
	err error
}

func (r *inventoryRun) Get(context.Context, any) error { return r.err }
