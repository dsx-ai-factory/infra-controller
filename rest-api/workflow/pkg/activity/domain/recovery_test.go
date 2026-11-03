// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package domain

import (
	"context"
	"errors"
	"testing"
	"time"

	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestManageDomain_ReconcileReservedDomains(t *testing.T) {
	for _, tc := range []struct {
		name        string
		firstFails  bool
		cancelFirst bool
	}{
		{name: "drains multiple due reservations"},
		{name: "a failed reservation does not delay other due work", firstFails: true},
		{name: "caller cancellation stops further claims", cancelFirst: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			session := common.TestInitDB(t)
			t.Cleanup(session.Close)
			common.TestSetupSchema(t, session)
			user := common.TestBuildUser(t, session, uuid.NewString(), "sweep-org", []string{"FORGE_TENANT_ADMIN"})
			providerUser := common.TestBuildUser(t, session, uuid.NewString(), "provider-org", []string{"FORGE_PROVIDER_ADMIN"})
			provider := common.TestBuildInfrastructureProvider(t, session, "provider", "provider-org", providerUser)
			site := common.TestBuildSite(t, session, provider, "site", providerUser)
			_, err := cdbm.NewSiteDAO(session).Update(ctx, nil, cdbm.SiteUpdateInput{
				SiteID: site.ID, Status: util.GetPtr(cdbm.SiteStatusRegistered),
			})
			require.NoError(t, err)
			tenant := common.TestBuildTenant(t, session, "tenant", "sweep-org", user)
			common.TestBuildTenantSite(t, session, tenant, site, user)
			dao := cdbm.NewDomainDAO(session)
			rows := make([]*cdbm.Domain, 0, 2)
			for _, name := range []string{"first.example.com", "second.example.com"} {
				coreID := uuid.New()
				row, err := cdb.WithTxResult(ctx, session, func(tx *cdb.Tx) (*cdbm.Domain, error) {
					row, _, err := dao.ReserveOwned(ctx, tx, cdbm.DomainCreateInput{
						Hostname: name, Org: "sweep-org", TenantID: &tenant.ID, SiteID: &site.ID,
						ControllerDomainID: &coreID, Status: cdbm.DomainStatusPending, CreatedBy: user.ID,
					})
					return row, err
				})
				require.NoError(t, err)
				rows = append(rows, row)
			}
			// ClaimRecovery orders equal-attempt rows by updated, then ID. Give
			// the cancellation case a deterministic first claim even when the
			// inserts receive the same timestamp.
			for i, row := range rows {
				_, err = session.DB.ExecContext(ctx, `UPDATE domain SET recovery_next_at = current_timestamp - interval '1 second', updated = ? WHERE id = ?`,
					time.Date(2026, time.January, 1, 0, 0, i, 0, time.UTC), row.ID)
				require.NoError(t, err)
			}

			client := &tmocks.Client{}
			pool := sc.NewClientPool(nil)
			pool.IDClientMap[site.ID.String()] = client
			for i, row := range rows {
				if tc.cancelFirst && i > 0 {
					break
				}
				run := &tmocks.WorkflowRun{}
				var resultErr error
				if i == 0 && tc.firstFails {
					resultErr = tp.NewNonRetryableApplicationError("Site unavailable", swe.ErrTypeNICoUnavailable, errors.New("Site unavailable"))
				} else if i == 0 && tc.cancelFirst {
					resultErr = context.Canceled
				}
				run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
					requestCtx := args.Get(0).(context.Context)
					deadline, bounded := requestCtx.Deadline()
					require.True(t, bounded)
					require.LessOrEqual(t, time.Until(deadline), 65*time.Second)
					if tc.cancelFirst {
						cancel()
					}
					payload, err := protojson.Marshal(&corev1.Domain{
						Id: &corev1.DomainId{Value: row.ControllerDomainID.String()}, Name: row.Hostname,
					})
					require.NoError(t, err)
					args.Get(1).(*grpcproxy.Response).ResponseJSON = payload
				}).Return(resultErr).Once()
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName,
					mock.MatchedBy(func(request grpcproxy.Request) bool {
						var forwarded corev1.CreateDomainRequest
						return request.FullMethod == corev1.Forge_CreateDomain_FullMethodName &&
							protojson.Unmarshal(request.RequestJSON, &forwarded) == nil &&
							forwarded.GetReservedId().GetValue() == row.ControllerDomainID.String()
					})).Return(run, nil).Once()
				t.Cleanup(func() { run.AssertExpectations(t) })
			}

			err = (ManageDomain{DB: session, Sites: pool}).ReconcileReservedDomains(ctx)
			if tc.cancelFirst {
				require.ErrorIs(t, err, context.Canceled)
			} else {
				require.NoError(t, err)
			}
			for i, row := range rows {
				stored, err := dao.GetByID(context.Background(), nil, row.ID, nil)
				require.NoError(t, err)
				expectedStatus, expectedAttempts := cdbm.DomainStatusReady, 1
				if tc.cancelFirst || (i == 0 && tc.firstFails) {
					expectedStatus = cdbm.DomainStatusPending
				}
				if tc.cancelFirst && i > 0 {
					expectedAttempts = 0
				}
				require.Equal(t, expectedStatus, stored.Status)
				require.Equal(t, expectedAttempts, stored.RecoveryAttempts)
				require.Nil(t, stored.RecoveryToken)
			}
			client.AssertExpectations(t)
		})
	}
}
