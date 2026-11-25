// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	"bytes"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/mocks"
)

func TestAPI_RegisterCron(t *testing.T) {
	previousMachine, previousWorkflow := ManagerAccess, workflow.ManagerAccess
	t.Cleanup(func() { ManagerAccess, workflow.ManagerAccess = previousMachine, previousWorkflow })

	e := elektratypes.NewElektraTypes()
	e.Conf.Temporal.TemporalSubscribeNamespace = "site"
	conf := &managerapi.ManagerConf{EB: e.Conf}
	workflow.NewWorkflowManager(e, nil, conf)
	api := NewMachineManager(e, nil, conf)

	const workflowID = "inventory-machine-site"
	const successMessage = "Machine: successfully registered Inventory Collect/Publish cron"
	for _, tc := range []struct {
		name     string
		startErr error
	}{
		{name: "start rejected", startErr: errors.New("machine inventory start rejected")},
		{name: "start accepted"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var logs bytes.Buffer
			e.Log = zerolog.New(&logs)
			subscriber := mocks.NewClient(t)
			e.Managers.Workflow.Temporal.Subscriber = subscriber
			var run client.WorkflowRun
			if tc.startErr == nil {
				started := mocks.NewWorkflowRun(t)
				started.On("GetID").Return(workflowID).Once()
				run = started
			}
			subscriber.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything).Return(run, tc.startErr).Once()

			api.RegisterCron()

			if tc.startErr != nil {
				require.Contains(t, logs.String(), tc.startErr.Error())
				require.NotContains(t, logs.String(), successMessage)
				return
			}
			require.Contains(t, logs.String(), successMessage)
			require.Contains(t, logs.String(), workflowID)
		})
	}
}
