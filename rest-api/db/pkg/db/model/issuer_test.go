// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
)

const testJWKSBlob = `{"keys":[{"kty":"RSA","use":"sig","kid":"key-1","alg":"RS256","n":"n","e":"AQAB"}]}`

func testIssuerInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// testIssuerSetupSchema resets the issuer table and adds the partial unique
// indexes that the migration creates (ResetModel builds only from struct tags).
func testIssuerSetupSchema(t *testing.T, dbSession *db.Session) {
	ctx := context.Background()
	require.NoError(t, dbSession.DB.ResetModel(ctx, (*Issuer)(nil)))
	_, err := dbSession.DB.Exec("CREATE UNIQUE INDEX IF NOT EXISTS issuer_issuer_url_idx ON issuer(issuer_url) WHERE deleted IS NULL")
	require.NoError(t, err)
}

func createTestIssuer(t *testing.T, ctx context.Context, dbSession *db.Session, dao IssuerDAO) *Issuer {
	t.Helper()

	creator := uuid.New()
	var created *Issuer
	err := db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
		var derr error
		created, derr = dao.Create(ctx, tx, IssuerCreateOrUpdateInput{
			Origin:      "custom",
			IssuerURL:   "https://idp.jwks-cache.test",
			JWKSUrl:     "https://idp.jwks-cache.test/.well-known/jwks.json",
			JWKSTimeout: "5s",
			ClaimMappings: []ClaimMapping{
				{OrgName: "acme", OrgDisplayName: "ACME", Roles: []string{"TENANT_ADMIN"}},
			},
			CreatedBy: &creator,
		})
		return derr
	})
	require.NoError(t, err)
	require.NotNil(t, created)

	return created
}

func TestIssuerSQLDAO_CRUD(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	creator := uuid.New()

	// Create
	var created *Issuer
	err := db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
		var derr error
		created, derr = dao.Create(ctx, tx, IssuerCreateOrUpdateInput{
			Origin:      "custom",
			IssuerURL:   "https://idp.acme.com",
			JWKSUrl:     "https://idp.acme.com/.well-known/jwks.json",
			JWKSTimeout: "5s",
			Audiences:   []string{"api.acme.com"},
			Scopes:      []string{"carbide"},
			ClaimMappings: []ClaimMapping{
				{OrgName: "acme", OrgDisplayName: "ACME", Roles: []string{"TENANT_ADMIN"}},
			},
			CreatedBy: &creator,
		})
		return derr
	})
	require.NoError(t, err)
	require.NotNil(t, created)
	assert.Equal(t, "https://idp.acme.com", created.IssuerURL)
	require.Len(t, created.ClaimMappings, 1)
	assert.Equal(t, "acme", created.ClaimMappings[0].OrgName)
	assert.Equal(t, []string{"TENANT_ADMIN"}, created.ClaimMappings[0].Roles)
	assert.Equal(t, []string{"api.acme.com"}, created.Audiences)

	// GetByID
	got, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.Equal(t, created.ID, got.ID)
	require.Len(t, got.ClaimMappings, 1)
	assert.Equal(t, "acme", got.ClaimMappings[0].OrgName)

	// GetAll
	all, err := dao.GetAll(ctx, nil, IssuerFilterInput{})
	require.NoError(t, err)
	require.Len(t, all, 1)

	// Delete (soft) → GetByID returns ErrDoesNotExist
	err = db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
		return dao.Delete(ctx, tx, created.ID)
	})
	require.NoError(t, err)
	_, err = dao.GetByID(ctx, nil, created.ID)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)
}

func TestIssuerSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)
	fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, json.RawMessage(testJWKSBlob), fetchedAt))

	actor := uuid.New()
	input := IssuerCreateOrUpdateInput{
		Origin:      "custom",
		IssuerURL:   created.IssuerURL,
		JWKSUrl:     created.JWKSUrl,
		JWKSTimeout: "9s",
		Audiences:   []string{"new-audience"},
		ClaimMappings: []ClaimMapping{
			{OrgName: "new-org", Roles: []string{"TENANT_ADMIN"}},
		},
		UpdatedBy: &actor,
	}

	t.Run("preserves cache for the same endpoint", func(t *testing.T) {
		updated, err := dao.Update(ctx, nil, created.ID, input, false)
		require.NoError(t, err)

		assert.Equal(t, created.ID, updated.ID)
		assert.Equal(t, created.CreatedAt, updated.CreatedAt)
		assert.Equal(t, "9s", updated.JWKSTimeout)
		assert.Equal(t, []string{"new-audience"}, updated.Audiences)
		assert.JSONEq(t, testJWKSBlob, string(updated.JWKSKeys))
		require.NotNil(t, updated.JWKSFetchedAt)
		assert.True(t, fetchedAt.Equal(*updated.JWKSFetchedAt))
		assert.Equal(t, &actor, updated.UpdatedBy)

		byURL, getErr := dao.GetByIssuerURL(ctx, nil, created.IssuerURL)
		require.NoError(t, getErr)
		assert.Equal(t, created.ID, byURL.ID)
	})

	t.Run("clears cache for a new endpoint", func(t *testing.T) {
		failed := "connection refused"
		require.NoError(t, db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			_, err := dao.RecordJWKSAttempt(ctx, tx, created.ID, created.JWKSUrl, "a", fetchedAt, &failed, time.Time{})
			return err
		}))
		input.JWKSUrl = "https://new-jwks.example.com/keys"
		updated, err := dao.Update(ctx, nil, created.ID, input, true)
		require.NoError(t, err)

		assert.Equal(t, input.JWKSUrl, updated.JWKSUrl)
		assert.Empty(t, updated.JWKSKeys)
		assert.Nil(t, updated.JWKSFetchedAt)
		assert.Empty(t, updated.JWKSReplicaFailures, "the old endpoint's failures do not describe the new one")
	})
}

func TestIssuerSQLDAO_DuplicateURLRejected(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)

	mk := func(url, jwksURL string) error {
		return db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			_, derr := dao.Create(ctx, tx, IssuerCreateOrUpdateInput{
				IssuerURL: url,
				JWKSUrl:   jwksURL,
			})
			return derr
		})
	}

	require.NoError(t, mk("https://idp.example.com", "https://idp.example.com/jwks"))

	// Same issuer URL, different JWKS URL → unique violation on issuer_url.
	err := mk("https://idp.example.com", "https://other.example.com/jwks")
	require.Error(t, err)
	assert.True(t, (&db.PostgresErrorChecker{}).IsUniqueConstraintError(err), "expected unique constraint error, got: %v", err)
}

func TestIssuerSQLDAO_SoftDeletedURLCanBeRecreated(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	create := func() *Issuer {
		var created *Issuer
		err := db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			var derr error
			created, derr = dao.Create(ctx, tx, IssuerCreateOrUpdateInput{
				IssuerURL: "https://recreated.example.com",
				JWKSUrl:   "https://recreated.example.com/jwks",
			})
			return derr
		})
		require.NoError(t, err)
		return created
	}

	first := create()
	require.NoError(t, db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
		return dao.Delete(ctx, tx, first.ID)
	}))
	second := create()

	assert.NotEqual(t, first.ID, second.ID)
	rows, err := dao.GetAll(ctx, nil, IssuerFilterInput{})
	require.NoError(t, err)
	require.Len(t, rows, 1)
	assert.Equal(t, second.ID, rows[0].ID)
}

func TestIssuerSQLDAO_GetPage(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	for _, name := range []string{"charlie", "alpha", "bravo"} {
		err := db.WithTx(ctx, dbSession, func(tx *db.Tx) error {
			_, derr := dao.Create(ctx, tx, IssuerCreateOrUpdateInput{
				IssuerURL: "https://" + name + ".example.com",
				JWKSUrl:   "https://" + name + ".example.com/jwks",
			})
			return derr
		})
		require.NoError(t, err)
	}

	offset, limit := 1, 1
	rows, total, err := dao.GetPage(ctx, nil, IssuerFilterInput{}, paginator.PageInput{
		Offset: &offset,
		Limit:  &limit,
		OrderBy: &paginator.OrderBy{
			Field: "issuer_url",
			Order: paginator.OrderAscending,
		},
	})
	require.NoError(t, err)
	assert.Equal(t, 3, total)
	require.Len(t, rows, 1)
	assert.Equal(t, "https://bravo.example.com", rows[0].IssuerURL)
}

func TestIssuerSQLDAO_UpdateJWKSCache(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)

	require.Empty(t, created.JWKSKeys, "a new issuer has no cached key set")
	require.Nil(t, created.JWKSFetchedAt)

	fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, json.RawMessage(testJWKSBlob), fetchedAt))

	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.JSONEq(t, testJWKSBlob, string(stored.JWKSKeys))
	require.NotNil(t, stored.JWKSFetchedAt)
	assert.WithinDuration(t, fetchedAt, *stored.JWKSFetchedAt, time.Millisecond)

	// The audit fields describe operator edits. A background key refresh is not
	// one, so the restricted column list must have kept them out of the SET clause.
	assert.Equal(t, created.UpdatedAt.UTC(), stored.UpdatedAt.UTC(), "updated_at must not move on a key refresh")
	assert.Equal(t, created.UpdatedBy, stored.UpdatedBy, "updated_by must not move on a key refresh")
}

// TestIssuerSQLDAO_UpdateJWKSCacheStaleWriteGuard covers two replicas refreshing
// the same issuer at once: the fetch that finishes second with older keys must
// not clobber the newer set.
func TestIssuerSQLDAO_UpdateJWKSCacheStaleWriteGuard(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)

	newer := time.Now().UTC()
	older := newer.Add(-time.Minute)
	staleBlob := `{"keys":[{"kty":"RSA","use":"sig","kid":"stale-key","alg":"RS256","n":"n","e":"AQAB"}]}`

	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, json.RawMessage(testJWKSBlob), newer))
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, json.RawMessage(staleBlob), older),
		"a losing stale write is a no-op, not an error")

	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.JSONEq(t, testJWKSBlob, string(stored.JWKSKeys), "the newer key set must survive")
	assert.WithinDuration(t, newer, *stored.JWKSFetchedAt, time.Millisecond)
}

// TestIssuerSQLDAO_UpdateJWKSCacheSkipsDeletedRow covers a refresh that finishes
// after the issuer was withdrawn: the dead row must not gain a fresh key set.
func TestIssuerSQLDAO_UpdateJWKSCacheSkipsDeletedRow(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)
	require.NoError(t, dao.Delete(ctx, nil, created.ID))

	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, json.RawMessage(testJWKSBlob), time.Now().UTC()))

	var row Issuer
	require.NoError(t, dbSession.DB.NewSelect().Model(&row).WhereAllWithDeleted().Where("id = ?", created.ID).Scan(ctx))
	assert.Empty(t, row.JWKSKeys, "a soft-deleted issuer must not be written by a late refresh")
	assert.Nil(t, row.JWKSFetchedAt)
}

func TestIssuerSQLDAO_UpdateJWKSCacheRejectsEmpty(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)

	assert.Error(t, dao.UpdateJWKSCache(ctx, nil, created.ID, created.JWKSUrl, nil, time.Now().UTC()))
}

// TestIssuerSignatureIgnoresJWKSCache is the guard for the invariant that makes
// the whole cache viable. If the cached key set entered the signature, every
// refresh would read as a config change and every replica would rebuild its
// JwksConfig, discarding the keys it had just fetched.
func TestIssuerSignatureIgnoresJWKSCache(t *testing.T) {
	fetchedAt := time.Now().UTC()
	base := Issuer{
		ID:            uuid.New(),
		Origin:        "custom",
		IssuerURL:     "https://idp.acme.com",
		JWKSUrl:       "https://idp.acme.com/.well-known/jwks.json",
		JWKSTimeout:   "5s",
		Audiences:     []string{"api.acme.com"},
		Scopes:        []string{"carbide"},
		ClaimMappings: []ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}},
	}

	withKeys := base
	withKeys.JWKSKeys = json.RawMessage(testJWKSBlob)
	withKeys.JWKSFetchedAt = &fetchedAt

	assert.Equal(t, base.Signature(), withKeys.Signature(),
		"caching a key set must not look like a configuration change")

	// A real configuration change must still be detected.
	changed := base
	changed.JWKSUrl = "https://idp.acme.com/other/jwks.json"
	assert.NotEqual(t, base.Signature(), changed.Signature())
}

// TestIssuerSQLDAO_RecordJWKSAttempt: the row keeps the latest attempt across
// replicas and one failure entry per failing replica, which only that replica's
// newer success removes and which expires when its replica stops renewing it.
func TestIssuerSQLDAO_RecordJWKSAttempt(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)
	t1 := time.Now().UTC().Truncate(time.Microsecond)
	failed := "connection refused"
	record := func(jwksURL, replica string, at time.Time, fetchErr *string, staleBefore time.Time) *Issuer {
		row, err := db.WithTxResult(ctx, dbSession, func(tx *db.Tx) (*Issuer, error) {
			return dao.RecordJWKSAttempt(ctx, tx, created.ID, jwksURL, replica, at, fetchErr, staleBefore)
		})
		require.NoError(t, err)
		return row
	}
	never := time.Time{}

	row := record(created.JWKSUrl, "a", t1, &failed, never)
	assert.Equal(t, map[string]JWKSReplicaFailure{"a": {At: t1, Error: failed}}, row.JWKSReplicaFailures)

	row = record(created.JWKSUrl, "b", t1.Add(time.Minute), nil, never)
	assert.Contains(t, row.JWKSReplicaFailures, "a", "another replica's success does not clear a's failure")
	assert.Nil(t, row.JWKSLastError)

	row = record(created.JWKSUrl, "a", t1.Add(-time.Minute), nil, never)
	assert.Contains(t, row.JWKSReplicaFailures, "a", "an older success does not clear a newer failure")
	assert.True(t, t1.Add(time.Minute).Equal(*row.JWKSLastAttemptAt), "the row keeps the latest attempt")

	assert.Nil(t, record("https://other.example.com/jwks", "a", t1.Add(2*time.Minute), nil, never), "an attempt against another endpoint is refused")

	row = record(created.JWKSUrl, "a", t1.Add(2*time.Minute), nil, never)
	assert.Empty(t, row.JWKSReplicaFailures, "a's own newer success clears it")

	record(created.JWKSUrl, "b", t1.Add(3*time.Minute), &failed, never)
	row = record(created.JWKSUrl, "a", t1.Add(4*time.Minute), nil, t1.Add(3*time.Minute+time.Second))
	assert.Empty(t, row.JWKSReplicaFailures, "an entry its replica stopped renewing expires")

	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.Empty(t, stored.JWKSReplicaFailures)
	assert.Equal(t, IssuerStatusPending, stored.Status, "status is the caller's to set")
}

// TestIssuerSQLDAO_UpdateJWKSCacheIgnoresAMovedEndpoint: after a PUT moves
// jwks_url and clears the cache, a refresh of the old endpoint must not fill it.
func TestIssuerSQLDAO_UpdateJWKSCacheIgnoresAMovedEndpoint(t *testing.T) {
	ctx := context.Background()
	dbSession := testIssuerInitDB(t)
	defer dbSession.Close()
	testIssuerSetupSchema(t, dbSession)

	dao := NewIssuerDAO(dbSession)
	created := createTestIssuer(t, ctx, dbSession, dao)
	oldJWKS := created.JWKSUrl

	moved, err := dao.Update(ctx, nil, created.ID, IssuerCreateOrUpdateInput{
		IssuerURL: created.IssuerURL, JWKSUrl: "https://new-idp.example.com/jwks",
		ClaimMappings: created.ClaimMappings,
	}, true)
	require.NoError(t, err)
	require.Nil(t, moved.JWKSFetchedAt)

	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, oldJWKS, json.RawMessage(testJWKSBlob), time.Now().UTC()))
	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.Empty(t, stored.JWKSKeys, "keys fetched from the old endpoint are not cached under the new one")
	assert.Nil(t, stored.JWKSFetchedAt)

	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, moved.JWKSUrl, json.RawMessage(testJWKSBlob), time.Now().UTC()))
	stored, err = dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.True(t, stored.HasCachedKeys(), "keys from the current endpoint are cached")
}
