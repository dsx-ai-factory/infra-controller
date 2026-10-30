// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/require"

	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestPrepareProviderSiteHandler(t *testing.T) {
	db := testRackInitDB(t)
	defer db.Close()
	ctx := context.Background()
	org := "provider-site-helper-test"
	_, site, _ := testRackSetupTestData(t, db, org)
	user := testRackBuildUser(t, db, "provider-site-helper-user", org, []string{auth.ProviderAdminRole})
	site.Config = &cdbm.SiteConfig{Flow: false}
	_, err := db.DB.NewUpdate().Model(site).Column("config").WherePK().Exec(ctx)
	require.NoError(t, err)

	resolved, apiErr := prepareProviderSiteHandler(ctx, db, user, org, site.ID.String(), zerolog.Nop())
	require.Nil(t, apiErr)
	require.NotNil(t, resolved)
	require.Equal(t, site.ID, resolved.ID)
	require.False(t, resolved.Config.Flow)
}

func TestPrepareFlowHandler(t *testing.T) {
	tests := []struct {
		name   string
		config *cdbm.SiteConfig
	}{
		{name: "missing config"},
		{name: "Flow disabled", config: &cdbm.SiteConfig{Flow: false}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			db := testRackInitDB(t)
			defer db.Close()
			ctx := context.Background()
			org := "flow-helper-test"
			_, site, _ := testRackSetupTestData(t, db, org)
			user := testRackBuildUser(t, db, "flow-helper-user", org, []string{auth.ProviderAdminRole})
			site.Config = tc.config
			_, err := db.DB.NewUpdate().Model(site).Column("config").WherePK().Exec(ctx)
			require.NoError(t, err)

			// A nil pool proves rejection happens before client retrieval.
			resolved, client, apiErr := prepareFlowHandler(ctx, db, nil, user, org, site.ID.String(), zerolog.Nop())
			require.Nil(t, resolved)
			require.Nil(t, client)
			require.NotNil(t, apiErr)
			require.Equal(t, http.StatusPreconditionFailed, apiErr.Code)
			require.Equal(t, "Site does not have NICo Flow enabled", apiErr.Message)
		})
	}
}
