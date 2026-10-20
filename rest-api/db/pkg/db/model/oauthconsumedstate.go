// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"bytes"
	"context"
	"time"

	"github.com/uptrace/bun"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// OAuthConsumedState represents entries in the oauth_consumed_state table.
type OAuthConsumedState struct {
	bun.BaseModel `bun:"table:oauth_consumed_state,alias:ocs"`

	StateDigest []byte    `bun:"state_digest,pk,type:bytea"`
	ExpiresAt   time.Time `bun:"expires_at,notnull,type:timestamptz"`
}

// OAuthConsumedStateCreateInput input parameters for Create method
type OAuthConsumedStateCreateInput struct {
	StateDigest []byte
	ExpiresAt   time.Time
}

var _ bun.BeforeCreateTableHook = (*OAuthConsumedState)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ocs *OAuthConsumedState) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ColumnExpr("CONSTRAINT oauth_consumed_state_digest_len_chk CHECK (octet_length(state_digest) = 32)")
	return nil
}

// OAuthConsumedStateDAO is an interface for interacting with the OAuthConsumedState model
type OAuthConsumedStateDAO interface {
	// Create inserts a new OAuthConsumedState row if absent and reports whether it was created
	Create(ctx context.Context, tx *db.Tx, input OAuthConsumedStateCreateInput) (*OAuthConsumedState, bool, error)
	// DeleteExpired deletes up to limit rows whose expiry is at or before expiredBefore
	DeleteExpired(ctx context.Context, tx *db.Tx, expiredBefore time.Time, limit int) error
}

// OAuthConsumedStateSQLDAO is an implementation of the OAuthConsumedStateDAO interface
type OAuthConsumedStateSQLDAO struct {
	dbSession *db.Session
	OAuthConsumedStateDAO
}

// Create inserts a new OAuthConsumedState from the given parameters if the digest does not already exist
func (ocssd OAuthConsumedStateSQLDAO) Create(ctx context.Context, tx *db.Tx, input OAuthConsumedStateCreateInput) (_ *OAuthConsumedState, _ bool, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ocsDAOSpan := cotel.StartSpan(ctx, "OAuthConsumedStateDAO.Create")
	defer func() { cotel.EndSpan(ocsDAOSpan, retErr) }()

	ocs := &OAuthConsumedState{
		StateDigest: bytes.Clone(input.StateDigest),
		ExpiresAt:   input.ExpiresAt.UTC(),
	}

	res, err := db.GetIDB(tx, ocssd.dbSession).
		NewInsert().
		Model(ocs).
		On("CONFLICT (state_digest) DO NOTHING").
		Exec(ctx)
	if err != nil {
		return nil, false, err
	}

	rows, err := res.RowsAffected()
	if err != nil {
		return nil, false, err
	}
	if rows == 0 {
		return nil, false, nil
	}

	return ocs, true, nil
}

// DeleteExpired deletes up to limit OAuthConsumedState rows whose expiry is at or before expiredBefore
func (ocssd OAuthConsumedStateSQLDAO) DeleteExpired(ctx context.Context, tx *db.Tx, expiredBefore time.Time, limit int) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ocsDAOSpan := cotel.StartSpan(ctx, "OAuthConsumedStateDAO.DeleteExpired")
	defer func() { cotel.EndSpan(ocsDAOSpan, retErr) }()

	if limit <= 0 {
		return nil
	}

	idb := db.GetIDB(tx, ocssd.dbSession)
	expiredSubquery := idb.NewSelect().
		Model((*OAuthConsumedState)(nil)).
		Column("state_digest").
		Where("ocs.expires_at <= ?", expiredBefore.UTC()).
		Limit(limit)

	_, err := idb.NewDelete().
		Model((*OAuthConsumedState)(nil)).
		Where("state_digest IN (?)", expiredSubquery).
		Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewOAuthConsumedStateDAO returns a new OAuthConsumedStateDAO
func NewOAuthConsumedStateDAO(dbSession *db.Session) OAuthConsumedStateDAO {
	return &OAuthConsumedStateSQLDAO{
		dbSession: dbSession,
	}
}
