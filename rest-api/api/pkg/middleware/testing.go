// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/require"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// TestResolveOrgEntity resolves the request org's entities with a new OrgEntityResolver and stores them in the
// request context, as OrgEntityMiddleware does. Call it after setting "user" on c. Each call uses a new resolver,
// so tests never share a cache.
func TestResolveOrgEntity(t *testing.T, c echo.Context, dbSession *cdb.Session, scp *sc.ClientPool) *OrgEntity {
	t.Helper()
	oe, err := resolveOrgEntity(c, NewOrgEntityResolver(dbSession, scp), zerolog.Nop())
	require.NoError(t, err)
	return oe
}
