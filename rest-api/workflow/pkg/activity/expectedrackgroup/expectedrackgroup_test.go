// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedrackgroup

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

func TestManageExpectedRackGroup_UpdateExpectedRackGroupsInDB(t *testing.T) {
	ctx := context.Background()
	session := cdbu.GetTestDBSession(t, false)
	defer session.Close()
	for _, model := range []interface{}{(*cdbm.InfrastructureProvider)(nil), (*cdbm.Site)(nil), (*cdbm.User)(nil), (*cdbm.ExpectedRackGroup)(nil)} {
		require.NoError(t, session.DB.ResetModel(ctx, model))
	}
	user := cwu.TestBuildUser(t, session, uuid.NewString(), []string{"group-test"}, []string{"FORGE_PROVIDER_ADMIN"})
	provider := cwu.TestBuildInfrastructureProvider(t, session, "provider", "group-test", user)
	site := cwu.TestBuildSite(t, session, provider, "site", cdbm.SiteStatusRegistered, nil, user)
	dao := cdbm.NewExpectedRackGroupDAO(session)
	manager := NewManageExpectedRackGroup(session, nil)

	for _, tc := range []struct {
		name      string
		inventory *corev1.ExpectedRackGroupInventory
		recent    bool
		deleted   bool
		wantErr   bool
		wantType  cdbm.ExpectedRackGroupMemberType
	}{
		{name: "missing snapshot", wantErr: true},
		{name: "unspecified status preserves inventory", inventory: &corev1.ExpectedRackGroupInventory{}, wantErr: true},
		{name: "failed snapshot preserves inventory", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}},
		{name: "successful empty snapshot deletes old rows", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}, deleted: true},
		{name: "recent API write survives empty snapshot", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}, recent: true},
		{name: "nonfinal page preserves absent rows", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, InventoryPage: &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2}}},
		{name: "final page preserves IDs from earlier pages", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, InventoryPage: &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, ItemIds: []string{"final page preserves IDs from earlier pages"}}}},
		{name: "final page deletes absent rows", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, InventoryPage: &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2}}, deleted: true},
		{name: "Core Switch is stored as REST NVSwitch", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, ExpectedRackGroups: []*corev1.ExpectedRackGroup{{Topology: "topology", Racks: []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}, Members: []*corev1.ExpectedRackGroupMember{{Type: "Switch", Manufacturer: "NVIDIA", Id: "device-01"}}}}}}}, wantType: cdbm.ExpectedRackGroupMemberTypeNVSwitch},
		{name: "invalid member does not delete inventory", inventory: &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, ExpectedRackGroups: []*corev1.ExpectedRackGroup{{Topology: "topology", Racks: []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: "rack-01"}, Members: []*corev1.ExpectedRackGroupMember{{Type: "NVSwitch", Manufacturer: "NVIDIA", Id: "device-01"}}}}}}}, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			for _, group := range tc.inventory.GetExpectedRackGroups() {
				group.RackGroupId = &corev1.RackGroupId{Id: tc.name}
			}
			row, err := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{
				ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: tc.name, Topology: "topology", CreatedBy: user.ID,
			})
			require.NoError(t, err)
			if !tc.recent {
				_, err = session.DB.NewUpdate().Model((*cdbm.ExpectedRackGroup)(nil)).
					Set("updated = ?", time.Now().Add(-time.Hour)).Where("id = ?", row.ID).Exec(ctx)
				require.NoError(t, err)
			}
			err = manager.UpdateExpectedRackGroupsInDB(ctx, site.ID, tc.inventory)
			if tc.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
			}
			stored, err := dao.Get(ctx, nil, row.ID, nil, false)
			if tc.deleted {
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			} else {
				require.NoError(t, err)
				if tc.wantType != "" {
					require.Equal(t, []cdbm.ExpectedRackGroupMember{{Type: tc.wantType, Manufacturer: "NVIDIA", ID: "device-01"}}, stored.Racks[0].Members)
				}
				require.NoError(t, dao.Delete(ctx, nil, row.ID))
			}
		})
	}

	for _, operation := range []string{"UPDATE", "DELETE"} {
		t.Run("concurrent API write survives inventory "+operation, func(t *testing.T) {
			row, err := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{
				ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: operation, Topology: "old", CreatedBy: user.ID,
			})
			require.NoError(t, err)
			_, err = session.DB.NewUpdate().Model((*cdbm.ExpectedRackGroup)(nil)).Set("updated = ?", time.Now().Add(-time.Hour)).Where("id = ?", row.ID).Exec(ctx)
			require.NoError(t, err)
			// Interleave an API write after inventory read, immediately before its mutation.
			hook := &rackGroupWriteHook{operation: operation, before: func() {
				topology := "API-new"
				_, err := dao.Update(ctx, nil, cdbm.ExpectedRackGroupUpdateInput{ExpectedRackGroupID: row.ID, Topology: &topology})
				require.NoError(t, err)
			}}
			session.DB.AddQueryHook(hook)
			inventory := &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
			if operation == "UPDATE" {
				inventory.ExpectedRackGroups = []*corev1.ExpectedRackGroup{{RackGroupId: &corev1.RackGroupId{Id: operation}, Topology: "inventory-old"}}
			}
			require.NoError(t, manager.UpdateExpectedRackGroupsInDB(ctx, site.ID, inventory))
			require.True(t, hook.fired)
			stored, err := dao.Get(ctx, nil, row.ID, nil, false)
			require.NoError(t, err)
			require.Equal(t, "API-new", stored.Topology)
			require.NoError(t, dao.Delete(ctx, nil, row.ID))
		})
	}

	for _, operation := range []string{"INSERT", "UPDATE", "DELETE"} {
		t.Run("database failure retries "+operation, func(t *testing.T) {
			groupID := "retry-" + operation
			if operation != "INSERT" {
				_, err := dao.Create(ctx, nil, cdbm.ExpectedRackGroupCreateInput{ExpectedRackGroupID: uuid.New(), SiteID: site.ID, RackGroupID: groupID, Topology: "old", CreatedBy: user.ID})
				require.NoError(t, err)
				_, err = session.DB.NewUpdate().Model((*cdbm.ExpectedRackGroup)(nil)).Set("updated = ?", time.Now().Add(-time.Hour)).Where("rack_group_id = ?", groupID).Exec(ctx)
				require.NoError(t, err)
			}
			_, err := session.DB.ExecContext(ctx, `CREATE FUNCTION fail_rack_group_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$`)
			require.NoError(t, err)
			_, err = session.DB.ExecContext(ctx, fmt.Sprintf("CREATE TRIGGER fail_write BEFORE %s ON expected_rack_group FOR EACH ROW EXECUTE FUNCTION fail_rack_group_write()", operation))
			require.NoError(t, err)
			inventory := &corev1.ExpectedRackGroupInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
			if operation != "DELETE" {
				inventory.ExpectedRackGroups = []*corev1.ExpectedRackGroup{{RackGroupId: &corev1.RackGroupId{Id: groupID}, Topology: "inventory"}}
			}
			err = manager.UpdateExpectedRackGroupsInDB(ctx, site.ID, inventory)
			require.ErrorContains(t, err, "injected database failure")
			var beforeRetry []cdbm.ExpectedRackGroup
			require.NoError(t, session.DB.NewSelect().Model(&beforeRetry).Where("rack_group_id = ?", groupID).Scan(ctx))
			if operation == "INSERT" {
				require.Empty(t, beforeRetry)
			} else {
				require.Len(t, beforeRetry, 1)
				require.Equal(t, "old", beforeRetry[0].Topology)
			}
			_, err = session.DB.ExecContext(ctx, "DROP TRIGGER fail_write ON expected_rack_group; DROP FUNCTION fail_rack_group_write()")
			require.NoError(t, err)
			require.NoError(t, manager.UpdateExpectedRackGroupsInDB(ctx, site.ID, inventory))
			var rows []cdbm.ExpectedRackGroup
			require.NoError(t, session.DB.NewSelect().Model(&rows).Where("rack_group_id = ?", groupID).Scan(ctx))
			if operation == "DELETE" {
				require.Empty(t, rows)
			} else {
				require.Len(t, rows, 1)
				require.Equal(t, "inventory", rows[0].Topology)
				require.NoError(t, dao.Delete(ctx, nil, rows[0].ID))
			}
		})
	}
}

type rackGroupWriteHook struct {
	operation string
	before    func()
	fired     bool
}

func (h *rackGroupWriteHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && strings.HasPrefix(event.Query, h.operation) && strings.Contains(event.Query, "expected_rack_group") {
		h.fired = true
		h.before()
	}
	return ctx
}

func (*rackGroupWriteHook) AfterQuery(context.Context, *bun.QueryEvent) {}
