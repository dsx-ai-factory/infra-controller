// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestValidateSpectrumXMachine(t *testing.T) {
	spectrumX := corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X
	capabilities := &corev1.MachineCapabilitiesSet{Network: []*corev1.MachineCapabilityAttributesNetwork{
		{Name: "ConnectX-8", Count: 2, DeviceType: &spectrumX},
		{Name: "BlueField-3", Count: 1, DeviceType: &spectrumX},
		{Name: "ordinary NIC", Count: 4},
	}}
	machine := &corev1.Machine{Status: &corev1.MachineStatus{Capabilities: capabilities}}
	attachment := func(device string, index int) cam.APISpectrumXAttachmentCreateOrUpdateRequest {
		return cam.APISpectrumXAttachmentCreateOrUpdateRequest{Device: device, DeviceInstance: &index}
	}
	for _, test := range []struct {
		name        string
		machine     *corev1.Machine
		attachments []cam.APISpectrumXAttachmentCreateOrUpdateRequest
		status      int
	}{
		{"all requested groups match", machine, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 1), attachment("BlueField-3", 0)}, 0},
		{"last attachment exceeds count", machine, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 1), attachment("BlueField-3", 1)}, http.StatusBadRequest},
		{"exact name required", machine, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("connectx-8", 0)}, http.StatusBadRequest},
		{"network capability without SpectrumX type", machine, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ordinary NIC", 0)}, http.StatusBadRequest},
		{"wide ordinal cannot wrap to zero", machine, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 1<<32)}, http.StatusBadRequest},
		{"missing Core machine", nil, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 0)}, http.StatusConflict},
		{"deprecated capabilities cannot satisfy request", &corev1.Machine{Capabilities: capabilities}, []cam.APISpectrumXAttachmentCreateOrUpdateRequest{attachment("ConnectX-8", 0)}, http.StatusBadRequest},
	} {
		t.Run(test.name, func(t *testing.T) {
			apiErr := validateSpectrumXMachine(test.machine, test.attachments)
			if test.status == 0 {
				require.Nil(t, apiErr)
				return
			}
			require.NotNil(t, apiErr)
			assert.Equal(t, test.status, apiErr.Code)
		})
	}
}

func TestFindSpectrumXMachines(t *testing.T) {
	for _, test := range []struct {
		name          string
		count         int
		fail          bool
		cancelled     bool
		cancelOnReply bool
	}{
		{name: "no IDs needs no workflow"},
		{name: "bounded batches correlate unordered partial responses", count: 12},
		{name: "one failed batch discards all results", count: 12, fail: true},
		{name: "expired caller cannot dispatch or return partial success", count: 12, cancelled: true},
		{name: "cancellation during discovery discards a successful reply", count: 1, cancelOnReply: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			client := &tmocks.Client{}
			var mu sync.Mutex
			var requested []string
			ctx, cancel := context.WithTimeout(context.Background(), time.Second*5)
			defer cancel()
			succeeded := make(chan struct{})
			var succeedOnce sync.Once
			ids := make([]string, test.count)
			for i := range ids {
				ids[i] = fmt.Sprintf("machine-%02d", i)
			}
			input := append([]string{}, ids...)
			input = append(input, ids...)
			client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
				Return(func(_ context.Context, _ tclient.StartWorkflowOptions, _ interface{}, args ...interface{}) tclient.WorkflowRun {
					request := args[0].(grpcproxy.Request)
					assert.Equal(t, corev1.Forge_FindMachinesByIds_FullMethodName, request.FullMethod)
					var byIDs corev1.MachinesByIdsRequest
					require.NoError(t, protojson.Unmarshal(request.RequestJSON, &byIDs))
					require.LessOrEqual(t, len(byIDs.MachineIds), spectrumXDiscoveryBatchSize)
					assert.False(t, byIDs.IncludeHistory)
					response := &corev1.MachineList{}
					mu.Lock()
					for _, id := range byIDs.MachineIds {
						requested = append(requested, id.Id)
						if id.Id != "machine-01" {
							response.Machines = append([]*corev1.Machine{{Id: id}}, response.Machines...)
						}
					}
					mu.Unlock()
					response.Machines = append(response.Machines, &corev1.Machine{Id: &corev1.MachineId{Id: "unsolicited"}})
					run := &tmocks.WorkflowRun{}
					if test.fail && byIDs.MachineIds[0].Id == "machine-00" {
						run.On("Get", mock.Anything, mock.Anything).Run(func(mock.Arguments) {
							select {
							case <-succeeded:
							case <-ctx.Done():
								t.Error("no successful batch before injected failure")
							}
						}).Return(context.DeadlineExceeded)
					} else {
						data, err := protojson.Marshal(response)
						require.NoError(t, err)
						run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
							args.Get(1).(*grpcproxy.Response).ResponseJSON = data
							succeedOnce.Do(func() { close(succeeded) })
							if test.cancelOnReply {
								cancel()
							}
						}).Return(nil)
					}
					return run
				}, nil).Maybe()
			if test.cancelled {
				cancel()
			}
			machines, apiErr := findSpectrumXMachines(ctx, client, uuid.New(), input)
			if test.fail || test.cancelled || test.cancelOnReply {
				require.NotNil(t, apiErr)
				assert.Nil(t, machines)
			} else {
				require.Nil(t, apiErr)
				assert.ElementsMatch(t, ids, requested)
				for _, id := range ids {
					if id != "machine-01" {
						require.Contains(t, machines, id)
						assert.Equal(t, id, machines[id].GetId().GetId())
					}
				}
				assert.NotContains(t, machines, "machine-01")
				assert.NotContains(t, machines, "unsolicited")
			}
			if test.count == 0 || test.cancelled {
				client.AssertNotCalled(t, "ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			}
		})
	}
}

func TestSpectrumXDiscoveryBatchPayload(t *testing.T) {
	// Inventory's measured pre-pruning size is about 156KB per machine.
	// Exercise the actual JSON transport envelope at that representative size;
	// this is headroom evidence, not a bound on every possible Machine response.
	response := &corev1.MachineList{}
	for range spectrumXDiscoveryBatchSize {
		response.Machines = append(response.Machines, &corev1.Machine{State: strings.Repeat("x", 156*1024)})
	}
	data, err := protojson.Marshal(response)
	require.NoError(t, err)
	envelope, err := json.Marshal(grpcproxy.Response{ResponseJSON: data})
	require.NoError(t, err)
	assert.Less(t, len(envelope), 1024*1024)
}
