// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/rackopreport"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// PostgresStore implements the Store interface using PostgreSQL.
type PostgresStore struct {
	pg *cdb.Session
}

// NewPostgres creates a new PostgreSQL-backed inventory store.
func NewPostgres(pg *cdb.Session) *PostgresStore {
	return &PostgresStore{pg: pg}
}

// Start starts the PostgresStore instance. Currently, it is no-op.
func (s *PostgresStore) Start(ctx context.Context) error {
	return nil
}

// Stop stops the PostgresStore instance by closing the PostgreSQL connection.
func (s *PostgresStore) Stop(ctx context.Context) error {
	s.pg.Close()
	return nil
}

// CreateExpectedRack creates an expected rack in the database and returns its UUID.
func (s *PostgresStore) CreateExpectedRack(
	ctx context.Context,
	rack *rack.Rack,
) (uuid.UUID, error) {
	if !rack.VerifyIDs() {
		return uuid.Nil, errors.GRPCErrorInvalidArgument(
			"rack or components have no IDs",
		)
	}

	rackDevInfo := rack.Info

	operation := func(ctx context.Context, tx bun.Tx) error {
		if err := dao.RackTo(rack).Create(ctx, tx); err != nil {
			if !s.pg.GetErrorChecker().IsUniqueConstraintError(err) {
				rackDevInfo.ID = uuid.Nil
				return err
			}

			_, err := s.getRack(ctx, tx, rackDevInfo, false)
			if err != nil {
				rackDevInfo.ID = uuid.Nil

				if err = s.checkDBGetError(err, ""); err != nil {
					return err
				}

				cur, err := s.getRack(ctx, tx, rackDevInfo, false)
				if err != nil {
					return s.checkDBGetError(
						err,
						rackDevInfo.InfoMsg("rack", false),
					)
				}

				rackDevInfo.ID = cur.Info.ID
			}

			return errors.GRPCErrorAlreadyExists(
				fmt.Sprintf(
					"%s exists",
					rackDevInfo.InfoMsg("rack", rackDevInfo.ID != uuid.Nil),
				),
			)
		}

		for _, c := range rack.Components {
			compDao := dao.ComponentTo(&c, rackDevInfo.ID)
			if err := compDao.Create(ctx, tx); err != nil {
				return errors.GRPCErrorInternal(err.Error())
			}

			for _, bmcDao := range compDao.BMCs {
				if err := bmcDao.Create(ctx, tx); err != nil {
					log.Info().Msgf("failed to create bmc entry: %s", bmcDao.MacAddress)
					return errors.GRPCErrorInternal(err.Error())
				}
			}
		}

		return nil
	}

	return rackDevInfo.ID, s.runInTx(ctx, operation)
}

// GetRackByID retrieves a rack by its UUID, optionally with its components.
func (s *PostgresStore) GetRackByID(
	ctx context.Context,
	id uuid.UUID,
	withComponents bool,
) (*rack.Rack, error) {
	racks, err := s.GetRacksByIDs(ctx, []uuid.UUID{id}, withComponents)
	if err != nil {
		return nil, err
	}
	if len(racks) == 0 {
		return nil, errors.GRPCErrorNotFound("rack not found")
	}
	return racks[0], nil
}

// GetRacksByIDs retrieves multiple racks by their UUIDs, optionally with their components.
func (s *PostgresStore) GetRacksByIDs(
	ctx context.Context,
	ids []uuid.UUID,
	withComponents bool,
) ([]*rack.Rack, error) {
	if len(ids) == 0 {
		return nil, errors.GRPCErrorInvalidArgument("rack ids are not specified")
	}

	validIDs := make([]uuid.UUID, 0, len(ids))
	for _, id := range ids {
		if id != uuid.Nil {
			validIDs = append(validIDs, id)
		}
	}

	if len(validIDs) == 0 {
		return nil, errors.GRPCErrorInvalidArgument("no valid rack ids specified")
	}

	rackDaos, err := model.GetRacksByIDs(ctx, s.pg.DB, validIDs, withComponents)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	results := make([]*rack.Rack, 0, len(rackDaos))
	for _, rackDao := range rackDaos {
		results = append(results, dao.RackFrom(&rackDao))
	}

	return results, nil
}

// GetRacksByIDsIncludingDeleted retrieves multiple racks by UUID, including
// soft-deleted rows.
func (s *PostgresStore) GetRacksByIDsIncludingDeleted(
	ctx context.Context,
	ids []uuid.UUID,
	withComponents bool,
) ([]*rack.Rack, error) {
	if len(ids) == 0 {
		return []*rack.Rack{}, nil
	}

	rackDaos, err := model.GetRacksByIDsIncludingDeleted(ctx, s.pg.DB, ids, withComponents)
	if err != nil {
		return nil, s.checkDBGetError(err, "")
	}

	results := make([]*rack.Rack, 0, len(rackDaos))
	for i := range rackDaos {
		results = append(results, dao.RackFrom(&rackDaos[i]))
	}
	return results, nil
}

// GetRackByExternalID retrieves a rack by its external ID.
func (s *PostgresStore) GetRackByExternalID(
	ctx context.Context,
	externalID string,
	withComponents bool,
) (*rack.Rack, error) {
	if externalID == "" {
		return nil, errors.GRPCErrorInvalidArgument("rack external id is not specified")
	}

	var rackDAO model.Rack
	query := s.pg.DB.NewSelect().Model(&rackDAO).Where("r.external_id = ?", externalID)
	if withComponents {
		query = query.Relation("Components").Relation("Components.BMCs")
	}
	if err := query.Scan(ctx); err != nil {
		return nil, s.checkDBGetError(err, fmt.Sprintf("rack with external id %s", externalID))
	}

	return dao.RackFrom(&rackDAO), nil
}

// GetRackBySerial retrieves a rack by its serial number and manufacturer.
func (s *PostgresStore) GetRackBySerial(
	ctx context.Context,
	manufacturer string,
	serialNumber string,
	withComponents bool,
) (*rack.Rack, error) {
	if len(serialNumber) == 0 {
		return nil,
			errors.GRPCErrorInvalidArgument("serial number is not specfied")
	}

	deviceInfo := deviceinfo.DeviceInfo{
		ID:           uuid.Nil,
		Manufacturer: manufacturer,
		SerialNumber: serialNumber,
	}

	return s.getRack(ctx, s.pg.DB, deviceInfo, withComponents)
}

// GetRackByIdentifier retrieves a rack by its identifier (ID or name).
func (s *PostgresStore) GetRackByIdentifier(
	ctx context.Context,
	identifier identifier.Identifier,
	withComponents bool,
) (*rack.Rack, error) {
	if !identifier.ValidateAtLeastOne() {
		return nil, errors.GRPCErrorInvalidArgument(
			"rack id and name both are not specfied",
		)
	}

	if identifier.ExternalID != "" {
		r, err := s.GetRackByExternalID(ctx, identifier.ExternalID, withComponents)
		if err == nil {
			return r, nil
		}
		if status.Code(err) != codes.NotFound || (identifier.ID == uuid.Nil && identifier.Name == "") {
			return nil, err
		}
	}
	if identifier.ID != uuid.Nil {
		deviceInfo := deviceinfo.DeviceInfo{
			ID: identifier.ID,
		}
		return s.getRack(ctx, s.pg.DB, deviceInfo, withComponents)
	}

	rackDaos, _, err := model.GetListOfRacks(
		ctx,
		s.pg.DB,
		dbquery.StringQueryInfo{
			Patterns:   []string{identifier.Name},
			IsWildcard: false,
			UseOR:      true,
		},
		nil,
		nil,
		&dbquery.Pagination{Limit: 2},
		nil,
		withComponents,
	)

	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	rackDao, err := uniqueRackByName(rackDaos, identifier.Name)
	if err != nil {
		return nil, err
	}
	return dao.RackFrom(rackDao), nil
}

func uniqueRackByName(racks []model.Rack, name string) (*model.Rack, error) {
	switch len(racks) {
	case 0:
		return nil, errors.GRPCErrorNotFound(fmt.Sprintf("rack %s", name))
	case 1:
		return &racks[0], nil
	default:
		return nil, errors.GRPCErrorInvalidArgument(
			fmt.Sprintf("rack name %q matches multiple racks; use rack id", name),
		)
	}
}

// PatchRack updates an existing rack.
func (s *PostgresStore) PatchRack(
	ctx context.Context,
	r *rack.Rack,
) (string, error) {
	report := ""

	if r == nil {
		return report, nil
	}

	if !r.VerifyIDOrSerial() {
		return report, errors.GRPCErrorInvalidArgument(
			"rack or components have no IDs and serial information",
		)
	}

	operation := func(ctx context.Context, tx bun.Tx) error {
		patchInfo, err := s.buildRackPatchInfo(ctx, tx, r)
		if err != nil {
			return err
		}

		if err := patchInfo.patch(); err != nil {
			return err
		}

		report = patchInfo.opReport.Finalize()

		return nil
	}

	return report, s.runInTx(ctx, operation)
}

// GetListOfRacks lists racks matching the given criteria.
func (s *PostgresStore) GetListOfRacks(
	ctx context.Context,
	info dbquery.StringQueryInfo,
	manufacturerFilter *dbquery.StringQueryInfo,
	modelFilter *dbquery.StringQueryInfo,
	pagination *dbquery.Pagination,
	orderBy *dbquery.OrderBy,
	withComponents bool,
) ([]*rack.Rack, int32, error) {
	racks, total, err := model.GetListOfRacks(
		ctx, s.pg.DB, info, manufacturerFilter, modelFilter, pagination, orderBy, withComponents,
	)
	if err != nil {
		return nil, 0, err
	}

	results := make([]*rack.Rack, 0, len(racks))
	for _, rackDao := range racks {
		results = append(results, dao.RackFrom(&rackDao))
	}

	return results, total, nil
}

// GetListOfComponents lists components matching the given criteria.
func (s *PostgresStore) GetListOfComponents(
	ctx context.Context,
	info dbquery.StringQueryInfo,
	manufacturerFilter *dbquery.StringQueryInfo,
	modelFilter *dbquery.StringQueryInfo,
	componentTypes []devicetypes.ComponentType,
	pagination *dbquery.Pagination,
	orderBy *dbquery.OrderBy,
) ([]*component.Component, int32, error) {
	components, total, err := model.GetListOfComponents(
		ctx, s.pg.DB, info, manufacturerFilter, modelFilter, componentTypes, pagination, orderBy,
	)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(err.Error())
	}

	results := make([]*component.Component, 0, len(components))
	for _, compDao := range components {
		results = append(results, dao.ComponentFrom(compDao))
	}

	return results, total, nil
}

// GetComponentByID retrieves a component by its UUID.
func (s *PostgresStore) GetComponentByID(
	ctx context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	if id == uuid.Nil {
		return nil, errors.GRPCErrorInvalidArgument("component id is not specfied")
	}

	deviceInfo := deviceinfo.DeviceInfo{ID: id}
	cur, err := s.getComponent(ctx, s.pg.DB, deviceInfo)
	if err != nil {
		return nil, s.checkDBGetError(
			err,
			deviceInfo.InfoMsg("component", true),
		)
	}

	return cur, nil
}

// GetComponentBySerial retrieves a component by its serial number and manufacturer.
func (s *PostgresStore) GetComponentBySerial(
	ctx context.Context,
	manufacturer string,
	serialNumber string,
	withRack bool,
) (*component.Component, error) {
	if len(serialNumber) == 0 {
		return nil, errors.GRPCErrorInvalidArgument("serial number is not specfied")
	}

	deviceInfo := deviceinfo.DeviceInfo{
		ID:           uuid.Nil,
		Manufacturer: manufacturer,
		SerialNumber: serialNumber,
	}

	cur, err := s.getComponent(ctx, s.pg.DB, deviceInfo)
	if err != nil {
		return nil, s.checkDBGetError(
			err,
			deviceInfo.InfoMsg("component", true),
		)
	}

	return cur, nil
}

// GetComponentByBMCMAC retrieves a component by its BMC MAC address.
func (s *PostgresStore) GetComponentByBMCMAC(
	ctx context.Context,
	macAddress string,
) (*component.Component, error) {
	if len(macAddress) == 0 {
		return nil, errors.GRPCErrorInvalidArgument("mac address is not specified")
	}

	c, err := model.GetComponentByBMCMAC(ctx, s.pg.DB, macAddress)
	if err != nil {
		return nil, s.checkDBGetError(err, fmt.Sprintf("component with BMC MAC %s", macAddress))
	}

	if c == nil {
		return nil, errors.GRPCErrorNotFound(fmt.Sprintf("component with BMC MAC %s not found", macAddress))
	}

	return dao.ComponentFrom(*c), nil
}

// GetComponentsByExternalIDs retrieves components by their external IDs.
func (s *PostgresStore) GetComponentsByExternalIDs(
	ctx context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	if len(externalIDs) == 0 {
		return nil, nil
	}

	var componentModels []model.Component
	err := s.pg.DB.NewSelect().
		Model(&componentModels).
		Where("c.external_id IN (?)", bun.In(externalIDs)).
		Relation("BMCs").
		Relation("Rack").
		Scan(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to query components by external IDs: %w", err)
	}

	result := make([]*component.Component, 0, len(componentModels))
	for _, compModel := range componentModels {
		comp := dao.ComponentFrom(compModel)
		result = append(result, comp)
	}

	return result, nil
}

// CreateNVLDomain creates a new NVL domain.
func (s *PostgresStore) CreateNVLDomain(
	ctx context.Context,
	nvlDomain *nvldomain.NVLDomain,
) (uuid.UUID, error) {
	if err := nvlDomain.Validate(); err != nil {
		return uuid.Nil, errors.GRPCErrorInvalidArgument(err.Error())
	}

	nvlDomainID := nvlDomain.Identifier.ID

	operation := func(ctx context.Context, tx bun.Tx) error {
		if err := dao.NVLDomainTo(nvlDomain).Create(ctx, tx); err != nil {
			if !s.pg.GetErrorChecker().IsUniqueConstraintError(err) {
				return errors.GRPCErrorInternal(err.Error())
			}

			queryNVLDomain := model.NVLDomain{ID: nvlDomain.Identifier.ID}
			if cur, err := queryNVLDomain.Get(ctx, tx); err == nil {
				nvlDomainID = cur.ID
			} else {
				queryNVLDomain = model.NVLDomain{
					ID:   uuid.Nil,
					Name: nvlDomain.Identifier.Name,
				}
				if cur, err := queryNVLDomain.Get(ctx, tx); err == nil {
					nvlDomainID = cur.ID
				} else {
					return errors.GRPCErrorInternal(err.Error())
				}
			}
		}

		return nil
	}

	if err := s.runInTx(ctx, operation); err != nil {
		return uuid.Nil, err
	}

	return nvlDomainID, nil
}

// AttachRacksToNVLDomain attaches racks to an NVL domain.
func (s *PostgresStore) AttachRacksToNVLDomain(
	ctx context.Context,
	nvlDomainID identifier.Identifier,
	rackIDs []identifier.Identifier,
) error {
	if !nvlDomainID.ValidateAtLeastOne() {
		return errors.GRPCErrorInvalidArgument(
			"nvl domain id and name both are not specfied",
		)
	}

	for _, rackID := range rackIDs {
		if !rackID.ValidateAtLeastOne() {
			return errors.GRPCErrorInvalidArgument(
				"rack id and name both are not specfied",
			)
		}
	}

	operation := func(ctx context.Context, tx bun.Tx) error {
		nvlDomain, err := s.getNVLDomain(ctx, tx, nvlDomainID)
		if err != nil {
			return err
		}

		if nvlDomain == nil {
			return errors.GRPCErrorNotFound(
				fmt.Sprintf("nvl domain %+v", nvlDomainID),
			)
		}

		racks := make([]*model.Rack, 0, len(rackIDs))
		for _, rackID := range rackIDs {
			rackDevInfo := deviceinfo.DeviceInfo{
				ID:   rackID.ID,
				Name: rackID.Name,
			}

			rack, err := s.getRackDao(ctx, tx, rackDevInfo, false)
			if err != nil {
				return err
			}

			if rack == nil {
				return errors.GRPCErrorNotFound(
					fmt.Sprintf("rack %+v", rackID),
				)
			}

			if rack.NVLDomainID != uuid.Nil {
				if rack.NVLDomainID != nvlDomain.ID {
					return errors.GRPCErrorAlreadyExists(
						fmt.Sprintf("rack %+v is already attached", rackID),
					)
				}
			} else {
				racks = append(racks, rack)
			}
		}

		for _, rack := range racks {
			rack.NVLDomainID = nvlDomainID.ID
			if err := rack.Patch(ctx, tx); err != nil {
				return errors.GRPCErrorInternal(err.Error())
			}
		}

		return nil
	}

	return s.runInTx(ctx, operation)
}

// DetachRacksFromNVLDomain detaches racks from their NVL domain.
func (s *PostgresStore) DetachRacksFromNVLDomain(
	ctx context.Context,
	rackIDs []identifier.Identifier,
) error {
	for _, rackID := range rackIDs {
		if !rackID.ValidateAtLeastOne() {
			return errors.GRPCErrorInvalidArgument(
				"rack id and name both are not specfied",
			)
		}
	}

	operation := func(ctx context.Context, tx bun.Tx) error {
		racks := make([]*model.Rack, 0, len(rackIDs))
		for _, rackID := range rackIDs {
			rackDevInfo := deviceinfo.DeviceInfo{
				ID:   rackID.ID,
				Name: rackID.Name,
			}

			rack, err := s.getRackDao(ctx, tx, rackDevInfo, false)
			if err != nil {
				return err
			}

			if rack == nil {
				return errors.GRPCErrorNotFound(
					fmt.Sprintf("rack %+v", rackID),
				)
			}

			if rack.NVLDomainID != uuid.Nil {
				racks = append(racks, rack)
			} else {
				return nil
			}
		}

		for _, rack := range racks {
			rack.NVLDomainID = uuid.Nil
			if err := rack.Patch(ctx, tx); err != nil {
				return errors.GRPCErrorInternal(err.Error())
			}
		}

		return nil
	}

	return s.runInTx(ctx, operation)
}

// GetListOfNVLDomains lists NVL domains matching the given criteria.
func (s *PostgresStore) GetListOfNVLDomains(
	ctx context.Context,
	info dbquery.StringQueryInfo,
	pagination *dbquery.Pagination,
) ([]*nvldomain.NVLDomain, int32, error) {
	domains, total, err := model.GetListOfNVLDomains(
		ctx, s.pg.DB, info, pagination,
	)
	if err != nil {
		return nil, 0, err
	}

	results := make([]*nvldomain.NVLDomain, 0, len(domains))
	for _, domainDao := range domains {
		results = append(results, dao.NVLDomainFrom(&domainDao))
	}

	return results, total, nil
}

// GetRacksForNVLDomain retrieves all racks belonging to an NVL domain.
func (s *PostgresStore) GetRacksForNVLDomain(
	ctx context.Context,
	nvlDomainID identifier.Identifier,
) ([]*rack.Rack, error) {
	if !nvlDomainID.ValidateAtLeastOne() {
		return nil, errors.GRPCErrorInvalidArgument(
			"nvl domain id and name both are not specfied",
		)
	}

	results := make([]*rack.Rack, 0)

	operation := func(ctx context.Context, tx bun.Tx) error {
		domainUUID := nvlDomainID.ID
		if domainUUID == uuid.Nil {
			nvlDomain, err := s.getNVLDomain(ctx, tx, nvlDomainID)
			if err != nil {
				return err
			}

			if nvlDomain == nil {
				return errors.GRPCErrorNotFound(
					fmt.Sprintf("nvl domain %+v", nvlDomainID),
				)
			}

			domainUUID = nvlDomain.ID
		}

		racks, err := model.GetRacksForNVLDomain(ctx, tx, domainUUID)
		if err != nil {
			return err
		}

		for _, rack := range racks {
			results = append(results, dao.RackFrom(&rack))
		}

		return nil
	}

	if err := s.runInTx(ctx, operation); err != nil {
		return nil, err
	}

	return results, nil
}

// Helper methods

func (s *PostgresStore) checkDBGetError(err error, info string) error {
	if !s.pg.GetErrorChecker().IsErrNoRows(err) {
		return errors.GRPCErrorInternal(err.Error())
	}

	if len(info) == 0 {
		return nil
	}

	return errors.GRPCErrorNotFound(fmt.Sprintf("%s is not found", info))
}

func (s *PostgresStore) runInTx(
	ctx context.Context,
	operation func(ctx context.Context, tx bun.Tx) error,
) error {
	if err := s.pg.RunInTx(ctx, operation); err != nil {
		if !errors.IsGRPCError(err) {
			err = errors.GRPCErrorInternal(err.Error())
		}
		return err
	}
	return nil
}

func (s *PostgresStore) getNVLDomain(
	ctx context.Context,
	idb bun.IDB,
	nvlDomainID identifier.Identifier,
) (*model.NVLDomain, error) {
	var nvlDomain model.NVLDomain

	if nvlDomainID.ID != uuid.Nil {
		nvlDomain.ID = nvlDomainID.ID
	} else {
		nvlDomain.Name = nvlDomainID.Name
	}

	cur, err := nvlDomain.Get(ctx, idb)
	if err != nil {
		return nil, s.checkDBGetError(
			err,
			fmt.Sprintf("nvl domain %+v", nvlDomainID),
		)
	}

	return cur, nil
}

func (s *PostgresStore) getRack(
	ctx context.Context,
	idb bun.IDB,
	deviceInfo deviceinfo.DeviceInfo,
	withComponents bool,
) (*rack.Rack, error) {
	cur, err := s.getRackDao(ctx, idb, deviceInfo, withComponents)
	if err != nil {
		return nil, err
	}

	return dao.RackFrom(cur), nil
}

func (s *PostgresStore) getComponent(
	ctx context.Context,
	idb bun.IDB,
	deviceInfo deviceinfo.DeviceInfo,
) (*component.Component, error) {
	infoMsg := deviceInfo.InfoMsg("component", deviceInfo.ID != uuid.Nil)

	c := component.New(devicetypes.ComponentTypeUnknown, &deviceInfo, "", nil)
	cur, err := dao.ComponentTo(&c, uuid.Nil).Get(ctx, idb)
	if err != nil {
		return nil, s.checkDBGetError(err, infoMsg)
	}

	return dao.ComponentFrom(*cur), nil
}

func (s *PostgresStore) getRackDao(
	ctx context.Context,
	idb bun.IDB,
	deviceInfo deviceinfo.DeviceInfo,
	withComponents bool,
) (*model.Rack, error) {
	infoMsg := deviceInfo.InfoMsg("rack", deviceInfo.ID != uuid.Nil)

	r := &model.Rack{
		ID:           deviceInfo.ID,
		Manufacturer: deviceInfo.Manufacturer,
		SerialNumber: deviceInfo.SerialNumber,
	}

	cur, err := r.Get(ctx, idb, withComponents)
	if err != nil {
		return nil, s.checkDBGetError(err, infoMsg)
	}

	return cur, nil
}

// Rack patch helpers

type rackPatchInfo struct {
	ctx         context.Context
	tx          bun.Tx
	cdao        *model.Rack
	ndao        *model.Rack
	opReport    *rackopreport.RackOpReport
	compIDToDao map[uuid.UUID]*model.Component
}

func (s *PostgresStore) buildRackPatchInfo(
	ctx context.Context,
	tx bun.Tx,
	r *rack.Rack,
) (*rackPatchInfo, error) {
	curDao, err := s.getRackDao(ctx, tx, r.Info, len(r.Components) > 0)
	if err != nil {
		log.Debug().Msgf("failed to get current rack: %s", err.Error())
		if nerr := s.checkDBGetError(err, ""); nerr != nil {
			return nil, nerr
		}

		if curDao != nil {
			panic("Don't expect the rack to be found")
		}
	}

	compIDToDao := make(map[uuid.UUID]*model.Component)
	compSerialToDao := make(map[deviceinfo.SerialInfo]*model.Component)

	if curDao != nil {
		for _, compDao := range curDao.Components {
			compIDToDao[compDao.ID] = &compDao
			compSerialToDao[compDao.SerialInfo()] = &compDao
		}

		r.Info.ID = curDao.ID
		for i, c := range r.Components {
			var cdao *model.Component
			if c.Info.ID != uuid.Nil {
				cdao = compIDToDao[c.Info.ID]
			} else {
				cdao = compSerialToDao[c.Info.GetSerialInfo()]
			}

			if cdao != nil {
				r.Components[i].Info.ID = cdao.ID
			} else {
				r.Components[i].Info.ID = uuid.New()
			}
		}
	}

	return &rackPatchInfo{
		ctx:         ctx,
		tx:          tx,
		cdao:        curDao,
		ndao:        dao.RackTo(r),
		opReport:    rackopreport.New(r.Info.ID, r.Info.GetSerialInfo()),
		compIDToDao: compIDToDao,
	}, nil
}

func (pi *rackPatchInfo) patch() error {
	if pi.isInvalid() {
		panic("unexpected invalid rackPatchInfo")
	}

	if pi.cdao == nil {
		pi.opReport.UpdateReport("NoExistingRack")
		return nil
	}

	for _, nc := range pi.ndao.Components {
		if nc.ID == uuid.Nil {
			panic("unexpected component with no ID")
		}

		err := pi.patchComponent(&nc, pi.compIDToDao[nc.ID])
		if err != nil {
			log.Debug().Msgf("failed to patch component: %s", err.Error())
			return err
		}
	}

	if pr := pi.ndao.BuildPatch(pi.cdao); pr != nil {
		if err := pr.Patch(pi.ctx, pi.tx); err != nil {
			log.Debug().Msgf("failed to patch rack: %s", err.Error())
			return err
		}

		pi.opReport.UpdateReport("Patched")
	} else {
		pi.opReport.UpdateReport("NoChange")
	}

	return nil
}

func (pi *rackPatchInfo) patchComponent(
	nc *model.Component,
	cc *model.Component,
) error {
	if nc == nil {
		panic("don't expect the input component to be nil")
	}

	if cc == nil {
		if nc.InvalidType() {
			pi.opReport.UpdateCompReport(nc.ID, nc.SerialInfo(), "InvalidType")
			return nil
		}

		if err := nc.Create(pi.ctx, pi.tx); err != nil {
			return errors.GRPCErrorInternal(err.Error())
		}

		pi.opReport.UpdateCompReport(nc.ID, nc.SerialInfo(), "Added")
	} else {
		if pc := nc.BuildPatch(cc); pc != nil {
			if err := pc.Patch(pi.ctx, pi.tx); err != nil {
				log.Debug().Msgf("failed to patch component: %s", err.Error())
				return err
			}

			pi.opReport.UpdateCompReport(nc.ID, nc.SerialInfo(), "Patched")
		} else {
			pi.opReport.UpdateCompReport(nc.ID, nc.SerialInfo(), "NoChange")
		}
	}

	bmcsByMac := make(map[string]*model.BMC)
	if cc != nil {
		for _, bd := range cc.BMCs {
			bmcsByMac[bd.MacAddress] = &bd
		}
	}

	for _, nb := range nc.BMCs {
		var op string

		if cb := bmcsByMac[nb.MacAddress]; cb != nil {
			if pb := nb.BuildPatch(cb); pb != nil {
				if err := pb.Patch(pi.ctx, pi.tx); err != nil {
					log.Debug().Msgf("failed to patch bmc: %s", err.Error())
					return err
				}

				op = "Patched"
			} else {
				op = "NoChange"
			}
		} else {
			if nb.InvalidType() {
				op = "InvalidType"
			} else {
				if err := nb.Create(pi.ctx, pi.tx); err != nil {
					log.Debug().Msgf("failed to create bmc: %s", err.Error())
					return err
				}
				op = "Added"
			}
		}

		pi.opReport.UpdateBMCReport(nc.ID, nc.SerialInfo(), nb.MacAddress, op)
	}

	return nil
}

func (pi *rackPatchInfo) isInvalid() bool {
	return pi == nil || pi.ndao == nil || pi.opReport == nil || pi.compIDToDao == nil
}

// AddComponent creates a single component (with BMCs) in the database and returns its UUID.
func (s *PostgresStore) AddComponent(ctx context.Context, comp *component.Component) (uuid.UUID, error) {
	compDAO := dao.ComponentTo(comp, comp.RackID)

	operation := func(ctx context.Context, tx bun.Tx) error {
		if err := compDAO.Create(ctx, tx); err != nil {
			return err
		}
		for _, bmcDAO := range compDAO.BMCs {
			if err := bmcDAO.Create(ctx, tx); err != nil {
				return err
			}
		}
		return nil
	}

	if err := s.runInTx(ctx, operation); err != nil {
		return uuid.Nil, err
	}
	return compDAO.ID, nil
}

// PatchComponent updates a single component's fields and reconciles BMCs in the database.
func (s *PostgresStore) PatchComponent(ctx context.Context, comp *component.Component) error {
	newDAO := dao.ComponentTo(comp, comp.RackID)

	operation := func(ctx context.Context, tx bun.Tx) error {
		if err := newDAO.Patch(ctx, tx); err != nil {
			return err
		}

		if len(newDAO.BMCs) == 0 {
			return nil
		}

		// Load existing BMCs for this component
		var existingBMCs []model.BMC
		if err := tx.NewSelect().Model(&existingBMCs).
			Where("component_id = ?", newDAO.ID).Scan(ctx); err != nil {
			return err
		}

		bmcsByMac := make(map[string]*model.BMC, len(existingBMCs))
		for i := range existingBMCs {
			bmcsByMac[existingBMCs[i].MacAddress] = &existingBMCs[i]
		}

		for i := range newDAO.BMCs {
			nb := &newDAO.BMCs[i]
			if cb, ok := bmcsByMac[nb.MacAddress]; ok {
				if pb := nb.BuildPatch(cb); pb != nil {
					if err := pb.Patch(ctx, tx); err != nil {
						return err
					}
				}
			} else {
				if !nb.InvalidType() {
					if err := nb.Create(ctx, tx); err != nil {
						return err
					}
				}
			}
		}

		return nil
	}

	return s.runInTx(ctx, operation)
}

// DeleteRack soft-deletes a rack and all its components in a single transaction.
func (s *PostgresStore) DeleteRack(ctx context.Context, id uuid.UUID) error {
	operation := func(ctx context.Context, tx bun.Tx) error {
		rackDAO := &model.Rack{ID: id}
		if _, err := rackDAO.Get(ctx, tx, false); err != nil {
			return s.checkDBGetError(err, fmt.Sprintf("rack %s", id))
		}

		// Soft-delete all components belonging to this rack.
		_, err := tx.NewDelete().Model((*model.Component)(nil)).
			Where("rack_id = ?", id).
			Exec(ctx)
		if err != nil {
			return err
		}

		return rackDAO.Delete(ctx, tx)
	}
	return s.runInTx(ctx, operation)
}

// PurgeRack permanently removes a soft-deleted rack and its components.
func (s *PostgresStore) PurgeRack(ctx context.Context, id uuid.UUID) error {
	operation := func(ctx context.Context, tx bun.Tx) error {
		rackDAO := &model.Rack{ID: id}
		r, err := rackDAO.GetIncludingDeleted(ctx, tx)
		if err != nil {
			return s.checkDBGetError(err, fmt.Sprintf("rack %s", id))
		}
		if r.DeletedAt == nil {
			return errors.GRPCErrorPreconditionFailed(
				fmt.Sprintf("rack %s is not soft-deleted; call DeleteRack first", id))
		}

		// Hard-delete all components (including already soft-deleted ones).
		_, err = tx.NewDelete().Model((*model.Component)(nil)).
			Where("rack_id = ?", id).
			ForceDelete().
			Exec(ctx)
		if err != nil {
			return err
		}

		return rackDAO.ForceDelete(ctx, tx)
	}
	return s.runInTx(ctx, operation)
}

// DeleteComponent soft-deletes a component by UUID.
func (s *PostgresStore) DeleteComponent(ctx context.Context, id uuid.UUID) error {
	compDAO := &model.Component{ID: id}
	return compDAO.Delete(ctx, s.pg.DB)
}

// PurgeComponent permanently removes a soft-deleted component.
func (s *PostgresStore) PurgeComponent(ctx context.Context, id uuid.UUID) error {
	compDAO := &model.Component{ID: id}
	c, err := compDAO.GetIncludingDeleted(ctx, s.pg.DB)
	if err != nil {
		return s.checkDBGetError(err, fmt.Sprintf("component %s", id))
	}
	if c.DeletedAt == nil {
		return errors.GRPCErrorPreconditionFailed(
			fmt.Sprintf("component %s is not soft-deleted; call DeleteComponent first", id))
	}
	return compDAO.ForceDelete(ctx, s.pg.DB)
}

// GetDriftsByComponentIDs retrieves drift records for the given component UUIDs.
func (s *PostgresStore) GetDriftsByComponentIDs(ctx context.Context, componentIDs []uuid.UUID) ([]ComponentDrift, error) {
	drifts, err := model.GetDriftsByComponentIDs(ctx, s.pg.DB, componentIDs)
	if err != nil {
		return nil, err
	}
	return convertDriftsFromModel(drifts), nil
}

// GetAllDrifts retrieves all drift records.
func (s *PostgresStore) GetAllDrifts(ctx context.Context) ([]ComponentDrift, error) {
	drifts, err := model.GetAllDrifts(ctx, s.pg.DB)
	if err != nil {
		return nil, err
	}
	return convertDriftsFromModel(drifts), nil
}

// convertDriftsFromModel converts model-layer drift records to store-layer ones.
func convertDriftsFromModel(drifts []model.ComponentDrift) []ComponentDrift {
	result := make([]ComponentDrift, 0, len(drifts))
	for _, d := range drifts {
		fieldDiffs := make([]FieldDiff, 0, len(d.Diffs))
		for _, fd := range d.Diffs {
			fieldDiffs = append(fieldDiffs, FieldDiff{
				FieldName:     fd.FieldName,
				ExpectedValue: fd.ExpectedValue,
				ActualValue:   fd.ActualValue,
			})
		}
		result = append(result, ComponentDrift{
			ID:          d.ID,
			ComponentID: d.ComponentID,
			ExternalID:  d.ExternalID,
			DriftType:   string(d.DriftType),
			Diffs:       fieldDiffs,
			CheckedAt:   d.CheckedAt,
		})
	}
	return result
}
