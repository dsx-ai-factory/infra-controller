// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestCreateInstanceHandler_machineUnavailableError(t *testing.T) {
	ctx := context.Background()
	session := testInstanceInitDB(t)
	defer session.Close()
	testInstanceSetupSchema(t, session)
	user := testInstanceBuildUser(t, session, "retry-user", "retry-org", nil)
	provider := testInstanceSiteBuildInfrastructureProvider(t, session, "retry-provider", "retry-provider-org", user)
	site := testInstanceBuildSite(t, session, provider, "retry-site", cdbm.SiteStatusRegistered, true, user)
	tenant := testInstanceBuildTenant(t, session, "retry-tenant", "retry-org", user)
	other := testInstanceBuildTenant(t, session, "other-tenant", "other-org", user)
	vpc := testInstanceBuildVPC(t, session, "retry-vpc", provider, tenant, site, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, user)

	for _, tt := range []struct {
		name    string
		owner   uuid.UUID
		status  string
		deleted bool
		second  bool
		want    *bool
	}{
		{"own release", tenant.ID, cdbm.InstanceStatusTerminating, false, false, cutil.GetPtr(true)},
		{"own live operation", tenant.ID, cdbm.InstanceStatusProvisioning, false, false, cutil.GetPtr(false)},
		{"other tenant live", other.ID, cdbm.InstanceStatusReady, false, false, cutil.GetPtr(false)},
		{"other tenant release", other.ID, cdbm.InstanceStatusTerminating, false, false, cutil.GetPtr(false)},
		{"missing association", uuid.Nil, "", false, false, nil},
		{"historical release is not current", tenant.ID, cdbm.InstanceStatusTerminating, true, false, nil},
		{"ambiguous associations", tenant.ID, cdbm.InstanceStatusTerminating, false, true, nil},
	} {
		t.Run(tt.name, func(t *testing.T) {
			machine := testInstanceBuildMachine(t, session, provider.ID, site.ID, cutil.GetPtr(true), nil)
			if tt.owner != uuid.Nil {
				occupant := testInstanceBuildInstance(t, session, uuid.NewString(), tt.owner, provider.ID, site.ID, nil, vpc.ID, &machine.ID, nil, nil, tt.status)
				if tt.deleted {
					_, err := session.DB.NewDelete().Model(occupant).WherePK().Exec(ctx)
					require.NoError(t, err)
				}
			}
			if tt.second {
				testInstanceBuildInstance(t, session, uuid.NewString(), other.ID, provider.ID, site.ID, nil, vpc.ID, &machine.ID, nil, nil, cdbm.InstanceStatusReady)
			}
			cih := CreateInstanceHandler{dbSession: session}
			err := cdb.WithTx(ctx, session, func(tx *cdb.Tx) error {
				lockErr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(machine.ID), nil)
				require.NoError(t, lockErr)
				locked, getErr := cdbm.NewMachineDAO(session).GetByID(ctx, tx, machine.ID, nil, true)
				require.NoError(t, getErr)
				apiErr := cih.machineUnavailableError(ctx, tx, zerolog.Nop(), locked, tenant.ID, "unavailable")
				assert.Equal(t, http.StatusBadRequest, apiErr.Code)
				assert.Equal(t, tt.want, apiErr.Retryable)
				assert.Empty(t, apiErr.RecoveryAction)
				return nil
			})
			require.NoError(t, err)
		})
	}
}
