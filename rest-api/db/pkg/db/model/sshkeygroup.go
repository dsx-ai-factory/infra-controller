// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"crypto/sha1"
	"database/sql"
	"encoding/hex"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

const (
	// SSHKeyGroupRelationName is the relation name for the SSHKey model
	SSHKeyGroupRelationName = "SSHKeyGroup"
)

const (
	// SSHKeyGroupStatus status is syncing
	SSHKeyGroupStatusSyncing = "Syncing"
	// SSHKeyGroupStatusSynced status is synced
	SSHKeyGroupStatusSynced = "Synced"
	// SSHKeyGroupStatusError status is error
	SSHKeyGroupStatusError = "Error"
	// SSHKeyGroupStatusDeleting status is deleting
	SSHKeyGroupStatusDeleting = "Deleting"

	// SSHKeyGroupOrderByDefault default field to be used for ordering when none specified
	SSHKeyGroupOrderByDefault = "created"
)

var (
	// SSHKeyGroupMap is a list of valid status for the SSHKeyGroup model
	SSHKeyGroupMap = map[string]bool{
		SSHKeyGroupStatusSyncing:  true,
		SSHKeyGroupStatusSynced:   true,
		SSHKeyGroupStatusError:    true,
		SSHKeyGroupStatusDeleting: true,
	}
)

var (
	// SSHKeyGroupOrderByFields is a list of valid order by fields for the SSHKeyGroup model
	SSHKeyGroupOrderByFields = []string{"name", "status", "created", "updated"}
	// SSHKeyGroupRelatedEntities is a list of valid relation by fields for the SSHKeyGroup model
	SSHKeyGroupRelatedEntities = map[string]bool{
		TenantRelationName: true,
	}
)

// SSHKeyGroup represents a collection of SSH Keys
type SSHKeyGroup struct {
	bun.BaseModel `bun:"table:sshkey_group,alias:skg"`

	ID          uuid.UUID  `bun:"type:uuid,pk"`
	Name        string     `bun:"name,notnull"`
	Description *string    `bun:"description"`
	Org         string     `bun:"org,notnull"`
	TenantID    uuid.UUID  `bun:"tenant_id,type:uuid,notnull"`
	Tenant      *Tenant    `bun:"rel:belongs-to,join:tenant_id=id"`
	Version     *string    `bun:"version"`
	Status      string     `bun:"status,notnull"`
	Created     time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated     time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted     *time.Time `bun:"deleted,soft_delete"`
	CreatedBy   uuid.UUID  `bun:"created_by,type:uuid,notnull"`
}

// SSHKeyGroupCreateInput input parameters for Create method
type SSHKeyGroupCreateInput struct {
	SSHKeyGroupID *uuid.UUID
	Name          string
	Description   *string
	TenantOrg     string
	TenantID      uuid.UUID
	Version       *string
	Status        string
	CreatedBy     uuid.UUID
}

// SSHKeyGroupUpdateInput input parameters for Update method
type SSHKeyGroupUpdateInput struct {
	SSHKeyGroupID uuid.UUID
	Name          *string
	Description   *string
	TenantOrg     *string
	TenantID      *uuid.UUID
	Version       *string
	Status        *string
}

// SSHKeyGroupilterInput input parameters for Filter method
type SSHKeyGroupFilterInput struct {
	SSHKeyGroupIDs []uuid.UUID
	Names          []string
	TenantOrgs     []string
	TenantIDs      []uuid.UUID
	Versions       []string
	Statuses       []string
	SearchQuery    *string
}

var _ bun.BeforeAppendModelHook = (*SSHKeyGroup)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (skg *SSHKeyGroup) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		skg.Created = db.GetCurTime()
		skg.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		skg.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SSHKeyGroup)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (a *SSHKeyGroup) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)
	return nil
}

// SSHKeyGroupDAO is an interface for interacting with the SSHKeyGroup model
type SSHKeyGroupDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SSHKeyGroupCreateInput) (*SSHKeyGroup, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SSHKeyGroup, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupFilterInput, page paginator.PageInput, includeRelations []string) ([]SSHKeyGroup, int, error)
	//
	GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, id uuid.UUID) (*SSHKeyGroup, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SSHKeyGroupUpdateInput) (*SSHKeyGroup, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SSHKeyGroupSQLDAO is an implementation of the SSHKeyGroupDAO interface
type SSHKeyGroupSQLDAO struct {
	dbSession *db.Session
	SSHKeyGroupDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SSHKeyGroup from the given parameters
func (skgsd SSHKeyGroupSQLDAO) Create(ctx context.Context, tx *db.Tx, input SSHKeyGroupCreateInput) (_ *SSHKeyGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupDAOSpan := skgsd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupDAO.Create")
	if SSHKeyGroupDAOSpan != nil {
		defer func() {
			SSHKeyGroupDAOSpan.EndWith(retErr)
		}()

		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "name", input.Name)
	}

	id := uuid.New()
	if input.SSHKeyGroupID != nil {
		id = *input.SSHKeyGroupID
	}

	skg := &SSHKeyGroup{
		ID:          id,
		Name:        input.Name,
		Description: input.Description,
		Org:         input.TenantOrg,
		TenantID:    input.TenantID,
		Version:     input.Version,
		Status:      input.Status,
		CreatedBy:   input.CreatedBy,
	}

	_, err := db.GetIDB(tx, skgsd.dbSession).NewInsert().Model(skg).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := skgsd.GetByID(ctx, tx, skg.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a SSHKeyGroup by ID
// returns db.ErrDoesNotExist error if the record is not found
func (skgsd SSHKeyGroupSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SSHKeyGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupDAOSpan := skgsd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupDAO.GetByID")
	if SSHKeyGroupDAOSpan != nil {
		defer func() {
			SSHKeyGroupDAOSpan.EndWith(retErr)
		}()

		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "id", id.String())
	}

	skg := &SSHKeyGroup{}

	query := db.GetIDB(tx, skgsd.dbSession).NewSelect().Model(skg).Where("skg.id = ?", id)

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

	return skg, nil
}

// GetAll returns all SSHKeyGroups with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SSHKeyGroupOrderByDefault in ascending order
func (skgsd SSHKeyGroupSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupFilterInput, page paginator.PageInput, includeRelations []string) (_ []SSHKeyGroup, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupDAOSpan := skgsd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupDAO.GetAll")
	if SSHKeyGroupDAOSpan != nil {
		defer func() {
			SSHKeyGroupDAOSpan.EndWith(retErr)
		}()
	}

	skgs := []SSHKeyGroup{}

	query := db.GetIDB(tx, skgsd.dbSession).NewSelect().Model(&skgs)

	if filter.Names != nil {
		query = query.Where("skg.name IN (?)", bun.In(filter.Names))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "name", filter.Names)
	}
	if filter.TenantOrgs != nil {
		query = query.Where("skg.org IN (?)", bun.In(filter.TenantOrgs))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "org", filter.TenantOrgs)
	}
	if filter.TenantIDs != nil {
		query = query.Where("skg.tenant_id IN (?)", bun.In(filter.TenantIDs))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "tenant_id", filter.TenantIDs)
	}
	if filter.SSHKeyGroupIDs != nil {
		query = query.Where("skg.id IN (?)", bun.In(filter.SSHKeyGroupIDs))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "id", filter.SSHKeyGroupIDs)
	}
	if filter.Versions != nil {
		query = query.Where("skg.version IN (?)", bun.In(filter.Versions))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "version", filter.Versions)
	}
	if filter.Statuses != nil {
		query = query.Where("skg.status IN (?)", bun.In(filter.Statuses))
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "status", filter.Statuses)
	}
	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', skg.name) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("skg.name ILIKE ?", "%"+searchQuery+"%")
		})
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SSHKeyGroupOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SSHKeyGroupOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return skgs, paginator.Total, nil
}

// GenerateAndUpdateVersion generates version based on current content and updates the version field
func (skgsd SSHKeyGroupSQLDAO) GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, id uuid.UUID) (*SSHKeyGroup, error) {
	// Generate SHA1 hash version using SSHKeyGroup ID
	// Retrieve SSH Key Group
	dbskg, err := skgsd.GetByID(ctx, tx, id, nil)
	if err != nil {
		return nil, err
	}

	// Initialize hash with SSH Key Group
	skgHash := sha1.New()
	skgHash.Write([]byte(dbskg.ID.String()))

	// Retrieve SSH Key Group Association details for calculating hash version based on SSH Key Group ID
	skgsaDAO := NewSSHKeyGroupSiteAssociationDAO(skgsd.dbSession)
	dbskgsas, _, err := skgsaDAO.GetAll(ctx, tx, SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{id}}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit),
		OrderBy: &paginator.OrderBy{
			Field: "created",
			Order: paginator.OrderAscending,
		},
	}, nil)
	if err != nil {
		return nil, err
	}

	// Update hash based on SiteID
	for _, skgsa := range dbskgsas {
		// skipping the association if it is in deleting status
		if skgsa.Status != SSHKeyGroupSiteAssociationStatusDeleting {
			skgHash.Write([]byte(skgsa.SiteID.String()))

			// Continue updating hash if in future we added support for VPC/Instance
		}
	}

	// Retrieve SSH Key Association details for calculating hash version based on SSH Key ID
	skaDAO := NewSSHKeyAssociationDAO(skgsd.dbSession)
	dbska, _, err := skaDAO.GetAll(ctx, tx, SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{id}}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit),
		OrderBy: &paginator.OrderBy{
			Field: "created",
			Order: paginator.OrderAscending,
		},
	}, nil)
	if err != nil {
		return nil, err
	}

	// Initialize hash with SSH Key Group Associations
	skgsaHash := sha1.New()
	skgHash.Write([]byte(dbskg.ID.String()))

	// Update hash based on SSH Key ID
	for _, ska := range dbska {
		skgHash.Write([]byte(ska.SSHKeyID.String()))
		skgsaHash.Write([]byte(ska.SSHKeyID.String()))
	}

	// Update version for SSH Key Group
	skgVersion := hex.EncodeToString(skgHash.Sum(nil))
	uskgsa, err := skgsd.Update(ctx, tx, SSHKeyGroupUpdateInput{SSHKeyGroupID: id, Version: &skgVersion})
	if err != nil {
		return nil, err
	}

	// Update version for all SSH Key Group Associations
	skgsaVersion := hex.EncodeToString(skgsaHash.Sum(nil))
	curTime := db.GetCurTime()
	_, err = db.GetIDB(tx, skgsd.dbSession).NewUpdate().Model((*SSHKeyGroupSiteAssociation)(nil)).Column("version", "updated").Set("version = ?", skgsaVersion).Set("updated = ?", curTime).Where("sshkey_group_id = ?", id).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return uskgsa, nil
}

// Update updates specified fields of an existing SSHKeyGroup
// The updated fields are assumed to be set to non-null values
func (skgsd SSHKeyGroupSQLDAO) Update(ctx context.Context, tx *db.Tx, input SSHKeyGroupUpdateInput) (_ *SSHKeyGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupDAOSpan := skgsd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupDAO.Update")
	if SSHKeyGroupDAOSpan != nil {
		defer func() {
			SSHKeyGroupDAOSpan.EndWith(retErr)
		}()

		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "id", input.SSHKeyGroupID.String())
	}

	skg := &SSHKeyGroup{
		ID: input.SSHKeyGroupID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		skg.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "name", *input.Name)
	}
	if input.Description != nil {
		skg.Description = input.Description
		updatedFields = append(updatedFields, "description")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "description", *input.Description)
	}
	if input.TenantOrg != nil {
		skg.Org = *input.TenantOrg
		updatedFields = append(updatedFields, "org")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "org", *input.TenantOrg)
	}
	if input.TenantID != nil {
		skg.TenantID = *input.TenantID
		updatedFields = append(updatedFields, "tenant_id")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "tenant_id", input.TenantID.String())
	}
	if input.Version != nil {
		skg.Version = input.Version
		updatedFields = append(updatedFields, "version")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "version", *input.Version)
	}
	if input.Status != nil {
		skg.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "status", *input.Status)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, skgsd.dbSession).NewUpdate().Model(skg).Column(updatedFields...).Where("skg.id = ?", skg.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := skgsd.GetByID(ctx, tx, skg.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an SSHKeyGroup by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (skgsd SSHKeyGroupSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupDAOSpan := skgsd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSQLDAO.Delete")
	if SSHKeyGroupDAOSpan != nil {
		defer func() {
			SSHKeyGroupDAOSpan.EndWith(retErr)
		}()

		skgsd.tracerSpan.SetAttribute(SSHKeyGroupDAOSpan, "id", id.String())
	}
	skg := &SSHKeyGroup{
		ID: id,
	}

	_, err := db.GetIDB(tx, skgsd.dbSession).NewDelete().Model(skg).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSSHKeyGroupDAO returns a new SSHKeyGroupDAO
func NewSSHKeyGroupDAO(dbSession *db.Session) SSHKeyGroupDAO {
	return &SSHKeyGroupSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
