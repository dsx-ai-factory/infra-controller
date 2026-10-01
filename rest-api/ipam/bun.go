/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	dbsql "database/sql"
	"encoding/json"
	"fmt"

	"github.com/uptrace/bun"
)

// Use bun library to interact with the database
// This is largely a "port" of sql.go which uses sqlx
// sqlx is translated to use bun constructs

// sqlIf is the interface implemented by sql providers (sqlx, bun)
// note that sqlIf is a superset of the storage interface
// it is convenient to have this interface for tests
type sqlIf interface {
	prefixExists(ctx context.Context, prefix Prefix, namespace string) (*Prefix, bool)
	CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	ReadPrefix(ctx context.Context, prefix, namespace string) (Prefix, error)
	DeleteAllPrefixes(ctx context.Context, namespace string) error
	ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error)
	ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error)
	UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error)
	CreateNamespace(ctx context.Context, namespace string) error
	ListNamespaces(ctx context.Context) ([]string, error)
	DeleteNamespace(ctx context.Context, namespace string) error
	Name() string
	cleanup() error
	close()
}

// Bundb represents the bun ipam storage. it has the bun db session,
// and possibly a user transaction that needs to be used
type Bundb struct {
	sqlIf
	// this is database session
	db *bun.DB
	// this is a Tx that the user may have started already
	// which needs to be used in ipam db interactions
	userTx *bun.Tx
}

// NewBunStorage will create a new Bundb interface to IPAM. The caller owns
// database instrumentation; REST's shared DB session attaches the Bun tracing
// hook before passing the DB here. Attaching another hook in this wrapper would
// duplicate spans because a new wrapper can be created for each transaction.
func NewBunStorage(db *bun.DB, tx *bun.Tx) *Bundb {
	return &Bundb{
		db:     db,
		userTx: tx,
	}
}

// BunPrefix is a bun model of the ipam database table
type BunPrefix struct {
	bun.BaseModel `bun:"table:prefixes,alias:bp"`

	Cidr      string          `bun:"cidr,notnull"`
	Prefix    json.RawMessage `bun:"prefix,type:jsonb"`
	Namespace string          `bun:"namespace,notnull"`
}

// getIDB will get the database interface
func (s *Bundb) getIDB() bun.IDB {
	if s.userTx != nil {
		return s.userTx
	}
	return s.db
}

// beginTx - if a user transaction is already present, return that
// else, create a new transaction and return that
func (s *Bundb) beginTx(ctx context.Context) (*bun.Tx, error) {
	if s.userTx != nil {
		// use the user's tx
		return s.userTx, nil
	}
	tx, err := s.db.BeginTx(ctx, &dbsql.TxOptions{})
	if err != nil {
		return nil, err
	}
	return &tx, nil
}

// commitTx - if a user transaction is present do nothing
// otherwise, commit the transaction
func (s *Bundb) commitTx(tx *bun.Tx) error {
	if s.userTx != nil {
		// dont do anything if the user has a transaction
		return nil
	}
	// user didnt have a transaction, commit the transaction
	return tx.Commit()
}

// rollbackTx - if a user transaction is present do nothing
// otherwise, rollback the transaction
func (s *Bundb) rollbackTx(tx *bun.Tx) error {
	if s.userTx != nil {
		// dont do anything if the user has a transaction
		return nil
	}
	// rollback the transaction
	return tx.Rollback()
}

// prefixExists - does the prefix exist
func (s *Bundb) prefixExists(ctx context.Context, prefix Prefix, namespace string) (*Prefix, bool) {
	p, err := s.ReadPrefix(ctx, prefix.Cidr, prefix.Namespace)
	if err != nil {
		return nil, false
	}
	return &p, true
}

// CreatePrefix - create the prefix
func (s *Bundb) CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	existingPrefix, exists := s.prefixExists(ctx, prefix, prefix.Namespace)
	if exists {
		return *existingPrefix, nil
	}
	prefix.version = int64(0)
	pj, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}
	tx, err := s.beginTx(ctx)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to start transaction:%w", err)
	}
	value := &BunPrefix{Cidr: prefix.Cidr, Namespace: prefix.Namespace, Prefix: pj}
	_, err = tx.NewInsert().Model(value).Exec(ctx)
	if err != nil {
		s.rollbackTx(tx)
		return Prefix{}, fmt.Errorf("unable to insert prefix:%w", err)
	}
	return prefix, s.commitTx(tx)
}

// ReadPrefix - read the prefix
func (s *Bundb) ReadPrefix(ctx context.Context, prefix, namespace string) (Prefix, error) {
	p := &BunPrefix{}
	query := s.getIDB().NewSelect().Model(p).Where("cidr = ? AND namespace = ?", prefix, namespace)
	err := query.Scan(ctx)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to read prefix:%w", err)
	}
	return fromJSON(p.Prefix)
}

// ReadPrefixes - read all the prefixes for the namespace
func (s *Bundb) ReadAllPrefixes(ctx context.Context, namespace string) (Prefixes, error) {
	var prefixes [][]byte
	ps := []BunPrefix{}
	query := s.getIDB().NewSelect().Model(&ps).Where("namespace = ?", namespace)
	err := query.Scan(ctx)
	if err != nil {
		return nil, fmt.Errorf("unable to read prefixes in namespace:%s %w", namespace, err)
	}
	for _, p := range ps {
		prefixes = append(prefixes, p.Prefix)
	}
	return toPrefixes(prefixes)
}

// DeleteAllPrefixes will delete all prefixes - used in tests
func (s *Bundb) DeleteAllPrefixesFromAllNamespaces(ctx context.Context) error {
	_, err := s.getIDB().ExecContext(ctx, "DELETE FROM prefixes")
	return err
}

// DeleteAllPrefixes will delete all prefixes - used in tests
func (s *Bundb) DeleteAllPrefixes(ctx context.Context, namespace string) error {
	_, err := s.getIDB().ExecContext(ctx, "DELETE FROM prefixes WHERE namespace = ?", namespace)
	return err
}

// ReadAllPrefixCidrs is cheaper than ReadAllPrefixes because it only returns the Cidrs.
func (s *Bundb) ReadAllPrefixCidrs(ctx context.Context, namespace string) ([]string, error) {
	ps := []BunPrefix{}
	cidrs := []string{}
	query := s.getIDB().NewSelect().Model(&ps).Column("cidr").Where("namespace = ?", namespace)
	err := query.Scan(ctx)
	if err != nil {
		return nil, fmt.Errorf("unable to read prefixes in namespace:%s %w", namespace, err)
	}
	for _, p := range ps {
		cidrs = append(cidrs, p.Cidr)
	}
	return cidrs, nil
}

// UpdatePrefix tries to update the prefix.
// Returns OptimisticLockError if it does not succeed due to a concurrent update.
func (s *Bundb) UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	oldVersion := prefix.version
	prefix.version = oldVersion + 1
	pn, err := prefix.toJSON()
	if err != nil {
		return Prefix{}, err
	}
	tx, err := s.beginTx(ctx)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to start transaction:%w", err)
	}
	p := BunPrefix{}
	query := tx.NewSelect().Model(&p).Column("prefix").Where("cidr = ? AND namespace = ? AND bp.prefix->>'Version' = '?'", prefix.Cidr, prefix.Namespace, oldVersion).For("UPDATE")
	err = query.Scan(ctx)
	if err != nil {
		s.rollbackTx(tx)
		return Prefix{}, fmt.Errorf("%w: unable to select for update prefix:%s", ErrOptimisticLockError, prefix.Cidr)
	}
	p.Prefix = pn
	result, err := tx.NewUpdate().Model(&p).Column("prefix").Where("cidr = ? AND namespace = ? AND prefix->>'Version' = '?'", prefix.Cidr, prefix.Namespace, oldVersion).Exec(ctx)
	if err != nil {
		s.rollbackTx(tx)
		return Prefix{}, fmt.Errorf("%w: unable to update prefix:%s", ErrOptimisticLockError, prefix.Cidr)
	}
	rows, err := result.RowsAffected()
	if err != nil {
		s.rollbackTx(tx)
		return Prefix{}, err
	}
	if rows == 0 {
		s.rollbackTx(tx)
		return Prefix{}, fmt.Errorf("%w: updatePrefix did not effect any row", ErrOptimisticLockError)
	}
	return prefix, s.commitTx(tx)
}

// DeletePrefix will delete a prefix
func (s *Bundb) DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (Prefix, error) {
	tx, err := s.beginTx(ctx)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to start transaction:%w", err)
	}
	p := BunPrefix{}
	_, err = tx.NewDelete().Model(&p).Where("cidr = ? and namespace = ?", prefix.Cidr, prefix.Namespace).Exec(ctx)
	if err != nil {
		s.rollbackTx(tx)
		return Prefix{}, fmt.Errorf("unable delete prefix:%w", err)
	}
	return prefix, s.commitTx(tx)
}

// Name is the name of the ipam storage interface
func (s *Bundb) Name() string {
	return "bunpostgres"
}

// ApplyDbSchema will apply the original ipam db schema
// this schema is idempotent (ie, if schema already exists, no harm applying it again)
// this function will be incorporated into cloud-db as a bun migration
func (s *Bundb) ApplyDbSchema() error {
	_, err := s.db.Exec(postgresSchema)
	return err
}

// Utilities only used in tests

// close closes the db connection
func (s *Bundb) close() {
	s.db.Close()
}

// getConnectionCount will get number of connections to the db
func (s *Bundb) getConnectionCount() (int, error) {
	return s.db.NewSelect().Table("pg_stat_activity").Count(context.Background())
}
