// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIServiceAccount(t *testing.T) {
	type args struct {
		serviceAccountEnabled bool
		dbProvider            *cdbm.InfrastructureProvider
		dbTenant              *cdbm.Tenant
	}

	dbProvider := &cdbm.InfrastructureProvider{
		ID: uuid.New(),
	}
	dbTenant := &cdbm.Tenant{
		ID: uuid.New(),
	}

	tests := []struct {
		name string
		args args
		want *APIServiceAccount
	}{
		{
			name: "test NewAPIServiceAccount with service account enabled",
			args: args{
				serviceAccountEnabled: true,
				dbProvider:            dbProvider,
				dbTenant:              dbTenant,
			},
			want: &APIServiceAccount{
				Enabled:                  true,
				InfrastructureProviderID: cutil.GetPtr(dbProvider.ID.String()),
				TenantID:                 cutil.GetPtr(dbTenant.ID.String()),
			},
		},
		{
			name: "test NewAPIServiceAccount with service account disabled",
			args: args{
				serviceAccountEnabled: false,
			},
			want: &APIServiceAccount{
				Enabled: false,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIServiceAccount(tt.args.serviceAccountEnabled, tt.args.dbProvider, tt.args.dbTenant)
			assert.Equal(t, tt.want, got)
		})
	}
}
