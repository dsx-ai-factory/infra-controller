// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

// Models consecutive one-row recovery ticks against actual PostgreSQL. This
// does not execute the Temporal activity or a real Site/Core request.
func TestDomainRecovery_OneSlowLeaseDoesNotStarveNextDueRow(t *testing.T) {
	ctx := context.Background()
	session := testDomainInitDB(t)
	defer session.Close()
	testDomainSetupSchema(t, session)
	user := testDomainBuildUser(t, session, "fairness-user")
	dao := NewDomainDAO(session)
	created := make([]*Domain, 0, 2)
	for _, name := range []string{"first.example.com", "second.example.com"} {
		tenant, site, core := uuid.New(), uuid.New(), uuid.New()
		tx, err := db.BeginTx(ctx, session, &sql.TxOptions{})
		require.NoError(t, err)
		row, fresh, err := dao.ReserveOwned(ctx, tx, DomainCreateInput{Hostname: name, Org: "fairness", TenantID: &tenant, SiteID: &site, ControllerDomainID: &core, Status: DomainStatusPending, CreatedBy: user.ID})
		require.NoError(t, err)
		require.True(t, fresh)
		require.NoError(t, tx.Commit())
		_, err = session.DB.ExecContext(ctx, `UPDATE domain SET recovery_next_at = current_timestamp - interval '1 second' WHERE id = ?`, row.ID)
		require.NoError(t, err)
		created = append(created, row)
	}
	first, err := dao.ClaimRecovery(ctx, 1, 120*time.Second)
	require.NoError(t, err)
	require.Len(t, first, 1)
	require.NotNil(t, first[0].RecoveryToken)
	second, err := dao.ClaimRecovery(ctx, 1, 120*time.Second)
	require.NoError(t, err)
	require.Len(t, second, 1)
	require.NotEqual(t, first[0].ID, second[0].ID, "first remote RPC still holds its lease")
	again, err := dao.ClaimRecovery(ctx, 1, 120*time.Second)
	require.NoError(t, err)
	require.Empty(t, again)
	// The first worker returns an unknown result after the second row was claimed:
	// neither reservation may be dropped or declared Ready by inference.
	changed, err := dao.DeferRecovery(ctx, first[0].ID, *first[0].RecoveryToken, time.Second)
	require.NoError(t, err)
	require.True(t, changed)
	for _, row := range created {
		persisted, err := dao.GetByID(ctx, nil, row.ID, nil)
		require.NoError(t, err)
		require.Equal(t, DomainStatusPending, persisted.Status)
		require.Equal(t, row.ControllerDomainID, persisted.ControllerDomainID)
	}
}
