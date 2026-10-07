// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"context"
	"net"
	"testing"

	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	dbtestutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/testutil"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/migrations"
)

func UnitTestDB(ctx context.Context, t *testing.T, dbConf cdb.Config) (*cdb.Session, error) {
	session, err := dbtestutil.CreateTestDB(ctx, t, dbConf)

	if err != nil {
		log.Warn().Msgf("Not running unit test due to unable to connect to db: %v", err)
		t.SkipNow()
		return nil, err
	}

	err = migrations.MigrateWithDB(ctx, session.DB, migrations.MigrateOptions{})

	return session, err
}

// NormalizeMAC parses a MAC address string and returns its canonical
// lowercase colon-separated form (e.g. "aa:bb:cc:dd:ee:ff").
// Handles colon-separated, hyphen-separated, and dot-separated (Cisco)
// formats, as well as mixed casing.
// If the input is not a valid MAC, it is returned unchanged so that
// downstream validation can report the real error.
func NormalizeMAC(mac string) string {
	hw, err := net.ParseMAC(mac)
	if err != nil {
		return mac
	}
	return hw.String()
}
