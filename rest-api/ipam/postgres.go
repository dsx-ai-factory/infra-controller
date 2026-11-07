/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"fmt"

	"github.com/jmoiron/sqlx"

	// import for sqlx to use postgres driver
	_ "github.com/lib/pq"
)

const postgresSchema = `
CREATE TABLE IF NOT EXISTS prefixes (
	cidr      text NOT NULL,
	prefix    JSONB,
	namespace text NOT NULL DEFAULT '',
	PRIMARY KEY (cidr, namespace)
);
-- FIXME write primary key migration as well
-- ALTER TABLE prefixes DROP CONSTRAINT IF EXISTS prefixes_pkey;
-- ALTER TABLE prefixes ALTER PRIMARY KEY (cidr, namespace);
ALTER TABLE prefixes ADD COLUMN IF NOT EXISTS namespace text NOT NULL DEFAULT '';
UPDATE prefixes SET namespace = DEFAULT WHERE namespace IS NULL;

CREATE INDEX IF NOT EXISTS prefix_idx ON prefixes USING GIN(prefix);
`

// SSLMode specifies how to configure ssl encryption to the database
type SSLMode string

func (s SSLMode) String() string {
	return "sslmode=" + string(s)
}

const (
	// SSLModeAllow I don't care about security
	// but I will pay the overhead of encryption if the server insists on it
	SSLModeAllow = SSLMode("allow")
	// SSLModeDisable I don't care about security
	// and I don't want to pay the overhead of encryption.
	SSLModeDisable = SSLMode("disable")
	// SSLModePrefer I don't care about encryption
	// but I wish to pay the overhead of encryption if the server supports it.
	SSLModePrefer = SSLMode("prefer")
	// SSLModeRequire I want my data to be encrypted and I accept the overhead.
	// I trust that the network will make sure I always connect to the server I want.
	SSLModeRequire = SSLMode("require")
	// SSLModeVerifyCA I want my data encrypted and I accept the overhead.
	// I want to be sure that I connect to a server that I trust.
	SSLModeVerifyCA = SSLMode("verify-ca")
	// SSLModeVerifyFull I want my data encrypted and I accept the overhead.
	// I want to be sure that I connect to a server I trust, and that it's the one I specify.
	SSLModeVerifyFull = SSLMode("verify-full")
)

// NewPostgresStorage creates a new Storage which uses postgres.
func NewPostgresStorage(host, port, user, password, dbname string, sslmode SSLMode) (Storage, error) {
	return newPostgres(host, port, user, password, dbname, sslmode)
}

func newPostgres(host, port, user, password, dbname string, sslmode SSLMode) (*sql, error) {
	db, err := sqlx.Connect("postgres", dataSource(host, port, user, password, dbname, sslmode))
	if err != nil {
		return nil, fmt.Errorf("unable to connect to database:%w", err)
	}
	db.MustExec(postgresSchema)
	return &sql{
		db: db,
	}, nil
}

func dataSource(host, port, user, password, dbname string, sslmode SSLMode) string {
	return fmt.Sprintf("host=%s user=%s password=%s dbname=%s port=%s %s",
		host, user, password, dbname, port, sslmode)
}
