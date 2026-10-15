// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package readiness

import (
	"context"
	"errors"
	"regexp"
	"testing"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
)

func TestDBReader_GetHostExternalIDsByRackIDs(t *testing.T) {
	const (
		coreRackID     = "D09"
		coreUUIDRackID = "11111111-1111-1111-1111-111111111111"
	)

	rackQuery := regexp.QuoteMeta(`SELECT r.external_id AS rack_external_id, c.external_id AS host_external_id FROM "rack" AS "r" LEFT JOIN component AS c ON (c.rack_id = r.id) AND (c.type = 'Compute') AND (c.external_id IS NOT NULL AND c.external_id != '') AND (c.deleted_at IS NULL) WHERE (r.external_id IN (`) + `.*` + regexp.QuoteMeta(`)) AND (r.deleted_at IS NULL)`)

	tests := []struct {
		name      string
		rackIDs   []string
		setup     func(sqlmock.Sqlmock)
		want      map[string][]string
		wantError string
	}{
		{
			name:    "resolves non UUID Core rack ID to Flow rack UUID",
			rackIDs: []string{coreRackID},
			setup: func(mock sqlmock.Sqlmock) {
				mock.ExpectQuery(rackQuery).
					WillReturnRows(sqlmock.NewRows([]string{"rack_external_id", "host_external_id"}).
						AddRow(coreRackID, "compute-1").
						AddRow(coreRackID, "compute-2"))
			},
			want: map[string][]string{coreRackID: {"compute-1", "compute-2"}},
		},
		{
			name:    "resolves UUID shaped Core rack ID instead of using it as Flow rack UUID",
			rackIDs: []string{coreUUIDRackID},
			setup: func(mock sqlmock.Sqlmock) {
				mock.ExpectQuery(rackQuery).
					WillReturnRows(sqlmock.NewRows([]string{"rack_external_id", "host_external_id"}).
						AddRow(coreUUIDRackID, "compute-1"))
			},
			want: map[string][]string{coreUUIDRackID: {"compute-1"}},
		},
		{
			name:    "keeps a resolved empty rack distinct from an unresolved rack",
			rackIDs: []string{coreRackID},
			setup: func(mock sqlmock.Sqlmock) {
				mock.ExpectQuery(rackQuery).
					WillReturnRows(sqlmock.NewRows([]string{"rack_external_id", "host_external_id"}).
						AddRow(coreRackID, nil))
			},
			want: map[string][]string{coreRackID: nil},
		},
		{
			name:    "rejects an unresolved Core rack ID",
			rackIDs: []string{coreRackID, "D10"},
			setup: func(mock sqlmock.Sqlmock) {
				mock.ExpectQuery(rackQuery).
					WillReturnRows(sqlmock.NewRows([]string{"rack_external_id", "host_external_id"}).
						AddRow(coreRackID, nil))
			},
			wantError: "resolve rack external IDs: no rack found for D10",
		},
		{
			name:    "returns rack resolution query errors",
			rackIDs: []string{coreRackID},
			setup: func(mock sqlmock.Sqlmock) {
				mock.ExpectQuery(rackQuery).
					WillReturnError(errors.New("database unavailable"))
			},
			wantError: "select host components by rack: database unavailable",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sqlDB, mock, err := sqlmock.New()
			require.NoError(t, err)
			t.Cleanup(func() { _ = sqlDB.Close() })

			db := bun.NewDB(sqlDB, pgdialect.New())
			tt.setup(mock)

			got, err := NewDBReader(db).GetHostExternalIDsByRackIDs(context.Background(), tt.rackIDs)
			if tt.wantError != "" {
				require.EqualError(t, err, tt.wantError)
				require.Nil(t, got)
			} else {
				require.NoError(t, err)
				require.Equal(t, tt.want, got)
			}
			require.NoError(t, mock.ExpectationsWereMet())
		})
	}
}
