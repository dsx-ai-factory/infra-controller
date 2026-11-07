// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/migrations"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"

	log "github.com/sirupsen/logrus"
)

var _ FirmwareUpdateStore = (*PostgresStore)(nil)

// PostgresStore is a Postgres-backed implementation of FirmwareUpdateStore.
type PostgresStore struct {
	session *cdb.Session
}

// NewPostgresStore initializes connectivity to Postgres and runs any pending migrations.
func NewPostgresStore(ctx context.Context, c cdb.Config) (*PostgresStore, error) {
	session, err := cdb.NewSessionFromConfig(ctx, c)
	if err != nil {
		return nil, err
	}

	if err := migrations.MigrateWithDB(ctx, session.DB); err != nil {
		session.Close()
		return nil, fmt.Errorf("failed to run migrations: %w", err)
	}

	return &PostgresStore{session}, nil
}

func (ps *PostgresStore) Start(ctx context.Context) error {
	log.Printf("Starting PostgresQL FW Store")
	return nil
}

func (ps *PostgresStore) Stop(ctx context.Context) error {
	log.Printf("Stopping PostgresQL FW Store")
	ps.session.Close()
	return nil
}

func (ps *PostgresStore) CreateOrReplace(ctx context.Context, mac net.HardwareAddr, component powershelf.Component, versionFrom, versionTo string) (*FirmwareUpdateRecord, error) {
	fu, err := model.NewFirmwareUpdate(ctx, ps.session.DB, mac, component, versionFrom, versionTo)
	if err != nil {
		return nil, err
	}
	return modelToRecord(fu), nil
}

func (ps *PostgresStore) Get(ctx context.Context, mac net.HardwareAddr, component powershelf.Component) (*FirmwareUpdateRecord, error) {
	fu, err := model.GetFirmwareUpdate(ctx, ps.session.DB, mac, component)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, ErrNotFound
		}
		return nil, err
	}
	return modelToRecord(fu), nil
}

func (ps *PostgresStore) GetAllPending(ctx context.Context) ([]*FirmwareUpdateRecord, error) {
	updates, err := model.GetAllPendingFirmwareUpdates(ctx, ps.session.DB)
	if err != nil {
		return nil, err
	}
	records := make([]*FirmwareUpdateRecord, len(updates))
	for i := range updates {
		records[i] = modelToRecord(&updates[i])
	}
	return records, nil
}

func (ps *PostgresStore) SetState(ctx context.Context, mac net.HardwareAddr, component powershelf.Component, newState powershelf.FirmwareState, errMsg string) error {
	return model.SetFirmwareUpdateState(ctx, ps.session.DB, mac, component, newState, errMsg)
}

func modelToRecord(fu *model.FirmwareUpdate) *FirmwareUpdateRecord {
	return &FirmwareUpdateRecord{
		PmcMacAddress:      net.HardwareAddr(fu.PmcMacAddress),
		Component:          fu.Component,
		VersionFrom:        fu.VersionFrom,
		VersionTo:          fu.VersionTo,
		State:              fu.State,
		JobID:              fu.JobID,
		ErrorMessage:       fu.ErrorMessage,
		LastTransitionTime: fu.LastTransitionTime,
		UpdatedAt:          fu.UpdatedAt,
	}
}
