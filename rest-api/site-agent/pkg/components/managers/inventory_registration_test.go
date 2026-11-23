// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers_test

import (
	"bytes"
	"encoding/json"
	"errors"
	"reflect"
	"runtime"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedmachine"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedpowershelf"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedrack"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedswitch"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	workflowtypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/workflow"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/worker"
)

type subscriberWorker struct {
	worker.Worker
	activityNames []string
}

func (w *subscriberWorker) RegisterWorkflow(any) {}

func (w *subscriberWorker) RegisterActivity(fn any) {
	w.activityNames = append(w.activityNames, temporalFunctionName(fn))
}

func (w *subscriberWorker) legacyFlowActivityNames() []string {
	var names []string
	for _, name := range w.activityNames {
		if strings.HasSuffix(name, "OnFlow") {
			names = append(names, name)
		}
	}
	return names
}

func temporalFunctionName(fn any) string {
	// `RegisterActivity` exposes a method's short name to Temporal. The worker
	// interface has no registry lookup, so mirror that naming rule while we
	// record the real subscriber calls.
	fullName := runtime.FuncForPC(reflect.ValueOf(fn).Pointer()).Name()
	shortName := fullName[strings.LastIndex(fullName, ".")+1:]
	return strings.TrimSuffix(shortName, "-fm")
}

// In-flight pre-cutover executions can still schedule these activity names, so
// current site-agent workers must keep their no-op handlers registered.
func TestExpectedInventorySubscribersRegisterLegacyFlowActivities(t *testing.T) {
	tests := []struct {
		name     string
		register func(*elektratypes.Elektra) error
		expected []string
	}{
		{
			name: "expected machine",
			register: func(e *elektratypes.Elektra) error {
				return expectedmachine.NewExpectedMachineManager(e, nil, nil).RegisterSubscriber()
			},
			expected: []string{
				"CreateExpectedMachineOnFlow",
				"CreateExpectedMachinesOnFlow",
			},
		},
		{
			name: "expected switch",
			register: func(e *elektratypes.Elektra) error {
				return expectedswitch.NewExpectedSwitchManager(e, nil, nil).RegisterSubscriber()
			},
			expected: []string{"CreateExpectedSwitchOnFlow"},
		},
		{
			name: "expected power shelf",
			register: func(e *elektratypes.Elektra) error {
				return expectedpowershelf.NewExpectedPowerShelfManager(e, nil, nil).RegisterSubscriber()
			},
			expected: []string{"CreateExpectedPowerShelfOnFlow"},
		},
		{
			name: "expected rack",
			register: func(e *elektratypes.Elektra) error {
				return expectedrack.NewExpectedRackManager(e, nil, nil).RegisterSubscriber()
			},
			expected: []string{"CreateExpectedRackOnFlow"},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			temporalWorker := &subscriberWorker{}
			elektra := elektratypes.NewElektraTypes()
			elektra.Managers.Workflow.Temporal = workflowtypes.Temporal{Worker: temporalWorker}

			require.NoError(t, tc.register(elektra))
			require.ElementsMatch(t, tc.expected, temporalWorker.legacyFlowActivityNames())
		})
	}
}

func TestInventoryPublishers_RegisterPublisher(t *testing.T) {
	e := elektratypes.NewElektraTypes()
	e.Conf.Temporal.ClusterID = uuid.NewString()
	manager, err := managers.NewInstance(e)
	require.NoError(t, err)
	e.Managers.Workflow.Temporal.Worker = &inventoryWorker{}
	startErr := errors.New("Temporal unavailable")

	tests := []struct {
		name     string
		register func() error
	}{
		{"VPC", manager.API.VPC.RegisterPublisher},
		{"VpcPrefix", manager.API.VpcPrefix.RegisterPublisher},
		{"VpcPeering", manager.API.VpcPeering.RegisterPublisher},
		{"Subnet", manager.API.Subnet.RegisterPublisher},
		{"InfiniBandPartition", manager.API.InfiniBandPartition.RegisterPublisher},
		{"SSHKeyGroup", manager.API.SSHKeyGroup.RegisterPublisher},
		{"Instance", manager.API.Instance.RegisterPublisher},
		{"Tenant", manager.API.Tenant.RegisterPublisher},
		{"InstanceType", manager.API.InstanceType.RegisterPublisher},
		{"NetworkSecurityGroup", manager.API.NetworkSecurityGroup.RegisterPublisher},
		{"ExpectedMachine", manager.API.ExpectedMachine.RegisterPublisher},
		{"ExpectedPowerShelf", manager.API.ExpectedPowerShelf.RegisterPublisher},
		{"ExpectedSwitch", manager.API.ExpectedSwitch.RegisterPublisher},
		{"SKU", manager.API.SKU.RegisterPublisher},
		{"DpuExtensionService", manager.API.DpuExtensionService.RegisterPublisher},
		{"NVLinkLogicalPartition", manager.API.NVLinkLogicalPartition.RegisterPublisher},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var logs bytes.Buffer
			e.Log = zerolog.New(&logs).Level(zerolog.ErrorLevel)
			subscriber := mocks.NewClient(t)
			e.Managers.Workflow.Temporal.Subscriber = subscriber
			subscriber.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything).Return(nil, startErr).Once()

			require.ErrorIs(t, tt.register(), startErr)
			var event struct {
				Error string `json:"error"`
			}
			require.NoError(t, json.Unmarshal(logs.Bytes(), &event), "expected a single error log")
			require.Equal(t, startErr.Error(), event.Error)
		})
	}
}
