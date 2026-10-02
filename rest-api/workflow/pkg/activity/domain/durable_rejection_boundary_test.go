// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package domain

import (
	"context"
	"database/sql"
	"testing"
	"time"

	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
)

// Actual local PG15 reservation, transaction and lease CAS; Core cancellation is substituted.
func TestReservedDomainDurableRejection_SQLStateOrders(t *testing.T) {
	for _, name := range []string{"stale_token_no_destructive_dispatch", "ready_before_reject_no_destructive_dispatch", "cancel_reply_lost_then_recovery_confirms"} {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			session := common.TestInitDB(t)
			t.Cleanup(session.Close)
			common.TestSetupSchema(t, session)
			user := common.TestBuildUser(t, session, uuid.NewString(), "recovery-test-org", []string{"FORGE_TENANT_ADMIN"})
			providerUser := common.TestBuildUser(t, session, uuid.NewString(), "recovery-provider-org", []string{"FORGE_PROVIDER_ADMIN"})
			provider := common.TestBuildInfrastructureProvider(t, session, "Recovery Provider", "recovery-provider-org", providerUser)
			site := common.TestBuildSite(t, session, provider, "Recovery Site", providerUser)
			_, err := cdbm.NewSiteDAO(session).Update(ctx, nil, cdbm.SiteUpdateInput{SiteID: site.ID, Status: util.GetPtr(cdbm.SiteStatusRegistered)})
			require.NoError(t, err)
			tenant := common.TestBuildTenant(t, session, "Recovery Tenant", "recovery-test-org", user)
			common.TestBuildTenantSite(t, session, tenant, site, user)
			coreID := uuid.New()
			dao := cdbm.NewDomainDAO(session)
			tx, err := cdb.BeginTx(ctx, session, &sql.TxOptions{})
			require.NoError(t, err)
			row, fresh, err := dao.ReserveOwned(ctx, tx, cdbm.DomainCreateInput{Hostname: "durable-fence.example.com", Org: "recovery-test-org", TenantID: &tenant.ID, SiteID: &site.ID, ControllerDomainID: &coreID, Status: cdbm.DomainStatusPending, CreatedBy: user.ID})
			require.NoError(t, err)
			require.True(t, fresh)
			require.NoError(t, tx.Commit())
			_, err = session.DB.ExecContext(ctx, `UPDATE domain SET recovery_next_at = current_timestamp - interval '1 second' WHERE id = ?`, row.ID)
			require.NoError(t, err)
			claims, err := dao.ClaimRecovery(ctx, 1, time.Minute)
			require.NoError(t, err)
			require.Len(t, claims, 1)
			stale := claims[0]
			require.NotNil(t, stale.RecoveryToken)
			switch name {
			case "stale_token_no_destructive_dispatch":
				changed, err := dao.DeferRecovery(ctx, row.ID, *stale.RecoveryToken, time.Second)
				require.NoError(t, err)
				require.True(t, changed)
				_, err = session.DB.ExecContext(ctx, `UPDATE domain SET recovery_next_at = current_timestamp - interval '1 second' WHERE id = ?`, row.ID)
				require.NoError(t, err)
				next, err := dao.ClaimRecovery(ctx, 1, time.Minute)
				require.NoError(t, err)
				require.Len(t, next, 1)
				require.NotEqual(t, stale.RecoveryToken, next[0].RecoveryToken)
				tx, err := cdb.BeginTx(ctx, session, &sql.TxOptions{})
				require.NoError(t, err)
				changed, err = dao.StageRejectedOwned(ctx, tx, row.ID, coreID, stale.RecoveryToken)
				require.NoError(t, err)
				require.False(t, changed, "stale lease must not authorize Core cancellation")
				require.NoError(t, tx.Commit())
				stored, err := dao.GetByID(ctx, nil, row.ID, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.DomainStatusPending, stored.Status)
				require.Equal(t, next[0].RecoveryToken, stored.RecoveryToken)
			case "ready_before_reject_no_destructive_dispatch":
				changed, err := dao.CompleteRecovery(ctx, row.ID, coreID, *stale.RecoveryToken, cdbm.DomainStatusPending, cdbm.DomainStatusReady, false)
				require.NoError(t, err)
				require.True(t, changed)
				tx, err := cdb.BeginTx(ctx, session, &sql.TxOptions{})
				require.NoError(t, err)
				changed, err = dao.StageRejectedOwned(ctx, tx, row.ID, coreID, nil)
				require.NoError(t, err)
				require.False(t, changed, "Ready row cannot be canceled by delayed handler")
				require.NoError(t, tx.Commit())
				stored, err := dao.GetByID(ctx, nil, row.ID, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.DomainStatusReady, stored.Status)
			case "cancel_reply_lost_then_recovery_confirms":
				tx, err := cdb.BeginTx(ctx, session, &sql.TxOptions{})
				require.NoError(t, err)
				changed, err := dao.StageRejectedOwned(ctx, tx, row.ID, coreID, stale.RecoveryToken)
				require.NoError(t, err)
				require.True(t, changed)
				require.NoError(t, tx.Commit(), "stage must commit before mock cancel reply is lost")
				// Simulated Core cancellation committed, but its reply was lost. This
				// mock cannot establish real Core tombstone durability.
				changed, err = cdb.WithTxResult(ctx, session, func(tx *cdb.Tx) (bool, error) {
					return dao.TransitionOwned(ctx, tx, row.ID, coreID, cdbm.DomainStatusPending, cdbm.DomainStatusReady)
				})
				require.NoError(t, err)
				require.False(t, changed, "original delayed Create success cannot commit Ready")
				stored, err := dao.GetByID(ctx, nil, row.ID, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.DomainStatusRejecting, stored.Status)
				require.Equal(t, &coreID, stored.ControllerDomainID)
				// Expire the first worker lease in this isolated fixture: the next
				// recovery tick must reclaim Rejecting rather than replay Create.
				_, err = session.DB.ExecContext(ctx, `UPDATE domain SET recovery_lease_until = current_timestamp - interval '1 second' WHERE id = ?`, row.ID)
				require.NoError(t, err)
				next, err := dao.ClaimRecovery(ctx, 1, time.Minute)
				require.NoError(t, err)
				require.Len(t, next, 1)
				require.Equal(t, cdbm.DomainStatusRejecting, next[0].Status)
				require.NotEqual(t, stale.RecoveryToken, next[0].RecoveryToken)
				changed, err = dao.CompleteRecovery(ctx, row.ID, coreID, *stale.RecoveryToken, cdbm.DomainStatusPending, cdbm.DomainStatusReady, false)
				require.NoError(t, err)
				require.False(t, changed, "late old worker cannot mark tombstoned reservation Ready")
				// New worker retries the same idempotent reserved-ID cancellation.
				client := &tmocks.Client{}
				run := &tmocks.WorkflowRun{}
				var forwarded corev1.DomainDeletionRequest
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(request grpcproxy.Request) bool {
					return request.FullMethod == corev1.Forge_DeleteDomain_FullMethodName && protojson.Unmarshal(request.RequestJSON, &forwarded) == nil
				})).Return(run, nil).Once()
				run.On("Get", mock.Anything, mock.Anything).Return(nil).Once()
				pool := sc.NewClientPool(nil)
				pool.IDClientMap[site.ID.String()] = client
				require.NoError(t, (ManageDomain{DB: session, Sites: pool}).reconcileOne(ctx, dao, &next[0]))
				require.Equal(t, coreID.String(), forwarded.GetId().GetValue())
				require.True(t, forwarded.GetCancelReservedId())
				stored, err = dao.GetByID(ctx, nil, row.ID, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.DomainStatusError, stored.Status)
				require.Nil(t, stored.RecoveryToken)
				client.AssertExpectations(t)
				run.AssertExpectations(t)
			}
		})
	}
}
