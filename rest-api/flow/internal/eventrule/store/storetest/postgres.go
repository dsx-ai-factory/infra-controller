// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package storetest

import (
	"context"
	"os"
	"testing"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/stretchr/testify/require"
)

// NewPostgresTestSession creates an isolated, migrated database for one
// event-rule PostgreSQL test and registers its cleanup.
func NewPostgresTestSession(t *testing.T) *cdb.Session {
	t.Helper()

	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping PostgreSQL event-rule test: no DB environment specified")
	}

	config, err := cdb.ConfigFromEnv()
	require.NoError(t, err)

	session, err := utils.UnitTestDB(context.Background(), t, config)
	require.NoError(t, err)
	t.Cleanup(session.Close)

	return session
}
