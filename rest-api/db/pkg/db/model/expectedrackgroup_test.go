// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"testing"

	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestExpectedRackGroupProtoConversion(t *testing.T) {
	t.Run("round trips identity profile membership and metadata", func(t *testing.T) {
		original := &ExpectedRackGroup{
			RackGroupID: "nvl5-gp1-jhb01",
			Topology:    "gb200_nvl72r1_c2g4",
			RackIDs:     []string{"rack-01", "rack-02"},
			Members:     ExpectedRackGroupMembers{{Type: "compute-tray", Manufacturer: "NVIDIA", ID: "device-01"}},
			Name:        "NVL group 1",
			Description: "JHB row 1",
			Labels: Labels{
				"chassis.manufacturer": "NVIDIA",
				"location.datacenter":  "jhb01",
			},
		}

		got := &ExpectedRackGroup{}
		got.FromProto(original.ToProto())

		assert.Equal(t, original.RackGroupID, got.RackGroupID)
		assert.Equal(t, original.Topology, got.Topology)
		assert.Equal(t, original.RackIDs, got.RackIDs)
		assert.Equal(t, original.Members, got.Members)
		assert.Equal(t, original.Name, got.Name)
		assert.Equal(t, original.Description, got.Description)
		assert.Equal(t, original.Labels, got.Labels)
	})

	t.Run("ignores empty identifiers and members", func(t *testing.T) {
		group := &ExpectedRackGroup{
			RackGroupID: "preserved-group",
			Topology:    "preserved-profile",
			RackIDs:     []string{"stale-rack"},
		}
		group.FromProto(&corev1.ExpectedRackGroup{
			RackGroupId: &corev1.RackGroupId{},
			RackIds: []*corev1.RackId{
				nil,
				{Id: "rack-01"},
				{},
			},
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{{Key: "location.room", Value: cutil.GetPtr("A1")}},
			},
		})

		assert.Equal(t, "preserved-group", group.RackGroupID)
		assert.Equal(t, "", group.Topology)
		assert.Equal(t, []string{"rack-01"}, group.RackIDs)
		assert.Equal(t, Labels{"location.room": "A1"}, group.Labels)
	})
}

func TestExpectedRackGroupPersistence(t *testing.T) {
	ctx := context.Background()
	session := dbutil.GetTestDBSession(t, false)
	defer session.Close()
	err := session.DB.ResetModel(ctx, (*ExpectedRackGroup)(nil))
	require.NoError(t, err)
	user := TestBuildUser(t, session, "rack-group-user", "rack-group-org", []string{"admin"})
	provider := TestBuildInfrastructureProvider(t, session, "rack-group-provider", "rack-group-org", user)
	site := TestBuildSite(t, session, provider, "rack-group-site", user)
	dao := NewExpectedRackGroupDAO(session)
	devices := ExpectedRackGroupMembers{{Type: "compute-tray", Manufacturer: "NVIDIA", ID: "device-01"}}
	inputs := []ExpectedRackGroupCreateInput{
		{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-a", Topology: "gb200_nvl72r1_c2g4", RackIDs: []string{"rack-02", "rack-01"}, Members: devices, CreatedBy: user.ID},
		{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: "group-b", Topology: "gb200_nvl72r1_c2g4", Members: devices, CreatedBy: user.ID},
	}
	err = db.WithTx(ctx, session, func(tx *db.Tx) error {
		rows, err := dao.CreateMultiple(ctx, tx, inputs)
		require.NoError(t, err)
		require.Equal(t, inputs[0].RackIDs, rows[0].RackIDs)
		require.Equal(t, devices, rows[0].Members)
		updated, err := dao.UpdateMultiple(ctx, tx, []ExpectedRackGroupUpdateInput{
			{ExpectedRackGroupID: rows[0].ID, Members: ExpectedRackGroupMembers{}},
			{ExpectedRackGroupID: rows[1].ID, Name: cutil.GetPtr("renamed")},
		})
		require.NoError(t, err)
		require.Empty(t, updated[0].Members)
		require.Equal(t, devices, updated[1].Members)
		require.Equal(t, rows[0].RackIDs, updated[0].RackIDs)
		return nil
	})
	require.NoError(t, err)
	got, err := dao.Get(ctx, nil, inputs[1].ExpectedRackGroupID, nil, false)
	require.NoError(t, err)
	require.Equal(t, devices, got.Members)
}
