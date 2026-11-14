// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type creationSelectHook struct{ selected chan struct{} }

func (h *creationSelectHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if strings.HasPrefix(event.Query, "SELECT") && strings.Contains(event.Query, `FROM "vpc" AS "v"`) && strings.Contains(event.Query, "FOR UPDATE") {
		select {
		case h.selected <- struct{}{}:
		default:
		}
	}
	return ctx
}

func (*creationSelectHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func testVpcCompletionAtomicity(t *testing.T, session *db.Session, saved *Vpc) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	dao := NewVpcDAO(session)
	reported := saved.ToProto()
	reported.Status = &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(50003))}
	_, err := session.DB.ExecContext(ctx, `ALTER TABLE status_detail ADD CONSTRAINT vpc_creation_history_failure CHECK (entity_id <> ? OR status <> 'Ready')`, saved.ID.String())
	require.NoError(t, err)
	_, err = dao.CompleteCreation(ctx, saved.ID, reported)
	require.Error(t, err)
	unchanged, err := dao.GetByID(ctx, nil, saved.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, VpcStatusPending, unchanged.Status)
	assert.Nil(t, unchanged.ActiveVni)
	assert.Nil(t, unchanged.CreationRejectedAt)
	_, err = session.DB.ExecContext(ctx, `ALTER TABLE status_detail DROP CONSTRAINT vpc_creation_history_failure`)
	require.NoError(t, err)

	// Both completion calls reach the query hook while the database row is locked.
	lock, err := session.DB.BeginTx(ctx, nil)
	require.NoError(t, err)
	defer func() { _ = lock.Rollback() }()
	_, err = lock.ExecContext(ctx, `SELECT id FROM vpc WHERE id = ? FOR UPDATE`, saved.ID)
	require.NoError(t, err)
	hook := &creationSelectHook{selected: make(chan struct{}, 2)}
	session.DB.AddQueryHook(hook)
	results := make(chan error, 2)
	for range 2 {
		go func() { _, completeErr := dao.CompleteCreation(ctx, saved.ID, reported); results <- completeErr }()
	}
	for range 2 {
		select {
		case <-hook.selected:
		case <-ctx.Done():
			t.Fatal("concurrent completions did not reach their row locks")
		}
	}
	require.NoError(t, lock.Commit())
	for range 2 {
		require.NoError(t, <-results)
	}
	completed, err := dao.GetByID(ctx, nil, saved.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, VpcStatusReady, completed.Status)
	assert.Nil(t, completed.CreationRejectedAt)
	assert.Equal(t, saved.CreatedBy, completed.CreatedBy)
	details, _, err := NewStatusDetailDAO(session).GetAll(ctx, nil, StatusDetailFilterInput{EntityIDs: []string{saved.ID.String()}}, paginator.PageInput{})
	require.NoError(t, err)
	require.Len(t, details, 1)
	assert.Equal(t, VpcStatusReady, details[0].Status)
}

func TestVpc_matchesCreation(t *testing.T) {
	prefixes := []string{"2001:0DB8:0:0:0:0:0:1/64"}
	saved := &Vpc{ID: uuid.New(), Org: "tenant", Name: "original", Labels: Labels{"empty": ""},
		NetworkVirtualizationType: cutil.GetPtr(VpcFNN), Vni: cutil.GetPtr(50001), RoutingProfile: cutil.GetPtr("INTERNAL"),
		RoutingProfileOverrides: &VpcRoutingProfileOverrides{AllowedAnycastPrefixes: &prefixes}}
	cases := []struct {
		name    string
		change  func(*corev1.Vpc)
		matches bool
		omitVni bool
	}{
		{name: "canonical prefix and empty label value match", matches: true, change: func(v *corev1.Vpc) {
			v.Config.RoutingProfileOverrides.AllowedAnycastPrefixes.Values[0].Prefix = "2001:db8::1/64"
			v.Metadata.Labels[0].Value = nil
		}},
		{name: "metadata from another creation conflicts", change: func(v *corev1.Vpc) { v.Metadata.Name = "another" }},
		{name: "another power group conflicts", change: func(v *corev1.Vpc) { v.Config.PowerResourceGroup = cutil.GetPtr("another-group") }},
		{name: "supplied VNI cannot be omitted", change: func(v *corev1.Vpc) { v.Config.Vni = nil }},
		{name: "omitted VNI cannot adopt a supplied request", omitVni: true, change: func(v *corev1.Vpc) { v.Config.Vni = cutil.GetPtr(uint32(50001)) }},
		{name: "assigned VNI preserves an omitted request", omitVni: true, matches: true, change: func(v *corev1.Vpc) { v.Status = &corev1.VpcStatus{Vni: cutil.GetPtr(uint32(50002))} }},
		{name: "supplied profile must match", change: func(v *corev1.Vpc) { v.Config.RoutingProfileType = cutil.GetPtr("EXTERNAL") }},
		{name: "different policy prefix conflicts", change: func(v *corev1.Vpc) {
			v.Config.RoutingProfileOverrides.AllowedAnycastPrefixes.Values[0].Prefix = "2001:db8:1::/64"
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := *saved
			if tc.omitVni {
				request.Vni = nil
			}
			reported := proto.Clone(request.ToProto()).(*corev1.Vpc)
			tc.change(reported)
			assert.Equal(t, tc.matches, request.matchesCreation(reported))
		})
	}
}

func TestVpcSQLDAO_RejectCreation(t *testing.T) {
	ctx := context.Background()
	session := testInitDB(t)
	defer session.Close()
	testVpcSetupSchema(t, session)
	require.NoError(t, session.DB.ResetModel(ctx, (*StatusDetail)(nil)))
	user := testBuildUser(t, session, nil, testGenerateStarfleetID(), nil, nil, nil)
	provider := testBuildInfrastructureProvider(t, session, nil, "reject-provider", "provider", user.ID)
	tenant := testBuildTenant(t, session, nil, "reject-tenant", "tenant", user.ID)
	site := testBuildSite(t, session, nil, provider.ID, "reject-site", "site", provider.Org, user.ID)
	saved := testBuildVpc(t, session, nil, "rejected", nil, tenant.Org, provider.ID, tenant.ID, site.ID,
		nil, nil, nil, nil, cutil.GetPtr(VpcStatusPending), user.ID, nil)
	dao := NewVpcDAO(session)

	t.Run("history failure rolls back decision then rejection survives replay", func(t *testing.T) {
		_, err := session.DB.ExecContext(ctx, `ALTER TABLE status_detail ADD CONSTRAINT vpc_rejection_history_failure CHECK (status <> 'Error')`)
		require.NoError(t, err)
		_, err = dao.RejectCreation(ctx, saved.ID)
		require.Error(t, err)
		pending, err := dao.GetByID(ctx, nil, saved.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, VpcStatusPending, pending.Status)
		assert.Nil(t, pending.CreationRejectedAt)
		_, err = session.DB.ExecContext(ctx, `ALTER TABLE status_detail DROP CONSTRAINT vpc_rejection_history_failure`)
		require.NoError(t, err)
		for range 2 {
			_, err = dao.RejectCreation(ctx, saved.ID)
			require.NoError(t, err)
		}
		rejected, err := dao.GetByID(ctx, nil, saved.ID, nil)
		require.NoError(t, err)
		assert.Equal(t, VpcStatusError, rejected.Status)
		require.NotNil(t, rejected.CreationRejectedAt)
		encoded, err := json.Marshal(rejected)
		require.NoError(t, err)
		assert.NotContains(t, string(encoded), "CreationRejectedAt")
		assert.NotContains(t, string(encoded), "creation_rejected_at")
		details, _, err := NewStatusDetailDAO(session).GetAll(ctx, nil, StatusDetailFilterInput{EntityIDs: []string{saved.ID.String()}}, paginator.PageInput{})
		require.NoError(t, err)
		require.Len(t, details, 1)
		assert.Equal(t, "VPC creation stopped before Core commit", *details[0].Message)
		updated, err := dao.Update(ctx, nil, VpcUpdateInput{VpcID: saved.ID, Status: cutil.GetPtr(VpcStatusDeleting)})
		require.NoError(t, err)
		assert.Equal(t, rejected.CreationRejectedAt, updated.CreationRejectedAt)
	})
	t.Run("missing identity has the DAO sentinel and no row", func(t *testing.T) {
		row, err := dao.RejectCreation(ctx, uuid.New())
		require.ErrorIs(t, err, db.ErrDoesNotExist)
		assert.Nil(t, row)
	})
}
