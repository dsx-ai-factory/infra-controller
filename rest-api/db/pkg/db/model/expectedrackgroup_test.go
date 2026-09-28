// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"testing"

	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestExpectedRackGroupMember_ToProto(t *testing.T) {
	for _, tc := range []struct {
		rest ExpectedRackGroupMemberType
		core string
	}{
		{ExpectedRackGroupMemberTypeCompute, "Compute"},
		{ExpectedRackGroupMemberTypeNVSwitch, "Switch"},
		{ExpectedRackGroupMemberTypePowerShelf, "PowerShelf"},
	} {
		t.Run(string(tc.rest), func(t *testing.T) {
			members := ExpectedRackGroupMember{Type: tc.rest, Manufacturer: "NVIDIA", ID: "device-01"}
			require.NoError(t, members.Validate())
			wire := members.ToProto()
			require.Equal(t, tc.core, wire.Type)
			var result ExpectedRackGroupMember
			require.NoError(t, result.FromProto(wire))
			require.Equal(t, members, result)
			stored, err := json.Marshal(result)
			require.NoError(t, err)
			require.JSONEq(t, `{"type":"`+string(tc.rest)+`","manufacturer":"NVIDIA","id":"device-01"}`, string(stored))
		})
	}
}

func TestExpectedRackGroupMember_FromProto(t *testing.T) {
	for _, name := range []string{"", "switch", "NVSwitch", "NVSWITCH", "Other"} {
		t.Run(name, func(t *testing.T) {
			original := ExpectedRackGroupMember{Type: ExpectedRackGroupMemberTypeCompute, Manufacturer: "NVIDIA", ID: "original"}
			members := original
			err := members.FromProto(&corev1.ExpectedRackGroupMember{Type: name, Manufacturer: "NVIDIA", Id: "invalid"})
			require.Error(t, err)
			require.Equal(t, original, members)
		})
	}
}

func TestExpectedRackGroupProtoConversion(t *testing.T) {
	testExpectedRackGroupMembershipFromProto(t)
	t.Run("round trips identity profile membership and metadata", func(t *testing.T) {
		original := &ExpectedRackGroup{
			RackGroupID: "nvl5-gp1-jhb01",
			Topology:    "gb200_nvl72r1_c2g4",
			Racks:       []ExpectedRackGroupRack{{RackID: "rack-01", Members: []ExpectedRackGroupMember{{Type: ExpectedRackGroupMemberTypeCompute, Manufacturer: "NVIDIA", ID: "device-01"}}}, {RackID: "rack-02", Members: []ExpectedRackGroupMember{}}},
			Name:        "NVL group 1",
			Description: "JHB row 1",
			Labels: Labels{
				"chassis.manufacturer": "NVIDIA",
				"location.datacenter":  "jhb01",
			},
		}

		got := &ExpectedRackGroup{}
		require.NoError(t, got.FromProto(original.ToProto()))

		assert.Equal(t, original.RackGroupID, got.RackGroupID)
		assert.Equal(t, original.Topology, got.Topology)
		assert.Equal(t, original.Racks, got.Racks)
		assert.Equal(t, original.Name, got.Name)
		assert.Equal(t, original.Description, got.Description)
		assert.Equal(t, original.Labels, got.Labels)
	})

	t.Run("replaces membership while preserving absent group identity", func(t *testing.T) {
		group := &ExpectedRackGroup{
			RackGroupID: "preserved-group",
			Topology:    "preserved-profile",
			Racks:       []ExpectedRackGroupRack{{RackID: "stale-rack"}},
		}
		err := group.FromProto(&corev1.ExpectedRackGroup{
			RackGroupId: &corev1.RackGroupId{},
			Racks:       []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}}},
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{{Key: "location.room", Value: cutil.GetPtr("A1")}},
			},
		})

		require.NoError(t, err)
		assert.Equal(t, "preserved-group", group.RackGroupID)
		assert.Equal(t, "", group.Topology)
		assert.Equal(t, []ExpectedRackGroupRack{{RackID: "rack-01", Members: []ExpectedRackGroupMember{}}}, group.Racks)
		assert.Equal(t, Labels{"location.room": "A1"}, group.Labels)
	})
}

func TestExpectedRackGroupPersistence(t *testing.T) {
	ctx := context.Background()
	session := dbutil.GetTestDBSession(t, false)
	defer session.Close()
	TestSetupSchema(t, session)
	err := session.DB.ResetModel(ctx, (*ExpectedRackGroup)(nil))
	require.NoError(t, err)
	user := TestBuildUser(t, session, "rack-group-user", "rack-group-org", []string{"admin"})
	provider := TestBuildInfrastructureProvider(t, session, "rack-group-provider", "rack-group-org", user)
	site := TestBuildSite(t, session, provider, "rack-group-site", user)
	dao := NewExpectedRackGroupDAO(session)
	devices := []ExpectedRackGroupMember{{Type: ExpectedRackGroupMemberTypeNVSwitch, Manufacturer: "NVIDIA", ID: "device-01"}}
	inputs := []ExpectedRackGroupCreateInput{
		{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-a", Topology: "gb200_nvl72r1_c2g4", Racks: []ExpectedRackGroupRack{{RackID: "rack-02", Members: devices}, {RackID: "rack-01", Members: []ExpectedRackGroupMember{}}}, CreatedBy: user.ID},
		{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-b", Topology: "gb200_nvl72r1_c2g4", Racks: []ExpectedRackGroupRack{{RackID: "rack-01", Members: devices}}, CreatedBy: user.ID},
	}
	err = db.WithTx(ctx, session, func(tx *db.Tx) error {
		rows, err := dao.CreateMultiple(ctx, tx, inputs)
		require.NoError(t, err)
		require.Equal(t, inputs[0].Racks, rows[0].Racks)
		require.Equal(t, devices, rows[0].Racks[0].Members)
		updated, err := dao.UpdateMultiple(ctx, tx, []ExpectedRackGroupUpdateInput{
			{ExpectedRackGroupID: rows[0].ID, Racks: []ExpectedRackGroupRack{}},
			{ExpectedRackGroupID: rows[1].ID, Name: cutil.GetPtr("renamed")},
		})
		require.NoError(t, err)
		require.Empty(t, updated[0].Racks)
		require.Equal(t, devices, updated[1].Racks[0].Members)
		return nil
	})
	require.NoError(t, err)
	got, err := dao.Get(ctx, nil, inputs[1].ExpectedRackGroupID, nil, false)
	require.NoError(t, err)
	require.Equal(t, devices, got.Racks[0].Members)
}

func TestExpectedRackGroupSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	session := dbutil.GetTestDBSession(t, false)
	defer session.Close()
	TestSetupSchema(t, session)
	require.NoError(t, session.DB.ResetModel(ctx, (*ExpectedRackGroup)(nil)))
	user := TestBuildUser(t, session, "clear-metadata-user", "clear-metadata-org", []string{"admin"})
	provider := TestBuildInfrastructureProvider(t, session, "clear-metadata-provider", "clear-metadata-org", user)
	site := TestBuildSite(t, session, provider, "clear-metadata-site", user)
	dao := NewExpectedRackGroupDAO(session)
	for _, tc := range []struct {
		name            string
		input           ExpectedRackGroupUpdateInput
		wantName        string
		wantDescription string
	}{
		{name: "clear name preserves description", input: ExpectedRackGroupUpdateInput{Name: cutil.GetPtr("")}, wantDescription: "original description"},
		{name: "clear description preserves name", input: ExpectedRackGroupUpdateInput{Description: cutil.GetPtr("")}, wantName: "original name"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			row, err := dao.Create(ctx, nil, ExpectedRackGroupCreateInput{
				ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: tc.name, Topology: "topology", CreatedBy: user.ID,
				Name: "original name", Description: "original description",
			})
			require.NoError(t, err)
			tc.input.ExpectedRackGroupID = row.ID
			updated, err := dao.Update(ctx, nil, tc.input)
			require.NoError(t, err)
			require.Equal(t, tc.wantName, updated.Name)
			require.Equal(t, tc.wantDescription, updated.Description)
			stored, err := dao.Get(ctx, nil, row.ID, nil, false)
			require.NoError(t, err)
			require.Equal(t, tc.wantName, stored.Name)
			require.Equal(t, tc.wantDescription, stored.Description)
		})
	}
}

func testExpectedRackGroupMembershipFromProto(t *testing.T) {
	t.Helper()
	device := &corev1.ExpectedRackGroupMember{Type: "Switch", Manufacturer: "NVIDIA", Id: "device-01"}
	for _, tc := range []struct {
		name    string
		wire    []*corev1.ExpectedRackGroupRack
		wantErr bool
	}{
		{"empty snapshot", nil, false},
		{"missing rack", []*corev1.ExpectedRackGroupRack{nil}, true},
		{"missing rack identity", []*corev1.ExpectedRackGroupRack{{}}, true},
		{"unknown member after valid member", []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}, Members: []*corev1.ExpectedRackGroupMember{device, {Type: "NVSwitch", Manufacturer: "NVIDIA", Id: "invalid"}}}}, true},
		{"duplicate rack", []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}}, {RackId: &corev1.RackId{Id: "rack-01"}}}, true},
		{"duplicate device ownership", []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}, Members: []*corev1.ExpectedRackGroupMember{device}}, {RackId: &corev1.RackId{Id: "rack-02"}, Members: []*corev1.ExpectedRackGroupMember{device}}}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			original := ExpectedRackGroup{Racks: []ExpectedRackGroupRack{{RackID: "preserved", Members: []ExpectedRackGroupMember{}}}}
			got := original
			err := got.FromProto(&corev1.ExpectedRackGroup{Racks: tc.wire})
			if tc.wantErr {
				require.Error(t, err)
				require.Equal(t, original, got)
			} else {
				require.NoError(t, err)
				require.Equal(t, []ExpectedRackGroupRack{}, got.Racks)
			}
		})
	}
}
