// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers_test

import (
	"context"
	"errors"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
)

type inventoryWorker struct {
	worker.Worker
	workflow any
	activity any
}

func (w *inventoryWorker) RegisterWorkflow(fn any) { w.workflow = fn }
func (w *inventoryWorker) RegisterActivity(fn any) { w.activity = fn }

func TestSitePrefix_RegisterPublisher(t *testing.T) {
	for _, tc := range []struct {
		name     string
		schedule string
		wantCron string
		startErr error
	}{
		{name: "default schedule publishes empty inventory", wantCron: "@every 3m"},
		{name: "configured schedule", schedule: "@every 5m", wantCron: "@every 5m"},
		{name: "cron failure surfaces", wantCron: "@every 3m", startErr: errors.New("Temporal unavailable")},
	} {
		t.Run(tc.name, func(t *testing.T) {
			e := elektratypes.NewElektraTypes()
			e.Conf.Temporal.ClusterID = uuid.NewString()
			e.Conf.Temporal.TemporalSubscribeNamespace = "site"
			e.Conf.Temporal.TemporalSubscribeQueue = "inventory"
			e.Conf.Temporal.TemporalPublishQueue = "cloud"
			e.Conf.Temporal.TemporalInventorySchedule = tc.schedule
			manager, err := managers.NewInstance(e)
			require.NoError(t, err)
			require.NotNil(t, manager.API.SitePrefix)
			manager.SitePrefix().Init()
			recorder := &inventoryWorker{}
			subscriber, publisher := &mocks.Client{}, &mocks.Client{}
			e.Managers.Workflow.Temporal.Worker = recorder
			e.Managers.Workflow.Temporal.Subscriber = subscriber
			e.Managers.Workflow.Temporal.Publisher = publisher
			e.Managers.CoreGrpc.Client = cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
			e.Managers.CoreGrpc.Client.SwapClient(cClient.NewMockCoreGrpcClient())
			run := &mocks.WorkflowRun{}
			run.On("GetID").Return("cron")
			subscriber.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(options client.StartWorkflowOptions) bool {
				return options.ID == "inventory-site-prefix-site" && options.TaskQueue == "inventory" && options.CronSchedule == tc.wantCron
			}), mock.Anything).Run(func(args mock.Arguments) {
				require.Equal(t, "DiscoverSitePrefixInventory", temporalFunctionName(args[2]))
			}).Return(run, tc.startErr).Once()
			err = manager.API.SitePrefix.RegisterPublisher()
			require.ErrorIs(t, err, tc.startErr)
			require.Equal(t, "DiscoverSitePrefixInventory", temporalFunctionName(recorder.workflow))
			require.Equal(t, "DiscoverSitePrefixInventory", temporalFunctionName(recorder.activity))
			subscriber.AssertExpectations(t)
			if tc.startErr != nil {
				return
			}

			cloudRun := &mocks.WorkflowRun{}
			cloudRun.On("Get", mock.Anything, nil).Return(nil).Once()
			publisher.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(options client.StartWorkflowOptions) bool {
				return options.TaskQueue == "cloud"
			}), "UpdateSitePrefixInventory", uuid.MustParse(e.Conf.Temporal.ClusterID),
				mock.MatchedBy(func(inventory *corev1.SitePrefixInventory) bool {
					return inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS && inventory.InventoryPage.TotalItems == 0
				})).Return(cloudRun, nil).Once()
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.SetWorkerOptions(worker.Options{BackgroundActivityContext: cClient.WithMockSitePrefixMaxFindByIDs(context.Background(), 100)})
			env.RegisterWorkflow(recorder.workflow)
			env.RegisterActivity(recorder.activity)
			env.ExecuteWorkflow("DiscoverSitePrefixInventory")
			require.NoError(t, env.GetWorkflowError())
			publisher.AssertExpectations(t)
			cloudRun.AssertExpectations(t)
		})
	}
}
