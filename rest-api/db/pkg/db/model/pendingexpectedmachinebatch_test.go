// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestPendingExpectedMachineBatch_Overlaps(t *testing.T) {
	id := uuid.New()
	batch := PendingExpectedMachineBatch{Inputs: []ExpectedMachineCreateInput{{ExpectedMachineID: id, BmcMacAddress: "aa:bb:cc:dd:ee:ff", ChassisSerialNumber: "Serial"}}}
	cases := []struct {
		name          string
		ids           []uuid.UUID
		macs, serials []string
		want          bool
	}{
		{name: "UUID", ids: []uuid.UUID{id}, want: true},
		{name: "MAC comparison ignores case", macs: []string{"AA:BB:CC:DD:EE:FF"}, want: true},
		{name: "MAC separator aliases reserve the same identity", macs: []string{"aa-bb-cc-dd-ee-ff"}, want: true},
		{name: "serial", serials: []string{"Serial"}, want: true},
		{name: "stored serial comparison preserves case", serials: []string{"SERIAL"}},
		{name: "independent identities", ids: []uuid.UUID{uuid.New()}, macs: []string{"00:11:22:33:44:55"}, serials: []string{"other"}},
		{name: "complete Site mutation", want: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) { require.Equal(t, tc.want, batch.Overlaps(tc.ids, tc.macs, tc.serials)) })
	}
}

func TestPendingExpectedMachineBatch_Succeeded(t *testing.T) {
	first, second := uuid.New(), uuid.New()
	batch := PendingExpectedMachineBatch{Inputs: []ExpectedMachineCreateInput{{ExpectedMachineID: first}, {ExpectedMachineID: second}}}
	result := func(id uuid.UUID, success bool) *corev1.ExpectedMachineOperationResult {
		return &corev1.ExpectedMachineOperationResult{Id: &corev1.UUID{Value: id.String()}, Success: success}
	}
	cases := []struct {
		name    string
		results []*corev1.ExpectedMachineOperationResult
		want    bool
	}{
		{name: "all reserved UUIDs in any order", results: []*corev1.ExpectedMachineOperationResult{result(second, true), result(first, true)}, want: true},
		{name: "partial response", results: []*corev1.ExpectedMachineOperationResult{result(first, true)}},
		{name: "repeated UUID", results: []*corev1.ExpectedMachineOperationResult{result(first, true), result(first, true)}},
		{name: "unrelated UUID", results: []*corev1.ExpectedMachineOperationResult{result(first, true), result(uuid.New(), true)}},
		{name: "failed member", results: []*corev1.ExpectedMachineOperationResult{result(first, true), result(second, false)}},
		{name: "malformed member", results: []*corev1.ExpectedMachineOperationResult{result(first, true), nil}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			require.Equal(t, tc.want, batch.Succeeded(&corev1.BatchExpectedMachineOperationResponse{Results: tc.results}))
		})
	}
}

func TestPendingExpectedMachineBatchDAO_Complete(t *testing.T) {
	cases := []struct {
		name              string
		deleted           bool
		failSecondWrite   bool
		populatedMetadata bool
	}{
		{name: "preserves imported values and repairs missing metadata"},
		{name: "preserves populated metadata creator and explicitly empty labels", populatedMetadata: true},
		{name: "deleted Site cannot be resurrected", deleted: true},
		{name: "member write failure rolls back and a later completion recovers", failSecondWrite: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			session := testInitDB(t)
			t.Cleanup(session.Close)
			testExpectedMachineSetupSchema(t, session)
			require.NoError(t, session.DB.ResetModel(ctx, (*PendingExpectedMachine)(nil)))
			user := TestBuildUser(t, session, "pending-user", "test-org", []string{"admin"})
			provider := TestBuildInfrastructureProvider(t, session, "pending-provider", "test-org", user)
			site := TestBuildSite(t, session, provider, "pending-site", user)
			sku := &SKU{ID: "pending-sku", SiteID: site.ID}
			_, err := session.DB.NewInsert().Model(sku).Exec(ctx)
			require.NoError(t, err)
			first, second := uuid.New(), uuid.New()
			batch := PendingExpectedMachineBatch{WorkflowID: uuid.NewString(), SiteID: site.ID, Inputs: []ExpectedMachineCreateInput{
				{ExpectedMachineID: first, SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "first", Name: cutil.GetPtr("original name"), Description: cutil.GetPtr("original description"), Manufacturer: cutil.GetPtr("original manufacturer"), Labels: map[string]string{"request": "original"}, CreatedBy: user.ID},
				{ExpectedMachineID: second, SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:02", ChassisSerialNumber: "second", SkuID: &sku.ID, CreatedBy: user.ID},
			}}
			dao := NewPendingExpectedMachineBatchDAO(session)
			require.NoError(t, dao.Create(ctx, nil, batch))
			err = NewSkuDAO(session).Delete(ctx, nil, sku.ID)
			require.Error(t, err, "private members must protect requested SKUs across dispatch")

			importInput := ExpectedMachineCreateInput{ExpectedMachineID: first, SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "newer serial", Name: cutil.GetPtr("newer name"), CreatedBy: site.ID}
			wantDescription, wantManufacturer, wantLabels, wantCreator := batch.Inputs[0].Description, batch.Inputs[0].Manufacturer, Labels(batch.Inputs[0].Labels), user.ID
			if tc.populatedMetadata {
				otherCreator := TestBuildUser(t, session, "other-creator", "test-org", []string{"admin"})
				importInput.Description, importInput.Manufacturer, importInput.Labels, importInput.CreatedBy = cutil.GetPtr("newer description"), cutil.GetPtr("newer manufacturer"), map[string]string{}, otherCreator.ID
				wantDescription, wantManufacturer, wantLabels, wantCreator = importInput.Description, importInput.Manufacturer, Labels(importInput.Labels), importInput.CreatedBy
			}
			imported, err := NewExpectedMachineDAO(session).Create(ctx, nil, importInput)
			require.NoError(t, err)
			if tc.deleted {
				require.NoError(t, NewSiteDAO(session).Delete(ctx, nil, site.ID))
			}
			failure := &pendingExpectedMachineWriteFailureHook{machineID: second, armed: tc.failSecondWrite}
			session.DB.AddQueryHook(failure)
			var completed []ExpectedMachine
			complete := func() error {
				return db.WithTx(ctx, session, func(tx *db.Tx) error {
					err := tx.TryAcquireAdvisoryLock(ctx, ExpectedInventoryMutationLockID(site.ID), nil)
					if err != nil {
						return err
					}
					completed, err = dao.Complete(ctx, tx, site.ID, batch.WorkflowID)
					return err
				})
			}
			err = complete()
			if tc.failSecondWrite {
				require.Error(t, err)
				require.True(t, failure.fired, "the failure must happen after the first member write")
				stored, readErr := NewExpectedMachineDAO(session).Get(ctx, nil, first, nil, false)
				require.NoError(t, readErr)
				require.Equal(t, imported, stored, "partial metadata repair and Updated must roll back")
				_, readErr = NewExpectedMachineDAO(session).Get(ctx, nil, second, nil, false)
				require.ErrorIs(t, readErr, db.ErrDoesNotExist)
				pending, readErr := dao.GetAll(ctx, nil, site.ID)
				require.NoError(t, readErr)
				require.Len(t, pending, 1)
				require.Len(t, pending[0].Inputs, 2)
				err = complete()
			}
			remaining, listErr := dao.GetAll(ctx, nil, site.ID)
			require.NoError(t, listErr)
			if tc.deleted {
				require.ErrorIs(t, err, db.ErrDoesNotExist)
				require.Len(t, remaining, 1)
				_, err = NewExpectedMachineDAO(session).Get(ctx, nil, second, nil, false)
				require.ErrorIs(t, err, db.ErrDoesNotExist)
				return
			}
			require.NoError(t, err)
			require.Empty(t, remaining)
			require.Len(t, completed, 2)
			require.Equal(t, imported.ChassisSerialNumber, completed[0].ChassisSerialNumber)
			require.Equal(t, imported.Name, completed[0].Name)
			require.True(t, completed[0].Updated.After(imported.Updated), "completion must fence older inventory snapshots")
			stored, err := NewExpectedMachineDAO(session).Get(ctx, nil, first, nil, false)
			require.NoError(t, err)
			require.Equal(t, wantDescription, stored.Description)
			require.Equal(t, wantManufacturer, stored.Manufacturer)
			require.Equal(t, wantLabels, stored.Labels)
			require.Equal(t, wantCreator, stored.CreatedBy)
			require.Equal(t, second, completed[1].ID)
			require.Equal(t, &sku.ID, completed[1].SkuID)
			err = NewSkuDAO(session).Delete(ctx, nil, sku.ID)
			require.Error(t, err, "successful finalization transfers protection to the public foreign key")
		})
	}
}

func TestPendingExpectedMachineBatchDAO_Delete(t *testing.T) {
	t.Run("definitive rejection releases the private SKU reference", func(t *testing.T) {
		ctx := context.Background()
		session := testInitDB(t)
		t.Cleanup(session.Close)
		testExpectedMachineSetupSchema(t, session)
		require.NoError(t, session.DB.ResetModel(ctx, (*PendingExpectedMachine)(nil)))
		user := TestBuildUser(t, session, "pending-user", "test-org", []string{"admin"})
		provider := TestBuildInfrastructureProvider(t, session, "pending-provider", "test-org", user)
		site := TestBuildSite(t, session, provider, "pending-site", user)
		sku := &SKU{ID: "pending-sku", SiteID: site.ID}
		_, err := session.DB.NewInsert().Model(sku).Exec(ctx)
		require.NoError(t, err)
		batch := PendingExpectedMachineBatch{WorkflowID: uuid.NewString(), SiteID: site.ID, Inputs: []ExpectedMachineCreateInput{
			{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "first", CreatedBy: user.ID},
			{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:02", ChassisSerialNumber: "second", SkuID: &sku.ID, CreatedBy: user.ID},
		}}
		dao := NewPendingExpectedMachineBatchDAO(session)
		require.NoError(t, dao.Create(ctx, nil, batch))
		err = NewSkuDAO(session).Delete(ctx, nil, sku.ID)
		require.Error(t, err, "private members must protect requested SKUs across dispatch")
		require.NoError(t, dao.Delete(ctx, nil, site.ID, batch.WorkflowID))
		require.NoError(t, NewSkuDAO(session).Delete(ctx, nil, sku.ID))
		remaining, err := dao.GetAll(ctx, nil, site.ID)
		require.NoError(t, err)
		require.Empty(t, remaining, "definitive rejection removes every group member")
	})
}

type pendingExpectedMachineWriteFailureHook struct {
	machineID uuid.UUID
	armed     bool
	fired     bool
}

func (h *pendingExpectedMachineWriteFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if h.armed && event.Operation() == "INSERT" && strings.Contains(event.Query, `"expected_machine"`) && strings.Contains(event.Query, h.machineID.String()) {
		h.armed, h.fired = false, true
		canceled, cancel := context.WithCancel(ctx)
		cancel()
		return canceled
	}
	return ctx
}

func (*pendingExpectedMachineWriteFailureHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func TestExpectedMachineCreateInput_JSON(t *testing.T) {
	t.Run("decodes the stored recovery contract", func(t *testing.T) {
		const stored = `{"ExpectedMachineID":"00000000-0000-0000-0000-000000000001","SiteID":"00000000-0000-0000-0000-000000000002","CreatedBy":"00000000-0000-0000-0000-000000000003","BmcMacAddress":"00:11:22:33:44:01","ChassisSerialNumber":"stored-serial","SkuID":"stored-sku","Name":"stored name","RetainBmcCredentials":true,"HostLifecycleProfile":{"disable_lockdown":false}}`
		var input ExpectedMachineCreateInput
		require.NoError(t, json.Unmarshal([]byte(stored), &input))
		require.Equal(t, ExpectedMachineCreateInput{
			ExpectedMachineID: uuid.MustParse("00000000-0000-0000-0000-000000000001"),
			SiteID:            uuid.MustParse("00000000-0000-0000-0000-000000000002"),
			CreatedBy:         uuid.MustParse("00000000-0000-0000-0000-000000000003"),
			BmcMacAddress:     "00:11:22:33:44:01", ChassisSerialNumber: "stored-serial",
			SkuID: cutil.GetPtr("stored-sku"), Name: cutil.GetPtr("stored name"),
			RetainBmcCredentials: cutil.GetPtr(true),
			HostLifecycleProfile: HostLifecycleProfile{DisableLockdown: cutil.GetPtr(false)},
		}, input)
	})
}

func TestPendingExpectedMachineBatchDAO_BindRunID(t *testing.T) {
	ctx := context.Background()
	session := testInitDB(t)
	t.Cleanup(session.Close)
	testExpectedMachineSetupSchema(t, session)
	require.NoError(t, session.DB.ResetModel(ctx, (*PendingExpectedMachine)(nil)))
	user := TestBuildUser(t, session, "run-binding-user", "test-org", []string{"admin"})
	provider := TestBuildInfrastructureProvider(t, session, "run-binding-provider", "test-org", user)
	site := TestBuildSite(t, session, provider, "run-binding-site", user)
	batch := PendingExpectedMachineBatch{WorkflowID: uuid.NewString(), SiteID: site.ID, Inputs: []ExpectedMachineCreateInput{
		{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "first", CreatedBy: user.ID},
		{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:02", ChassisSerialNumber: "second", CreatedBy: user.ID},
	}}
	dao := NewPendingExpectedMachineBatchDAO(session)
	require.NoError(t, dao.Create(ctx, nil, batch))
	require.NoError(t, dao.BindRunID(ctx, site.ID, batch.WorkflowID, "original-run"))
	require.NoError(t, dao.BindRunID(ctx, site.ID, batch.WorkflowID, "replacement-run"))
	var members []PendingExpectedMachine
	require.NoError(t, session.DB.NewSelect().Model(&members).Scan(ctx))
	require.Len(t, members, 2)
	for _, member := range members {
		require.Equal(t, cutil.GetPtr("original-run"), member.RunID)
	}
	groups, err := dao.GetAll(ctx, nil, site.ID)
	require.NoError(t, err)
	require.Len(t, groups, 1)
	require.Equal(t, cutil.GetPtr("original-run"), groups[0].RunID)
}
