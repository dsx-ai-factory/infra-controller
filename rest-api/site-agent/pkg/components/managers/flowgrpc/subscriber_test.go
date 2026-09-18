// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	"reflect"
	"runtime"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/worker"
)

type recordingWorker struct {
	worker.Worker
	workflowNames []string
	activityNames []string
}

func (w *recordingWorker) RegisterWorkflow(fn any) {
	w.workflowNames = append(w.workflowNames, registeredFunctionName(fn))
}

func (w *recordingWorker) RegisterActivity(fn any) {
	w.activityNames = append(w.activityNames, registeredFunctionName(fn))
}

func registeredFunctionName(fn any) string {
	fullName := runtime.FuncForPC(reflect.ValueOf(fn).Pointer()).Name()
	shortName := fullName[strings.LastIndex(fullName, ".")+1:]
	return strings.TrimSuffix(shortName, "-fm")
}

func TestAPI_RegisterSubscriber(t *testing.T) {
	temporalWorker := &recordingWorker{}
	elektra := elektratypes.NewElektraTypes()
	elektra.Conf.FlowGrpc.Enabled = true
	elektra.Managers.Workflow.Temporal = workflowtypes.Temporal{Worker: temporalWorker}
	manager := NewFlowGrpcManager(elektra, nil, &managerapi.ManagerConf{EB: elektra.Conf})

	require.NoError(t, manager.RegisterSubscriber())
	require.Equal(t, []string{"InvokeFlowGRPC"}, temporalWorker.workflowNames)
	require.Equal(t, []string{"InvokeFlowGRPCOnSite"}, temporalWorker.activityNames)
}
