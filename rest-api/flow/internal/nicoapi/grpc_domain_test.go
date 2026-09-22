// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type deadlineTrackingForgeClient struct {
	corev1.ForgeClient
	deadlines []time.Time
}

func (c *deadlineTrackingForgeClient) recordDeadline(ctx context.Context) {
	deadline, ok := ctx.Deadline()
	if ok {
		c.deadlines = append(c.deadlines, deadline)
	}
}

func (c *deadlineTrackingForgeClient) FindSwitchIds(
	ctx context.Context,
	_ *corev1.SwitchSearchFilter,
	_ ...grpc.CallOption,
) (*corev1.SwitchIdList, error) {
	c.recordDeadline(ctx)
	time.Sleep(10 * time.Millisecond)
	return &corev1.SwitchIdList{Ids: []*corev1.SwitchId{{Id: "switch-a"}}}, nil
}

func (c *deadlineTrackingForgeClient) Version(
	ctx context.Context,
	_ *corev1.VersionRequest,
	_ ...grpc.CallOption,
) (*corev1.BuildInfo, error) {
	c.recordDeadline(ctx)
	return &corev1.BuildInfo{RuntimeConfig: &corev1.RuntimeConfig{MaxFindByIds: 100}}, nil
}

func (c *deadlineTrackingForgeClient) FindSwitchesByIds(
	ctx context.Context,
	_ *corev1.SwitchesByIdsRequest,
	_ ...grpc.CallOption,
) (*corev1.SwitchList, error) {
	c.recordDeadline(ctx)
	return &corev1.SwitchList{Switches: []*corev1.Switch{
		{
			Id:               &corev1.SwitchId{Id: "switch-a"},
			RackId:           &corev1.RackId{Id: "rack-a"},
			NvlinkDomainUuid: &corev1.NVLinkDomainId{Value: "20000000-0000-0000-0000-000000000001"},
		},
	}}, nil
}

func TestGetObservedNVLinkDomainMembershipsUsesPerRPCTimeouts(t *testing.T) {
	forgeClient := &deadlineTrackingForgeClient{}
	client := &grpcClient{gclient: newBatchingForgeClient(forgeClient), grpcTimeout: time.Minute}

	memberships, err := client.GetObservedNVLinkDomainMemberships(context.Background())
	require.NoError(t, err)
	require.Len(t, memberships, 1)
	require.Len(t, forgeClient.deadlines, 3)
	assert.Greater(t, forgeClient.deadlines[1].Sub(forgeClient.deadlines[0]), 5*time.Millisecond)
	assert.Equal(t, forgeClient.deadlines[1], forgeClient.deadlines[2])
}

func TestNVLinkDomainMembershipsFromSwitches(t *testing.T) {
	domainID := "20000000-0000-0000-0000-000000000001"
	switchWithDomain := func(switchID, rackID string) *corev1.Switch {
		return &corev1.Switch{
			Id:               &corev1.SwitchId{Id: switchID},
			RackId:           &corev1.RackId{Id: rackID},
			NvlinkDomainUuid: &corev1.NVLinkDomainId{Value: domainID},
		}
	}

	tests := []struct {
		name      string
		requested []*corev1.SwitchId
		switches  []*corev1.Switch
		want      []NVLinkDomainMembership
		wantErr   string
	}{
		{
			name:      "returns duplicate rack observations",
			requested: []*corev1.SwitchId{{Id: "switch-a"}, {Id: "switch-b"}},
			switches: []*corev1.Switch{
				switchWithDomain("switch-a", "rack-a"),
				switchWithDomain("switch-b", "rack-a"),
			},
			want: []NVLinkDomainMembership{
				{DomainID: domainID, RackID: "rack-a"},
				{DomainID: domainID, RackID: "rack-a"},
			},
		},
		{
			name:      "skips switch without domain observation",
			requested: []*corev1.SwitchId{{Id: "switch-a"}},
			switches: []*corev1.Switch{
				{Id: &corev1.SwitchId{Id: "switch-a"}, RackId: &corev1.RackId{Id: "rack-a"}},
			},
			want: []NVLinkDomainMembership{},
		},
		{
			name:      "skips switch without rack assignment",
			requested: []*corev1.SwitchId{{Id: "switch-a"}},
			switches: []*corev1.Switch{
				{Id: &corev1.SwitchId{Id: "switch-a"}, NvlinkDomainUuid: &corev1.NVLinkDomainId{Value: domainID}},
			},
			want: []NVLinkDomainMembership{},
		},
		{
			name:      "rejects partial details response",
			requested: []*corev1.SwitchId{{Id: "switch-a"}, {Id: "switch-b"}},
			switches:  []*corev1.Switch{switchWithDomain("switch-a", "rack-a")},
			wantErr:   "omitted active switch switch-b",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := nvLinkDomainMembershipsFromSwitches(test.requested, test.switches)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}
