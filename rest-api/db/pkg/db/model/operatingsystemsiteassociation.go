// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"crypto/sha1"
	"database/sql"
	"encoding/hex"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	// OperatingSystemSiteAssociationOrderByFields is a list of valid order by fields for the OperatingSystemSiteAssociation model
	OperatingSystemSiteAssociationOrderByFields = []string{"status", "created", "updated"}

	// OperatingSystemSiteAssociationRelatedEntities is a list of valid relation by fields for the OperatingSystemSiteAssociation model
	OperatingSystemSiteAssociationRelatedEntities = map[string]bool{
		OperatingSystemRelationName: true,
	}

	// OperatingSystemSiteAssociationEntityTypes is a list of valid choices for the EntityType field
	OperatingSystemSiteAssociationEntityTypes = map[string]bool{
		SiteRelationName:            true,
		OperatingSystemRelationName: true,
	}
)

const (
	// OperatingSystemSiteAssociationStatusSyncing status is syncing
	OperatingSystemSiteAssociationStatusSyncing = "Syncing"
	// OperatingSystemSiteAssociationStatusSynced status is synced
	OperatingSystemSiteAssociationStatusSynced = "Synced"
	// OperatingSystemSiteAssociationStatusError status is error
	OperatingSystemSiteAssociationStatusError = "Error"
	// OperatingSystemSiteAssociationStatusDeleting status is deleting
	OperatingSystemSiteAssociationStatusDeleting = "Deleting"

	// OperatingSystemSiteAssociationOrderByDefault default field to be used for ordering when none specified
	OperatingSystemSiteAssociationOrderByDefault = "created"
)

var (
	// OperatingSystemSiteAssociationStatusSyncingMap is a list of valid status for the OperatingSystemSiteAssociation model
	OperatingSystemSiteAssociationStatusSyncingMap = map[string]bool{
		OperatingSystemSiteAssociationStatusSyncing:  true,
		OperatingSystemSiteAssociationStatusSynced:   true,
		OperatingSystemSiteAssociationStatusError:    true,
		OperatingSystemSiteAssociationStatusDeleting: true,
	}

	// OperatingSystemSiteAssociationStatusFromProtoMap maps nico-core tenant states to per-site association status values.
	OperatingSystemSiteAssociationStatusFromProtoMap = map[corev1.TenantState]string{
		corev1.TenantState_PROVISIONING: OperatingSystemSiteAssociationStatusSyncing,
		corev1.TenantState_READY:        OperatingSystemSiteAssociationStatusSynced,
		corev1.TenantState_CONFIGURING:  OperatingSystemSiteAssociationStatusSyncing,
		corev1.TenantState_TERMINATING:  OperatingSystemSiteAssociationStatusDeleting,
		corev1.TenantState_FAILED:       OperatingSystemSiteAssociationStatusError,
	}
)

// OperatingSystemSiteAssociation associates an OperatingSystem with different Sites
type OperatingSystemSiteAssociation struct {
	bun.BaseModel `bun:"table:operating_system_site_association,alias:ossa"`

	ID                uuid.UUID        `bun:"type:uuid,pk"`
	OperatingSystemID uuid.UUID        `bun:"operating_system_id,type:uuid,notnull"`
	OperatingSystem   *OperatingSystem `bun:"rel:belongs-to,join:operating_system_id=id"`
	SiteID            uuid.UUID        `bun:"site_id,type:uuid,notnull"`
	Site              *Site            `bun:"rel:belongs-to,join:site_id=id"`
	Version           *string          `bun:"version"`
	Status            string           `bun:"status,notnull"`
	// ControllerState mirrors the tenant state reported by nico-core for this OS at this site.
	ControllerState *string    `bun:"controller_state"`
	IsMissingOnSite bool       `bun:"is_missing_on_site,notnull"`
	Created         time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID  `bun:"created_by,type:uuid,notnull"`
}

// OperatingSystemSiteAssociationCreateInput input parameters for Create method
type OperatingSystemSiteAssociationCreateInput struct {
	OperatingSystemID uuid.UUID
	SiteID            uuid.UUID
	Version           *string
	Status            string
	ControllerState   *string
	CreatedBy         uuid.UUID
}

// OperatingSystemSiteAssociationUpdateInput input parameters for Update method
type OperatingSystemSiteAssociationUpdateInput struct {
	OperatingSystemSiteAssociationID uuid.UUID
	OperatingSystemID                *uuid.UUID
	SiteID                           *uuid.UUID
	Version                          *string
	Status                           *string
	ControllerState                  *string
	IsMissingOnSite                  *bool
}

type OperatingSystemSiteAssociationFilterInput struct {
	OperatingSystemIDs []uuid.UUID
	SiteIDs            []uuid.UUID
	Versions           []string
	Statuses           []string
}

var _ bun.BeforeAppendModelHook = (*OperatingSystemSiteAssociation)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ossa *OperatingSystemSiteAssociation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ossa.Created = db.GetCurTime()
		ossa.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ossa.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*OperatingSystemSiteAssociation)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ossa *OperatingSystemSiteAssociation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("operating_system_id") REFERENCES "operating_system" ("id")`)
	return nil
}

// OperatingSystemSiteAssociationDAO is an interface for interacting with the OperatingSystemSiteAssociation model
type OperatingSystemSiteAssociationDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input OperatingSystemSiteAssociationCreateInput) (*OperatingSystemSiteAssociation, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*OperatingSystemSiteAssociation, error)
	//
	GetByOperatingSystemIDAndSiteID(ctx context.Context, tx *db.Tx, OperatingSystemID uuid.UUID, siteID uuid.UUID, includeRelations []string) (*OperatingSystemSiteAssociation, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter OperatingSystemSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) ([]OperatingSystemSiteAssociation, int, error)
	//
	GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, ID uuid.UUID) (*OperatingSystemSiteAssociation, error)
	//
	Update(ctx context.Context, tx *db.Tx, input OperatingSystemSiteAssociationUpdateInput) (*OperatingSystemSiteAssociation, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// OperatingSystemSiteAssociationSQLDAO is an implementation of the OperatingSystemSiteAssociationDAO interface
type OperatingSystemSiteAssociationSQLDAO struct {
	dbSession *db.Session
	OperatingSystemSiteAssociationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new OperatingSystemSiteAssociation from the given parameters
func (ossasd OperatingSystemSiteAssociationSQLDAO) Create(
	ctx context.Context, tx *db.Tx,
	input OperatingSystemSiteAssociationCreateInput,
) (_ *OperatingSystemSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.Create")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()
	}

	ossa := &OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: input.OperatingSystemID,
		SiteID:            input.SiteID,
		Version:           input.Version,
		Status:            input.Status,
		ControllerState:   input.ControllerState,
		CreatedBy:         input.CreatedBy,
	}

	_, err := db.GetIDB(tx, ossasd.dbSession).NewInsert().Model(ossa).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := ossasd.GetByID(ctx, tx, ossa.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a OperatingSystemSiteAssociation by ID
// returns db.ErrDoesNotExist error if the record is not found
func (ossasd OperatingSystemSiteAssociationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *OperatingSystemSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.GetByID")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()

		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "id", id.String())
	}

	ossa := &OperatingSystemSiteAssociation{}

	query := db.GetIDB(tx, ossasd.dbSession).NewSelect().Model(ossa).Where("ossa.id = ?", id)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return ossa, nil
}

// GetByOperatingSystemIDAndSiteID returns an OperatingSystemSiteAssociation by OperatingSystemID and SiteID
// returns db.ErrDoesNotExist error if the record is not found
func (ossasd OperatingSystemSiteAssociationSQLDAO) GetByOperatingSystemIDAndSiteID(ctx context.Context, tx *db.Tx, OperatingSystemID uuid.UUID, siteID uuid.UUID, includeRelations []string) (_ *OperatingSystemSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.GetByOperatingSystemIDAndSiteID")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()

		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "operating_system_id", OperatingSystemID.String())
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "site_id", siteID.String())
	}

	ossa := &OperatingSystemSiteAssociation{}

	query := db.GetIDB(tx, ossasd.dbSession).NewSelect().Model(ossa).Where("ossa.operating_system_id = ?", OperatingSystemID.String()).Where("ossa.site_id = ?", siteID.String())

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return ossa, nil
}

// GetAll returns all OperatingSystemSiteAssociation with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in OperatingSystemSiteAssociationOrderByDefault in ascending order
func (ossasd OperatingSystemSiteAssociationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter OperatingSystemSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) (_ []OperatingSystemSiteAssociation, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.GetAll")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()
	}

	ossas := []OperatingSystemSiteAssociation{}

	query := db.GetIDB(tx, ossasd.dbSession).NewSelect().Model(&ossas)
	if filter.OperatingSystemIDs != nil {
		query = query.Where("ossa.operating_system_id IN (?)", bun.In(filter.OperatingSystemIDs))
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "operating_system_id", filter.OperatingSystemIDs)
	}
	if filter.SiteIDs != nil {
		query = query.Where("ossa.site_id IN (?)", bun.In(filter.SiteIDs))
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.Versions != nil {
		query = query.Where("ossa.version IN (?)", bun.In(filter.Versions))
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "version", filter.Versions)
	}
	if filter.Statuses != nil {
		query = query.Where("ossa.status IN (?)", bun.In(filter.Statuses))
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "status", filter.Statuses)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(OperatingSystemSiteAssociationOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, OperatingSystemSiteAssociationOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return ossas, paginator.Total, nil
}

// GenerateAndUpdateVersion is a utility function to generate latest version and update the OperatingSystemSiteAssociation
func (ossasd OperatingSystemSiteAssociationSQLDAO) GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, id uuid.UUID) (*OperatingSystemSiteAssociation, error) {
	// Retrieve Operating Systemp Association details for calculating hash version based on OperatingSystemSiteAssociation ID
	dbossa, err := ossasd.GetByID(ctx, tx, id, []string{OperatingSystemRelationName})
	if err != nil {
		return nil, err
	}

	// Initial has contains OperatingSystemID
	hash := sha1.New()
	hash.Write([]byte(dbossa.OperatingSystemID.String()))

	// Update hash based on Operating System parameter (Image based OS)
	if dbossa.OperatingSystem != nil {
		if dbossa.OperatingSystem.ImageURL != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.ImageURL))
		}
		if dbossa.OperatingSystem.ImageSHA != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.ImageSHA))
		}
		if dbossa.OperatingSystem.ImageAuthType != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.ImageAuthType))
		}
		if dbossa.OperatingSystem.ImageAuthToken != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.ImageAuthToken))
		}
		if dbossa.OperatingSystem.ImageDisk != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.ImageDisk))
		}
		if dbossa.OperatingSystem.RootFsID != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.RootFsID))
		}
		if dbossa.OperatingSystem.RootFsLabel != nil {
			hash.Write([]byte(*dbossa.OperatingSystem.RootFsLabel))
		}

		var isBlockStorageEnable byte
		if dbossa.OperatingSystem.EnableBlockStorage {
			isBlockStorageEnable = 1
			hash.Write([]byte{isBlockStorageEnable})
		} else {
			isBlockStorageEnable = 0
			hash.Write([]byte{isBlockStorageEnable})
		}
	}

	version := hex.EncodeToString(hash.Sum(nil))

	// Update OperatingSystemSiteAssociation with new version
	uossa, err := ossasd.Update(ctx, tx, OperatingSystemSiteAssociationUpdateInput{
		OperatingSystemSiteAssociationID: id,
		Version:                          &version,
	})
	if err != nil {
		return nil, err
	}

	return uossa, nil
}

// Update updates specified fields of an existing OperatingSystemSiteAssociation
func (ossasd OperatingSystemSiteAssociationSQLDAO) Update(
	ctx context.Context, tx *db.Tx, input OperatingSystemSiteAssociationUpdateInput,
) (_ *OperatingSystemSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.Update")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "id", input.OperatingSystemSiteAssociationID.String())
	}

	ossa := &OperatingSystemSiteAssociation{
		ID: input.OperatingSystemSiteAssociationID,
	}

	updatedFields := []string{}

	if input.OperatingSystemID != nil {
		ossa.OperatingSystemID = *input.OperatingSystemID
		updatedFields = append(updatedFields, "operating_system_id")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "operating_system_id", input.OperatingSystemID.String())
	}
	if input.SiteID != nil {
		ossa.SiteID = *input.SiteID
		updatedFields = append(updatedFields, "site_id")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "site_id", input.SiteID.String())
	}
	if input.Version != nil {
		ossa.Version = input.Version
		updatedFields = append(updatedFields, "version")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "version", *input.Version)
	}
	if input.Status != nil {
		ossa.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		ossa.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}
	if input.ControllerState != nil {
		ossa.ControllerState = input.ControllerState
		updatedFields = append(updatedFields, "controller_state")
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "controller_state", *input.ControllerState)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ossasd.dbSession).NewUpdate().Model(ossa).Column(updatedFields...).Where("ossa.id = ?", input.OperatingSystemSiteAssociationID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := ossasd.GetByID(ctx, tx, ossa.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an OperatingSystemSiteAssociation by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (ossasd OperatingSystemSiteAssociationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, OperatingSystemSiteAssociationDAOSpan := ossasd.tracerSpan.CreateChildInCurrentContext(ctx, "OperatingSystemSiteAssociationDAO.Delete")
	if OperatingSystemSiteAssociationDAOSpan != nil {
		defer func() {
			OperatingSystemSiteAssociationDAOSpan.EndWith(retErr)
		}()
		ossasd.tracerSpan.SetAttribute(OperatingSystemSiteAssociationDAOSpan, "id", id.String())
	}

	ossa := &OperatingSystemSiteAssociation{
		ID: id,
	}

	_, err := db.GetIDB(tx, ossasd.dbSession).NewDelete().Model(ossa).Where("ossa.id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewOperatingSystemSiteAssociationDAO returns a new OperatingSystemSiteAssociationDAO
func NewOperatingSystemSiteAssociationDAO(dbSession *db.Session) OperatingSystemSiteAssociationDAO {
	return &OperatingSystemSiteAssociationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
