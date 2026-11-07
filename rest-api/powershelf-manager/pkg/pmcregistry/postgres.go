// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcregistry

import (
	"context"
	"fmt"
	"net"

	log "github.com/sirupsen/logrus"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/errors"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/converter/dao"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
)

// PostgresPmcRegistry implements the PmcRegister interface and it uses PostgresQL
// as the datastore.
type PostgresPmcRegistry struct {
	session *cdb.Session
}

// newPostgresRegistry initializes connectivity to Postgres and runs any pending migrations.
func newPostgresRegistry(ctx context.Context, c cdb.Config) (*PostgresPmcRegistry, error) {
	session, err := cdb.NewSessionFromConfig(ctx, c)
	if err != nil {
		return nil, err
	}

	// Run migrations automatically at startup to ensure schema is up to date
	if err := migrations.MigrateWithDB(ctx, session.DB); err != nil {
		session.Close()

		return nil, fmt.Errorf("failed to run migrations: %w", err)
	}

	return &PostgresPmcRegistry{session}, nil
}

// NewPostgresRegistryFromDB creates a PostgresPmcRegistry from an existing database connection.
// This is useful for tests where migrations have already been applied.
func NewPostgresRegistryFromDB(session *cdb.Session) *PostgresPmcRegistry {
	return &PostgresPmcRegistry{session: session}
}

// Start starts the PostgresStore instance. Currently, it is no-op.
func (ps *PostgresPmcRegistry) Start(ctx context.Context) error {
	log.Printf("Starting PostgresQL PMC Register")
	return nil
}

// Stop stops the PostgresStore instance by closing the PostgresQL connection.
func (ps *PostgresPmcRegistry) Stop(ctx context.Context) error {
	log.Printf("Stopping PostgresQL PMC Register")
	ps.session.Close()
	return nil
}

func (ps *PostgresPmcRegistry) runInTx(
	ctx context.Context,
	operation func(ctx context.Context, tx bun.Tx) error,
) error {
	if err := ps.session.RunInTx(ctx, operation); err != nil {
		if !errors.IsGRPCError(err) {
			err = errors.GRPCErrorInternal(err.Error())
		}

		return err
	}

	return nil
}

// RegisterPmc creates or updates a PMC row via INSERT … ON CONFLICT upsert,
// which is safe under concurrent registrations of the same MAC.
func (ps *PostgresPmcRegistry) RegisterPmc(ctx context.Context, pmc *pmc.PMC) error {
	if pmc == nil {
		return fmt.Errorf("cannot register nil PMC")
	}

	pmcDao := dao.PmcTo(pmc)

	return ps.runInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		_, err := tx.NewInsert().
			Model(pmcDao).
			On("CONFLICT (mac_address) DO UPDATE").
			Set("ip_address = EXCLUDED.ip_address").
			Set("vendor = EXCLUDED.vendor").
			Exec(ctx)
		if err != nil {
			return fmt.Errorf("failed to upsert PMC %s: %w", pmcDao.MacAddress, err)
		}
		return nil
	})
}

// GetPmc queries by MAC and maps DAO → domain.
func (ps *PostgresPmcRegistry) GetPmc(
	ctx context.Context,
	mac net.HardwareAddr,
) (*pmc.PMC, error) {
	pmcModel := &model.PMC{
		MacAddress: model.MacAddr(mac),
	}

	cur, err := pmcModel.Get(ctx, ps.session.DB)
	if err != nil {
		return nil, err
	}

	return dao.PmcFrom(cur)
}

// IsPmcRegistered returns true if a PMC exists.
func (ps *PostgresPmcRegistry) IsPmcRegistered(ctx context.Context, mac net.HardwareAddr) (bool, error) {
	pmc, err := ps.GetPmc(ctx, mac)
	if err != nil {
		return false, err
	}

	return pmc != nil, nil

}

// GetAllPmcs returns all PMCs mapped to domain objects.
func (ps *PostgresPmcRegistry) GetAllPmcs(ctx context.Context) ([]*pmc.PMC, error) {
	pmcDaos := make([]model.PMC, 0)
	err := ps.session.DB.NewSelect().Model(&pmcDaos).Scan(ctx)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	pmcs := make([]*pmc.PMC, 0, len(pmcDaos))
	for _, pmcDao := range pmcDaos {
		pmc, err := dao.PmcFrom(&pmcDao)
		if err != nil {
			return nil, errors.GRPCErrorInternal(err.Error())
		}
		pmcs = append(pmcs, pmc)
	}

	return pmcs, nil
}
