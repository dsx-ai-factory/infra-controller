// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
)

type recordingForgeClient struct {
	corev1.ForgeClient

	mu                sync.Mutex
	runtimeConfig     *corev1.RuntimeConfig
	versionErr        error
	versionErrors     []error
	versionDelay      time.Duration
	machineIDDelay    time.Duration
	machineDelay      time.Duration
	switchIDDelay     time.Duration
	switchDelay       time.Duration
	shelfIDDelay      time.Duration
	shelfDelay        time.Duration
	versionRequests   []*corev1.VersionRequest
	machineSearches   []*corev1.MachineSearchConfig
	machineIDs        []string
	switchIDs         []string
	shelfIDs          []string
	switchResponseIDs []string
	machineBatches    [][]string
	switchBatches     [][]string
	shelfBatches      [][]string
	failCall          int
	omitID            string
}

func (c *recordingForgeClient) Version(
	ctx context.Context,
	request *corev1.VersionRequest,
	_ ...grpc.CallOption,
) (*corev1.BuildInfo, error) {
	c.mu.Lock()
	c.versionRequests = append(c.versionRequests, request)
	call := len(c.versionRequests)
	err := c.versionErr
	if call <= len(c.versionErrors) {
		err = c.versionErrors[call-1]
	}
	delay := c.versionDelay
	config := c.runtimeConfig
	c.mu.Unlock()

	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if err != nil {
		return nil, err
	}
	return &corev1.BuildInfo{BuildVersion: "test", RuntimeConfig: config}, nil
}

func (c *recordingForgeClient) FindMachineIds(
	ctx context.Context,
	request *corev1.MachineSearchConfig,
	_ ...grpc.CallOption,
) (*corev1.MachineIdList, error) {
	c.mu.Lock()
	c.machineSearches = append(c.machineSearches, request)
	delay := c.machineIDDelay
	c.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return &corev1.MachineIdList{MachineIds: stringsToMachineIds(c.machineIDs)}, nil
}

func (c *recordingForgeClient) FindMachinesByIds(
	ctx context.Context,
	request *corev1.MachinesByIdsRequest,
	_ ...grpc.CallOption,
) (*corev1.MachineList, error) {
	batch := protoIDsToStrings(request.GetMachineIds())
	c.mu.Lock()
	c.machineBatches = append(c.machineBatches, batch)
	failed := c.failCall == len(c.machineBatches)
	omitID := c.omitID
	delay := c.machineDelay
	c.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if failed {
		return nil, errors.New("injected machine lookup failure")
	}

	machines := make([]*corev1.Machine, 0, len(batch))
	for _, id := range batch {
		if id != omitID {
			machines = append(machines, &corev1.Machine{Id: &corev1.MachineId{Id: id}})
		}
	}
	return &corev1.MachineList{Machines: machines}, nil
}

func (c *recordingForgeClient) FindSwitchIds(
	ctx context.Context,
	_ *corev1.SwitchSearchFilter,
	_ ...grpc.CallOption,
) (*corev1.SwitchIdList, error) {
	if c.switchIDDelay > 0 {
		select {
		case <-time.After(c.switchIDDelay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return &corev1.SwitchIdList{Ids: stringsToSwitchIds(c.switchIDs)}, nil
}

func (c *recordingForgeClient) FindPowerShelfIds(
	ctx context.Context,
	_ *corev1.PowerShelfSearchFilter,
	_ ...grpc.CallOption,
) (*corev1.PowerShelfIdList, error) {
	if c.shelfIDDelay > 0 {
		select {
		case <-time.After(c.shelfIDDelay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return &corev1.PowerShelfIdList{Ids: stringsToPowerShelfIds(c.shelfIDs)}, nil
}

func (c *recordingForgeClient) FindSwitchesByIds(
	ctx context.Context,
	request *corev1.SwitchesByIdsRequest,
	_ ...grpc.CallOption,
) (*corev1.SwitchList, error) {
	batch := protoIDsToStrings(request.GetSwitchIds())
	c.mu.Lock()
	c.switchBatches = append(c.switchBatches, batch)
	failed := c.failCall == len(c.switchBatches)
	omitID := c.omitID
	delay := c.switchDelay
	responseIDs := batch
	if c.switchResponseIDs != nil {
		responseIDs = slices.Clone(c.switchResponseIDs)
	}
	c.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if failed {
		return nil, errors.New("injected switch lookup failure")
	}

	switches := make([]*corev1.Switch, 0, len(responseIDs))
	for _, id := range responseIDs {
		if omitID == "" || id != omitID {
			nvosIP := "ip-" + id
			bmcMAC := "mac-" + id
			switches = append(switches, &corev1.Switch{
				Id:              &corev1.SwitchId{Id: id},
				RackId:          &corev1.RackId{Id: "rack-" + id},
				ControllerState: "state-" + id,
				NvosInfo:        &corev1.SwitchNvosInfo{Ip: &nvosIP},
				BmcInfo:         &corev1.BmcInfo{Mac: &bmcMAC},
			})
		}
	}
	return &corev1.SwitchList{Switches: switches}, nil
}

func (c *recordingForgeClient) FindPowerShelvesByIds(
	ctx context.Context,
	request *corev1.PowerShelvesByIdsRequest,
	_ ...grpc.CallOption,
) (*corev1.PowerShelfList, error) {
	batch := protoIDsToStrings(request.GetPowerShelfIds())
	c.mu.Lock()
	c.shelfBatches = append(c.shelfBatches, batch)
	failed := c.failCall == len(c.shelfBatches)
	omitID := c.omitID
	delay := c.shelfDelay
	c.mu.Unlock()
	if delay > 0 {
		select {
		case <-time.After(delay):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if failed {
		return nil, errors.New("injected power shelf lookup failure")
	}

	shelves := make([]*corev1.PowerShelf, 0, len(batch))
	for _, id := range batch {
		if id != omitID {
			bmcMAC := "mac-" + id
			shelves = append(shelves, &corev1.PowerShelf{
				Id:              &corev1.PowerShelfId{Id: id},
				RackId:          &corev1.RackId{Id: "rack-" + id},
				ControllerState: "state-" + id,
				BmcInfo:         &corev1.BmcInfo{Mac: &bmcMAC},
			})
		}
	}
	return &corev1.PowerShelfList{PowerShelves: shelves}, nil
}

func newRecordingGRPCClient(fake *recordingForgeClient) *grpcClient {
	return &grpcClient{gclient: newBatchingForgeClient(fake), grpcTimeout: time.Second}
}

func stringsToSwitchIds(ids []string) []*corev1.SwitchId {
	result := make([]*corev1.SwitchId, 0, len(ids))
	for _, id := range ids {
		result = append(result, &corev1.SwitchId{Id: id})
	}
	return result
}

func stringsToPowerShelfIds(ids []string) []*corev1.PowerShelfId {
	result := make([]*corev1.PowerShelfId, 0, len(ids))
	for _, id := range ids {
		result = append(result, &corev1.PowerShelfId{Id: id})
	}
	return result
}

func TestGrpcClient_GetMachines(t *testing.T) {
	ids := []string{"a", "b", "c", "d"}
	tests := []struct {
		name        string
		fake        *recordingForgeClient
		grpcTimeout time.Duration
		check       func(*testing.T, *recordingForgeClient, []MachineDetail, error)
	}{
		{
			name: "includes DPUs in ID search",
			fake: &recordingForgeClient{},
			check: func(t *testing.T, fake *recordingForgeClient, _ []MachineDetail, err error) {
				require.NoError(t, err)
				require.Len(t, fake.machineSearches, 1)
				assert.True(t, fake.machineSearches[0].GetIncludeDpus())
			},
		},
		{
			name: "gives ID and detail RPCs independent timeouts",
			fake: &recordingForgeClient{
				machineIDs:     []string{"machine-1"},
				machineIDDelay: 120 * time.Millisecond,
				machineDelay:   120 * time.Millisecond,
			},
			grpcTimeout: 200 * time.Millisecond,
			check: func(t *testing.T, _ *recordingForgeClient, machines []MachineDetail, err error) {
				require.NoError(t, err)
				assert.Len(t, machines, 1)
			},
		},
		{
			name: "rejects a partial projected snapshot",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				machineIDs:    ids,
				failCall:      2,
			},
			check: func(t *testing.T, _ *recordingForgeClient, machines []MachineDetail, err error) {
				require.Error(t, err)
				assert.Nil(t, machines)
			},
		},
		{
			name: "honors the Core batch limit",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				machineIDs:    []string{"a", "b", "c", "d", "e"},
			},
			check: func(t *testing.T, fake *recordingForgeClient, machines []MachineDetail, err error) {
				require.NoError(t, err)
				assert.Len(t, machines, 5)
				assert.Equal(t, [][]string{{"a", "b"}, {"c", "d"}, {"e"}}, fake.machineBatches)
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			client := newRecordingGRPCClient(test.fake)
			if test.grpcTimeout != 0 {
				client.grpcTimeout = test.grpcTimeout
			}

			machines, err := client.GetMachines(context.Background())

			test.check(t, test.fake, machines, err)
		})
	}
}

func TestGrpcClient_ActualInventoryRPCsHaveIndependentTimeouts(t *testing.T) {
	testCases := []struct {
		name   string
		fake   *recordingForgeClient
		invoke func(context.Context, *grpcClient) (int, error)
	}{
		{
			name: "switches",
			fake: &recordingForgeClient{
				switchIDs:     []string{"switch-1"},
				switchIDDelay: 120 * time.Millisecond,
				switchDelay:   120 * time.Millisecond,
			},
			invoke: func(ctx context.Context, client *grpcClient) (int, error) {
				devices, err := client.GetSwitches(ctx)
				return len(devices), err
			},
		},
		{
			name: "power shelves",
			fake: &recordingForgeClient{
				shelfIDs:     []string{"shelf-1"},
				shelfIDDelay: 120 * time.Millisecond,
				shelfDelay:   120 * time.Millisecond,
			},
			invoke: func(ctx context.Context, client *grpcClient) (int, error) {
				devices, err := client.GetPowerShelves(ctx)
				return len(devices), err
			},
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			client := newRecordingGRPCClient(tc.fake)
			client.grpcTimeout = 200 * time.Millisecond

			count, err := tc.invoke(context.Background(), client)

			require.NoError(t, err)
			assert.Equal(t, 1, count)
		})
	}
}

func TestGrpcClient_ActualInventoryEmptyIDsAvoidDetailLookup(t *testing.T) {
	testCases := []struct {
		name       string
		invoke     func(context.Context, *grpcClient) ([]ObservedControllerDevice, error)
		batchCalls func(*recordingForgeClient) [][]string
	}{
		{
			name: "switches",
			invoke: func(ctx context.Context, client *grpcClient) ([]ObservedControllerDevice, error) {
				return client.GetSwitches(ctx)
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "power shelves",
			invoke: func(ctx context.Context, client *grpcClient) ([]ObservedControllerDevice, error) {
				return client.GetPowerShelves(ctx)
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.shelfBatches },
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			fake := &recordingForgeClient{}

			devices, err := tc.invoke(context.Background(), newRecordingGRPCClient(fake))

			require.NoError(t, err)
			assert.Empty(t, devices)
			assert.Empty(t, tc.batchCalls(fake))
			assert.Empty(t, fake.versionRequests)
		})
	}
}

func TestGrpcClient_ByIDLookupsHonorCoreBatchLimit(t *testing.T) {
	ids := []string{"a", "b", "c", "d", "e"}
	expectedBatches := [][]string{{"a", "b"}, {"c", "d"}, {"e"}}

	tests := []struct {
		name       string
		invoke     func(context.Context, *grpcClient, []string) (int, error)
		batchCalls func(*recordingForgeClient) [][]string
	}{
		{
			name: "machines by IDs",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				machines, err := client.FindMachinesByIds(ctx, ids)
				return len(machines), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.machineBatches },
		},
		{
			name: "direct switch lookup",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				request := &corev1.SwitchesByIdsRequest{
					SwitchIds: make([]*corev1.SwitchId, 0, len(ids)),
				}
				for _, id := range ids {
					request.SwitchIds = append(request.SwitchIds, &corev1.SwitchId{Id: id})
				}
				response, err := client.gclient.FindSwitchesByIds(ctx, request)
				return len(response.GetSwitches()), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "active switches",
			invoke: func(ctx context.Context, client *grpcClient, _ []string) (int, error) {
				values, err := client.GetSwitches(ctx)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "switch rack IDs",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				values, err := client.FindSwitchRackIDs(ctx, ids)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "switch controller states",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				values, err := client.FindSwitchControllerStates(ctx, ids)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "switch NVOS IPs",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				values, err := client.FindSwitchNvosIPs(ctx, ids)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.switchBatches },
		},
		{
			name: "active power shelves",
			invoke: func(ctx context.Context, client *grpcClient, _ []string) (int, error) {
				values, err := client.GetPowerShelves(ctx)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.shelfBatches },
		},
		{
			name: "power shelf rack IDs",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				values, err := client.FindPowerShelfRackIDs(ctx, ids)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.shelfBatches },
		},
		{
			name: "power shelf controller states",
			invoke: func(ctx context.Context, client *grpcClient, ids []string) (int, error) {
				values, err := client.FindPowerShelfControllerStates(ctx, ids)
				return len(values), err
			},
			batchCalls: func(fake *recordingForgeClient) [][]string { return fake.shelfBatches },
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			fake := &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				machineIDs:    ids,
				switchIDs:     ids,
				shelfIDs:      ids,
			}
			count, err := test.invoke(context.Background(), newRecordingGRPCClient(fake), ids)

			require.NoError(t, err)
			assert.Equal(t, len(ids), count)
			assert.Equal(t, expectedBatches, test.batchCalls(fake))
			require.Len(t, fake.versionRequests, 1)
			assert.True(t, fake.versionRequests[0].GetDisplayConfig())
		})
	}
}

func TestGrpcClient_ByIDLookupsTreatZeroOrAbsentLimitAsServerUnlimited(t *testing.T) {
	ids := []string{"a", "b", "c"}
	tests := []struct {
		name   string
		config *corev1.RuntimeConfig
	}{
		{name: "zero", config: &corev1.RuntimeConfig{MaxFindByIds: 0}},
		{name: "absent", config: nil},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			fake := &recordingForgeClient{runtimeConfig: test.config}
			result, err := newRecordingGRPCClient(fake).FindSwitchRackIDs(context.Background(), ids)

			require.NoError(t, err)
			assert.Len(t, result, len(ids))
			assert.Equal(t, [][]string{ids}, fake.switchBatches)
		})
	}
}

func TestGrpcClient_ByIDLookupsApplyFlowClientBatchCap(t *testing.T) {
	ids := make([]string, 0, flowFindByIDsBatchSize*2+1)
	for i := range flowFindByIDsBatchSize*2 + 1 {
		ids = append(ids, fmt.Sprintf("switch-%03d", i))
	}

	tests := []struct {
		name            string
		coreLimit       uint32
		expectedLengths []int
	}{
		{
			name:            "unlimited Core still uses Flow cap",
			coreLimit:       0,
			expectedLengths: []int{flowFindByIDsBatchSize, flowFindByIDsBatchSize, 1},
		},
		{
			name:            "larger Core limit uses Flow cap",
			coreLimit:       flowFindByIDsBatchSize + 50,
			expectedLengths: []int{flowFindByIDsBatchSize, flowFindByIDsBatchSize, 1},
		},
		{
			name:            "smaller Core limit remains authoritative",
			coreLimit:       60,
			expectedLengths: []int{60, 60, 60, 21},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			fake := &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: test.coreLimit},
			}

			result, err := newRecordingGRPCClient(fake).FindSwitchRackIDs(context.Background(), ids)

			require.NoError(t, err)
			assert.Len(t, result, len(ids))
			require.Len(t, fake.switchBatches, len(test.expectedLengths))
			for i, expectedLength := range test.expectedLengths {
				assert.Len(t, fake.switchBatches[i], expectedLength)
			}
		})
	}
}

func TestVisitFindByIDBatches(t *testing.T) {
	tests := []struct {
		name       string
		visitError bool
		wantEvents []string
		wantError  string
	}{
		{
			name:       "projects each batch before fetching the next",
			wantEvents: []string{"fetch:a,b", "visit:a,b", "fetch:c,d", "visit:c,d"},
		},
		{
			name:       "projection failure stops the lookup",
			visitError: true,
			wantEvents: []string{"fetch:a,b", "visit:a,b"},
			wantError:  "injected projection failure",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var events []string
			err := visitFindByIDBatches(
				context.Background(),
				"FindByIds",
				func(context.Context) (uint32, error) { return 2, nil },
				[]string{"a", "b", "c", "d"},
				func(_ context.Context, batch []string) ([]string, error) {
					events = append(events, "fetch:"+strings.Join(batch, ","))
					return slices.Clone(batch), nil
				},
				func(value string) string { return value },
				func(batch []string) error {
					events = append(events, "visit:"+strings.Join(batch, ","))
					if test.visitError {
						return errors.New("injected projection failure")
					}
					return nil
				},
			)

			assert.Equal(t, test.wantEvents, events)
			if test.wantError == "" {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, test.wantError)
			}
		})
	}
}

func TestVisitFindByIDBatchesRejectsInvalidRequestsBeforeFetch(t *testing.T) {
	tests := []struct {
		name      string
		ids       []string
		wantError string
	}{
		{
			name:      "empty ID",
			ids:       []string{"a", ""},
			wantError: "FindByIds request contains an empty ID",
		},
		{
			name:      "duplicate ID across prospective batches",
			ids:       []string{"a", "b", "a"},
			wantError: "FindByIds request contains duplicate ID: a",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			loadedLimit := false
			fetched := false
			visited := false
			err := visitFindByIDBatches(
				context.Background(),
				"FindByIds",
				func(context.Context) (uint32, error) {
					loadedLimit = true
					return 2, nil
				},
				test.ids,
				func(context.Context, []string) ([]string, error) {
					fetched = true
					return nil, nil
				},
				func(value string) string { return value },
				func([]string) error {
					visited = true
					return nil
				},
			)

			require.ErrorContains(t, err, test.wantError)
			assert.False(t, loadedLimit)
			assert.False(t, fetched)
			assert.False(t, visited)
		})
	}
}

func TestValidateByIDsResponse(t *testing.T) {
	tests := []struct {
		name      string
		requested []string
		returned  []string
		wantError string
	}{
		{
			name:      "exact identity set",
			requested: []string{"a", "b"},
			returned:  []string{"b", "a"},
		},
		{
			name:      "empty returned ID",
			requested: []string{"a"},
			returned:  []string{""},
			wantError: "FindByIds returned an empty ID",
		},
		{
			name:      "duplicate returned ID",
			requested: []string{"a", "b"},
			returned:  []string{"a", "a", "b"},
			wantError: "FindByIds returned duplicate ID: a",
		},
		{
			name:      "unrequested returned ID",
			requested: []string{"a"},
			returned:  []string{"a", "b"},
			wantError: "FindByIds returned unrequested ID: b",
		},
		{
			name:      "missing returned ID",
			requested: []string{"a", "b"},
			returned:  []string{"a"},
			wantError: "FindByIds returned an incomplete response; missing IDs: b",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateByIDsResponse(test.requested, test.returned, "FindByIds")

			if test.wantError == "" {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, test.wantError)
			}
		})
	}
}

func TestGrpcClient_ByIDLookupsRejectInvalidReturnedIdentitiesBeforeProjection(t *testing.T) {
	tests := []struct {
		name        string
		returnedIDs []string
		wantError   string
	}{
		{
			name:        "empty ID",
			returnedIDs: []string{"", "b"},
			wantError:   "returned an empty ID",
		},
		{
			name:        "duplicate ID",
			returnedIDs: []string{"a", "a", "b"},
			wantError:   "returned duplicate ID: a",
		},
		{
			name:        "unrequested ID",
			returnedIDs: []string{"a", "unexpected"},
			wantError:   "returned unrequested ID: unexpected",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			fake := &recordingForgeClient{
				runtimeConfig:     &corev1.RuntimeConfig{MaxFindByIds: 2},
				switchResponseIDs: test.returnedIDs,
			}

			result, err := newRecordingGRPCClient(fake).FindSwitchRackIDs(
				context.Background(),
				[]string{"a", "b"},
			)

			require.ErrorContains(t, err, test.wantError)
			assert.Nil(t, result)
		})
	}
}

func TestGrpcClient_ByIDLookupsRejectPartialResults(t *testing.T) {
	ids := []string{"a", "b", "c", "d"}
	tests := []struct {
		name        string
		fake        *recordingForgeClient
		errorString string
	}{
		{
			name: "runtime config lookup fails",
			fake: &recordingForgeClient{
				versionErr: errors.New("injected Version failure"),
			},
			errorString: "injected Version failure",
		},
		{
			name: "batch RPC fails",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				failCall:      2,
			},
			errorString: "injected switch lookup failure",
		},
		{
			name: "batch response is incomplete",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				omitID:        "c",
			},
			errorString: "incomplete response; missing IDs: c",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			result, err := newRecordingGRPCClient(test.fake).FindSwitchRackIDs(context.Background(), ids)

			require.Error(t, err)
			assert.ErrorContains(t, err, test.errorString)
			assert.Nil(t, result)
		})
	}
}

func TestGrpcClient_ActualInventoryRejectsPartialProjectedSnapshots(t *testing.T) {
	ids := []string{"a", "b", "c", "d"}
	tests := []struct {
		name   string
		fake   *recordingForgeClient
		invoke func(context.Context, *grpcClient) (any, error)
	}{
		{
			name: "switches",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				switchIDs:     ids,
				failCall:      2,
			},
			invoke: func(ctx context.Context, client *grpcClient) (any, error) {
				return client.GetSwitches(ctx)
			},
		},
		{
			name: "power shelves",
			fake: &recordingForgeClient{
				runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
				shelfIDs:      ids,
				failCall:      2,
			},
			invoke: func(ctx context.Context, client *grpcClient) (any, error) {
				return client.GetPowerShelves(ctx)
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			result, err := test.invoke(context.Background(), newRecordingGRPCClient(test.fake))

			require.Error(t, err)
			assert.Nil(t, result)
		})
	}
}

func TestGrpcClient_ByIDLookupsCacheSuccessfulCoreLimit(t *testing.T) {
	fake := &recordingForgeClient{
		runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
	}
	client := newRecordingGRPCClient(fake)

	_, err := client.FindSwitchRackIDs(context.Background(), []string{"s1", "s2", "s3"})
	require.NoError(t, err)
	_, err = client.FindPowerShelfRackIDs(context.Background(), []string{"p1", "p2", "p3"})
	require.NoError(t, err)

	require.Len(t, fake.versionRequests, 1, "all resource lookups must share the cached Core limit")
}

func TestGrpcClient_ByIDLookupsRetryFailedCoreLimitLoad(t *testing.T) {
	fake := &recordingForgeClient{
		runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
		versionErrors: []error{
			errors.New("transient Version failure"),
			nil,
		},
	}
	client := newRecordingGRPCClient(fake)

	result, err := client.FindSwitchRackIDs(context.Background(), []string{"a", "b", "c"})
	require.ErrorContains(t, err, "transient Version failure")
	assert.Nil(t, result)

	result, err = client.FindSwitchRackIDs(context.Background(), []string{"a", "b", "c"})
	require.NoError(t, err)
	assert.Len(t, result, 3)
	require.Len(t, fake.versionRequests, 2, "a failed limit load must not be cached")
}

func TestGrpcClient_ByIDLookupsCoalesceConcurrentCoreLimitLoads(t *testing.T) {
	fake := &recordingForgeClient{
		runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 2},
		versionDelay:  20 * time.Millisecond,
	}
	client := newRecordingGRPCClient(fake)

	const callers = 8
	start := make(chan struct{})
	errs := make(chan error, callers)
	var wg sync.WaitGroup
	for range callers {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			_, err := client.FindSwitchRackIDs(context.Background(), []string{"a", "b", "c"})
			errs <- err
		}()
	}
	close(start)
	wg.Wait()
	close(errs)

	for err := range errs {
		require.NoError(t, err)
	}
	require.Len(t, fake.versionRequests, 1, "concurrent first lookups must share one Version RPC")
}

// One public lookup deadline covers the limit discovery and every batch. If
// earlier RPCs consume that budget, a later batch fails the whole lookup and
// no partial result is returned.
func TestGrpcClient_ByIDLookupsFailWithoutPartialResultWhenDeadlineStarvesLaterBatch(t *testing.T) {
	fake := &recordingForgeClient{
		runtimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 1},
		versionDelay:  10 * time.Millisecond,
		switchDelay:   15 * time.Millisecond,
	}
	client := newRecordingGRPCClient(fake)
	client.grpcTimeout = 35 * time.Millisecond

	result, err := client.FindSwitchRackIDs(context.Background(), []string{"a", "b", "c"})

	require.ErrorIs(t, err, context.DeadlineExceeded)
	assert.Nil(t, result)
	assert.Less(t, len(fake.switchBatches), 3, "the exhausted operation deadline must prevent all batches from completing")
}
