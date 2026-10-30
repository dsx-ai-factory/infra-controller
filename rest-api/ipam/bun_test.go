/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	dbsql "database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"testing"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/stdlib"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"
	"github.com/uptrace/bun/extra/bundebug"
)

// These tests are supplemental to the tests in sql_test.go
// which also exercise bunpostgres storage
// Here, the tests attempt to simulate the API client
// interactions with ipam (with an existing transaction)

// TestDBConfig describes a test DB config params
type TestDBConfig struct {
	Host     string
	Port     string
	Name     string
	User     string
	Password string
}

// getTestDBParams returns the DB params for a test DB
func getTestDBParams() TestDBConfig {
	tdbcfg := TestDBConfig{
		Host:     "localhost",
		Port:     "30432",
		Name:     "nicotest",
		User:     "postgres",
		Password: "postgres",
	}

	if os.Getenv("CI") == "true" {
		tdbcfg.Host = "postgres"
		tdbcfg.Port = "5432"
	}

	return tdbcfg
}

// newBunPostgres will open a connection to a database
func newBunPostgres(host, port, user, password, dbname string) (*bun.DB, error) {
	configDSN := fmt.Sprintf("postgres://%v:%v@%v:%v/%v", url.PathEscape(user), url.PathEscape(password), host, port, dbname)

	config, err := pgx.ParseConfig(configDSN)
	if err != nil {
		return nil, err
	}
	config.DefaultQueryExecMode = pgx.QueryExecModeSimpleProtocol

	sqldb := stdlib.OpenDB(*config)
	db := bun.NewDB(sqldb, pgdialect.New())

	return db, nil
}

func (s *Bundb) debug() {
	if testing.Verbose() {
		s.db.AddQueryHook(bundebug.NewQueryHook(bundebug.WithEnabled(true), bundebug.WithVerbose(true)))
	}
}

func getTestBundbSession(t *testing.T) (*bun.DB, error) {
	cfg := getTestDBParams()
	return newBunPostgres(cfg.Host, cfg.Port, cfg.User, cfg.Password, "postgres")
}

func getTestSqlxSession(t *testing.T) (sqlIf, error) {
	cfg := getTestDBParams()
	return newPostgres(cfg.Host, cfg.Port, cfg.User, cfg.Password, "postgres", SSLModeDisable)
}

func getTestBadBundbSession(t *testing.T) (*bun.DB, error) {
	cfg := getTestDBParams()
	return newBunPostgres("1.2.3.4", cfg.Port, cfg.User, cfg.Password, "postgres")
}

func dumpConnectionCount(t *testing.T, dbbun *Bundb) {
	if testing.Verbose() {
		cnt, err := dbbun.getConnectionCount()
		assert.Nil(t, err)
		t.Logf("Bundb connections: %d\n", cnt)
	}
}

func testSetupSchema(t *testing.T, dbbun *Bundb) {
	assert.Nil(t, dbbun.ApplyDbSchema())
}

func TestBundbGetIDB(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	defer func(rollback func() error) {
		err := rollback()
		if !errors.Is(err, dbsql.ErrTxDone) {
			assert.NoError(t, err)
		}
	}(tx.Rollback)
	dbbun.userTx = &tx
	assert.Nil(t, err)
	tests := []struct {
		name        string
		Bundb       *Bundb
		expectedIDB bun.IDB
		zeroUserTx  bool
	}{
		{
			name:        "when usertx is present",
			Bundb:       dbbun,
			expectedIDB: dbbun.userTx,
			zeroUserTx:  true,
		},
		{
			name:        "when usertx is NOT present",
			Bundb:       dbbun,
			expectedIDB: dbbun.db,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			idb := tc.Bundb.getIDB()
			assert.Equal(t, tc.expectedIDB, idb)
			if tc.zeroUserTx {
				tc.Bundb.userTx = nil
			}
		})
	}
}

func TestBundbBeginTx(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	assert.Nil(t, err)
	dbbun := NewBunStorage(dbSession, nil)
	t.Log(dbbun.Name())
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	defer func(rollback func() error) {
		err := rollback()
		if !errors.Is(err, dbsql.ErrTxDone) {
			assert.NoError(t, err)
		}
	}(tx.Rollback)
	dbbun.userTx = &tx
	assert.Nil(t, err)
	tests := []struct {
		name         string
		Bundb        *Bundb
		expectUserTx bool
		zeroUserTx   bool
	}{
		{
			name:         "when usertx is present",
			Bundb:        dbbun,
			expectUserTx: true,
			zeroUserTx:   true,
		},
		{
			name:         "when usertx is NOT present",
			Bundb:        dbbun,
			expectUserTx: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			temptx, err := tc.Bundb.beginTx(ctx)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectUserTx, temptx == &tx)
			if !tc.expectUserTx {
				assert.NoError(t, temptx.Rollback())
			}
			if tc.zeroUserTx {
				tc.Bundb.userTx = nil
			}
		})
	}
}

func TestBundbCommitTx(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	dumpConnectionCount(t, dbbun)
	tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	dbbun.userTx = &tx
	assert.Nil(t, err)
	err = dbbun.commitTx(&tx)
	assert.Nil(t, err)
	err = tx.Commit()
	assert.Nil(t, err)
	dumpConnectionCount(t, dbbun)
}

func TestBundbRollbackTx(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	dumpConnectionCount(t, dbbun)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	dbbun.userTx = &tx
	assert.Nil(t, err)
	err = dbbun.rollbackTx(&tx)
	assert.Nil(t, err)
	err = tx.Rollback()
	assert.Nil(t, err)
	dumpConnectionCount(t, dbbun)
}

func TestBundbprefixExists(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	dbsqlx, err := getTestSqlxSession(t)
	assert.Nil(t, err)
	defer dbsqlx.close()
	dumpConnectionCount(t, dbbun)
	db := dbbun.db
	tx, err := db.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	// create prefix with sqlx, read from Bundb
	prefix := Prefix{Cidr: "10.0.0.0/16"}
	p, err := dbsqlx.CreatePrefix(ctx, prefix, prefix.Namespace)
	assert.Nil(t, err)
	assert.NotNil(t, p)
	assert.Equal(t, prefix.Cidr, p.Cidr)
	apiDbSession := NewBunStorage(db, &tx)
	got, exists := apiDbSession.prefixExists(ctx, prefix, prefix.Namespace)
	assert.True(t, exists)
	assert.Equal(t, got.Cidr, prefix.Cidr)
	// Delete Existing Prefix
	_, err = apiDbSession.DeletePrefix(ctx, prefix, prefix.Namespace)
	assert.Nil(t, err)
	got, exists = apiDbSession.prefixExists(ctx, prefix, prefix.Namespace)
	assert.False(t, exists)
	assert.Nil(t, got)
	// create prefix with Bundb, read from sqlx
	p, err = apiDbSession.CreatePrefix(ctx, prefix, prefix.Namespace)
	assert.Nil(t, err)
	assert.NotNil(t, p)
	assert.Equal(t, prefix.Cidr, p.Cidr)
	err = tx.Commit()
	assert.Nil(t, err)
	got, exists = dbsqlx.prefixExists(ctx, prefix, prefix.Namespace)
	assert.True(t, exists)
	assert.Equal(t, got.Cidr, prefix.Cidr)
	// Delete Existing Prefix
	apiDbSession = NewBunStorage(db, nil)
	_, err = apiDbSession.DeletePrefix(ctx, prefix, prefix.Namespace)
	assert.Nil(t, err)
	got, exists = apiDbSession.prefixExists(ctx, prefix, prefix.Namespace)
	assert.False(t, exists)
	assert.Nil(t, got)
	dumpConnectionCount(t, dbbun)
}

func TestBundbReadPrefix(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	require.NoError(t, dbbun.DeleteAllPrefixesFromAllNamespaces(ctx))
	dumpConnectionCount(t, dbbun)
	db := dbbun.db
	tx, err := db.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	apiDbSession := NewBunStorage(db, &tx)
	p, err := apiDbSession.ReadPrefix(ctx, "12.0.0.0/8", "a")
	assert.NotNil(t, err)
	assert.Equal(t, "unable to read prefix:sql: no rows in result set", err.Error())
	assert.Empty(t, p)
	prefix := Prefix{Cidr: "12.0.0.0/16", Namespace: "a"}
	p, err = apiDbSession.CreatePrefix(ctx, prefix, prefix.Namespace)
	assert.Nil(t, err)
	assert.NotNil(t, p)
	p, err = apiDbSession.ReadPrefix(ctx, "12.0.0.0/16", "a")
	assert.Nil(t, err)
	assert.NotNil(t, p)
	assert.Equal(t, "12.0.0.0/16", p.Cidr)
	assert.Equal(t, "a", p.Namespace)
	ps, err := apiDbSession.ReadAllPrefixes(ctx, "a")
	assert.Nil(t, err)
	assert.NotNil(t, ps)
	assert.Equal(t, 1, len(ps))
	assert.Equal(t, "12.0.0.0/16", ps[0].Cidr)
	assert.Equal(t, "a", ps[0].Namespace)
	err = tx.Commit()
	assert.Nil(t, err)
	p, err = apiDbSession.ReadPrefix(ctx, "12.0.0.0/16", "a")
	assert.NotNil(t, err)
	t.Logf("error: %v\n", err)
	_, err = apiDbSession.ReadAllPrefixes(ctx, "a")
	assert.NotNil(t, err)
	t.Logf("error: %v\n", err)
	dumpConnectionCount(t, dbbun)
}

func TestBundbReadAllPrefixes(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	require.NoError(t, dbbun.DeleteAllPrefixesFromAllNamespaces(ctx))
	dumpConnectionCount(t, dbbun)
	db := dbbun.db
	tx, err := db.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	apiDbSession := NewBunStorage(db, &tx)
	// no Prefixes
	ps, err := apiDbSession.ReadAllPrefixCidrs(ctx, "")
	require.Nil(t, err)
	require.NotNil(t, ps)
	require.Equal(t, 0, len(ps))

	// One Prefix
	prefix := Prefix{Cidr: "12.0.0.0/16"}
	p, err := apiDbSession.CreatePrefix(ctx, prefix, prefix.Namespace)
	require.Nil(t, err)
	require.NotNil(t, p)

	ps, err = apiDbSession.ReadAllPrefixCidrs(ctx, "")
	require.Nil(t, err)
	require.NotNil(t, ps)
	require.Equal(t, 1, len(ps))

	// no Prefixes again
	_, err = apiDbSession.DeletePrefix(ctx, prefix, prefix.Namespace)
	require.Nil(t, err)
	ps, err = apiDbSession.ReadAllPrefixCidrs(ctx, "")
	require.Nil(t, err)
	require.NotNil(t, ps)
	require.Equal(t, 0, len(ps))

	err = tx.Commit()
	require.NoError(t, err)
	_, err = apiDbSession.CreatePrefix(ctx, prefix, prefix.Namespace)
	require.NotNil(t, err)
	_, err = apiDbSession.DeletePrefix(ctx, prefix, prefix.Namespace)
	require.NotNil(t, err)
	_, err = apiDbSession.ReadAllPrefixCidrs(ctx, "")
	require.NotNil(t, err)
	_, err = apiDbSession.ReadAllPrefixes(ctx, "")
	require.NotNil(t, err)
	_, err = apiDbSession.ReadAllPrefixes(ctx, "")
	require.NotNil(t, err)

	dumpConnectionCount(t, dbbun)
}

func TestBundbUpdatePrefix(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	assert.Nil(t, err)
	defer dbbun.close()
	testSetupSchema(t, dbbun)
	require.NoError(t, dbbun.DeleteAllPrefixesFromAllNamespaces(ctx))
	dumpConnectionCount(t, dbbun)

	tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	apiDbSession := NewBunStorage(dbSession, &tx)

	// Prefix
	prefix := Prefix{Cidr: "13.0.0.0/16", ParentCidr: "13.0.0.0/8"}
	p, err := apiDbSession.CreatePrefix(ctx, prefix, prefix.Namespace)
	require.Nil(t, err)
	require.NotNil(t, p)

	// Check if present
	p, err = apiDbSession.ReadPrefix(ctx, "13.0.0.0/16", "")
	require.Nil(t, err)
	require.NotNil(t, p)
	require.Equal(t, "13.0.0.0/16", p.Cidr)
	require.Equal(t, "13.0.0.0/8", p.ParentCidr)
	assert.Nil(t, tx.Commit())

	// Modify
	tx, err = dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	apiDbSession = NewBunStorage(dbSession, &tx)
	prefix.ParentCidr = "13.0.0.0/12"
	p, err = apiDbSession.UpdatePrefix(ctx, prefix, prefix.Namespace)
	require.Nil(t, err)
	require.NotNil(t, p)
	assert.Nil(t, tx.Commit())

	tx, err = dbSession.BeginTx(ctx, &dbsql.TxOptions{})
	assert.Nil(t, err)
	apiDbSession = NewBunStorage(dbSession, &tx)
	p, err = apiDbSession.ReadPrefix(ctx, "13.0.0.0/16", "")
	require.Nil(t, err)
	require.NotNil(t, p)
	require.Equal(t, "13.0.0.0/16", p.Cidr)
	require.Equal(t, "13.0.0.0/12", p.ParentCidr)
	assert.Nil(t, tx.Rollback())
}

// test concurrent apis attempting to acquire a child prefix
func TestBundbConcurrentAcquirePrefix(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	dbbun := NewBunStorage(dbSession, nil)
	defer dbSession.DB.Close()
	testSetupSchema(t, dbbun)

	dumpConnectionCount(t, dbbun)
	assert.Nil(t, err)

	require.NoError(t, dbbun.DeleteAllPrefixesFromAllNamespaces(ctx))

	ipamerBun := NewWithStorage(dbbun)

	const parentCidr = "1.0.0.0/16"
	_, err = ipamerBun.NewPrefix(ctx, parentCidr)
	assert.Nil(t, err)
	count := 10
	prefixes := make(chan string)
	for i := 0; i < count; i++ {
		assert.Nil(t, err)
		go func() {
			tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
			assert.Nil(t, err)
			apiIpam := NewWithStorage(NewBunStorage(dbSession, &tx))
			acquirePrefix(t, ctx, apiIpam, parentCidr, prefixes)
			assert.Nil(t, tx.Commit())
		}()
	}

	prefixMap := make(map[string]bool)
	for i := 0; i < count; i++ {
		p := <-prefixes
		_, duplicate := prefixMap[p]
		if duplicate {
			t.Errorf("prefix:%s already acquired", p)
		}
		prefixMap[p] = true
	}
	dumpConnectionCount(t, dbbun)

}

// test concurrent apis attempting to acquire IP
func TestBundbConcurrentAcquireIP(t *testing.T) {
	ctx := context.Background()
	dbSession, err := getTestBundbSession(t)
	assert.Nil(t, err)
	dbbun := NewBunStorage(dbSession, nil)
	defer dbbun.close()
	testSetupSchema(t, dbbun)

	dumpConnectionCount(t, dbbun)
	require.NoError(t, dbbun.DeleteAllPrefixesFromAllNamespaces(ctx))

	ipamerBun := NewWithStorage(dbbun)
	const parentCidr = "2.7.0.0/16"
	_, err = ipamerBun.NewPrefix(ctx, parentCidr)
	assert.Nil(t, err)

	count := 15
	ips := make(chan string)
	for i := 0; i < count; i++ {
		go func() {
			tx, err := dbSession.BeginTx(ctx, &dbsql.TxOptions{})
			assert.Nil(t, err)
			apiIpam := NewWithStorage(NewBunStorage(dbSession, &tx))
			acquireIP(t, ctx, apiIpam, parentCidr, ips)
			assert.Nil(t, tx.Commit())
		}()
	}

	ipMap := make(map[string]bool)
	for i := 0; i < count; i++ {
		p := <-ips
		_, duplicate := ipMap[p]
		if duplicate {
			t.Errorf("prefix:%s already acquired", p)
		}
		ipMap[p] = true
	}
	dumpConnectionCount(t, dbbun)

}
