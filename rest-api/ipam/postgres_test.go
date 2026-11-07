/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDatasource(t *testing.T) {

	tests := []struct {
		name     string
		host     string
		port     string
		user     string
		password string
		dbname   string
		sslmode  SSLMode
		want     string
		wantErr  bool
	}{
		{
			name:     "basic, no escape",
			host:     "host",
			port:     "5432",
			user:     "user",
			password: "password",
			dbname:   "dbname",
			sslmode:  SSLModeAllow,
			want:     "host=host user=user password=password dbname=dbname port=5432 sslmode=allow",
			wantErr:  false,
		},
		{
			name:     "username and password with escape chars",
			host:     "host",
			port:     "5432",
			user:     "us@r",
			password: "pass:word",
			dbname:   "dbname",
			sslmode:  SSLModeAllow,
			want:     "host=host user=us@r password=pass:word dbname=dbname port=5432 sslmode=allow",
			wantErr:  false,
		},
		{
			name:     "username and password with very special characters",
			user:     "us@r",
			password: "+S-@u]JBpWo^kduE7+(25zts",
			dbname:   "dbname",
			sslmode:  SSLModeAllow,
			want:     "host= user=us@r password=+S-@u]JBpWo^kduE7+(25zts dbname=dbname port= sslmode=allow",
			wantErr:  false,
		},
		{
			name:     "space allowed in dbname",
			host:     "host",
			port:     "5432",
			user:     "user",
			password: "password",
			dbname:   "db name",
			sslmode:  SSLModeAllow,
			want:     "host=host user=user password=password dbname=db name port=5432 sslmode=allow",
			wantErr:  false,
		},
		{
			name:     "empty password",
			host:     "host",
			port:     "5432",
			user:     "user",
			password: "",
			dbname:   "db name",
			sslmode:  SSLModeAllow,
			want:     "host=host user=user password= dbname=db name port=5432 sslmode=allow",
			wantErr:  false,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			got := dataSource(tc.host, tc.port, tc.user, tc.password, tc.dbname, tc.sslmode)
			require.Equal(t, tc.want, got)
		})
	}
}
