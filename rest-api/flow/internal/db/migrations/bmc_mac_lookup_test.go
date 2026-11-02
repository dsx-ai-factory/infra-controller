// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations_test

import (
	"context"
	_ "embed"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/store/storetest"
)

//go:embed 20260912010000_bmc_mac_lookup.up.sql
var bmcMACLookupUp string

//go:embed 20260912010000_bmc_mac_lookup.down.sql
var bmcMACLookupDown string

func TestBMCMACLookupMigration(t *testing.T) {
	ctx := context.Background()
	session := storetest.NewPostgresTestSession(t)
	_, err := session.DB.ExecContext(ctx, bmcMACLookupDown)
	require.NoError(t, err)

	owner := uuid.New()
	_, err = session.DB.ExecContext(ctx, "INSERT INTO component (id, type) VALUES (?, 'Compute')", owner)
	require.NoError(t, err)
	_, err = session.DB.ExecContext(ctx, "INSERT INTO bmc (mac_address, type, component_id) VALUES ('D8:AB:CD:EF:00:01', 'Host', ?), ('d8:ab:cd:ef:00:01', 'Host', ?)", owner, owner)
	require.NoError(t, err)

	_, err = session.DB.ExecContext(ctx, bmcMACLookupUp)
	require.NoError(t, err, "index must accept predecessor records, including case variants")
	_, err = session.DB.ExecContext(ctx, "INSERT INTO bmc (mac_address, type, component_id) VALUES ('D8:Ab:Cd:Ef:00:01', 'Host', ?)", owner)
	require.NoError(t, err, "predecessor writers must remain compatible")

	var plan string
	err = session.DB.RunInTx(ctx, nil, func(ctx context.Context, tx bun.Tx) error {
		_, err := tx.ExecContext(ctx, "SET LOCAL enable_seqscan = off")
		if err != nil {
			return err
		}
		return tx.NewRaw("EXPLAIN (FORMAT JSON) SELECT component_id FROM bmc WHERE lower(mac_address) = lower(?)", "d8:ab:cd:ef:00:01").Scan(ctx, &plan)
	})
	require.NoError(t, err)
	require.Contains(t, plan, "bmc_mac_address_lower_idx")

	_, err = session.DB.ExecContext(ctx, bmcMACLookupDown)
	require.NoError(t, err)
	var macs []string
	err = session.DB.NewRaw("SELECT mac_address FROM bmc ORDER BY mac_address").Scan(ctx, &macs)
	require.NoError(t, err)
	require.ElementsMatch(t, []string{"D8:AB:CD:EF:00:01", "d8:ab:cd:ef:00:01", "D8:Ab:Cd:Ef:00:01"}, macs, "upgrade and rollback must preserve persisted identities")
}
