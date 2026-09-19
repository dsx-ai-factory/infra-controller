// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

const (
	// SSHKeyRelationName is the relation name for the SSHKey model
	SSHKeyRelationName = "SSHKey"

	// SSHKeyOrderByDefault default field to be used for ordering when none specified
	SSHKeyOrderByDefault = "created"
)

var (
	// SSHKeyOrderByFields is a list of valid order by fields for the Instance model
	SSHKeyOrderByFields = []string{"name", "org", "tenant_id", "created", "updated"}
	// SSHKeyRelatedEntities is a list of valid relation by fields for the SSHKey model
	SSHKeyRelatedEntities = map[string]bool{
		TenantRelationName: true,
	}
)

// SSHKey is a user ssh key
type SSHKey struct {
	bun.BaseModel `bun:"table:ssh_key,alias:sk"`

	ID          uuid.UUID  `bun:"type:uuid,pk"`
	Name        string     `bun:"name,notnull"`
	Org         string     `bun:"org,notnull"`
	TenantID    uuid.UUID  `bun:"tenant_id,type:uuid,notnull"`
	Tenant      *Tenant    `bun:"rel:belongs-to,join:tenant_id=id"`
	PublicKey   string     `bun:"public_key,notnull"`
	Fingerprint *string    `bun:"fingerprint"`
	Expires     *time.Time `bun:"expires"`
	Created     time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated     time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted     *time.Time `bun:"deleted,soft_delete"`
	CreatedBy   uuid.UUID  `bun:"created_by,type:uuid,notnull"`
}

// SSHKeyCreateInput input parameters for Create method
type SSHKeyCreateInput struct {
	SSHKeyID    *uuid.UUID
	Name        string
	TenantOrg   string
	TenantID    uuid.UUID
	PublicKey   string
	Fingerprint *string
	Expires     *time.Time
	CreatedBy   uuid.UUID
}

// SSHKeyUpdateInput input parameters for Update method
type SSHKeyUpdateInput struct {
	SSHKeyID    uuid.UUID
	Name        *string
	TenantOrg   *string
	TenantID    *uuid.UUID
	PublicKey   *string
	Fingerprint *string
	Expires     *time.Time
}

// SSHKeyFilterInput input parameters for Filter method
type SSHKeyFilterInput struct {
	SSHKeyIDs      []uuid.UUID
	SSHKeyGroupIDs []uuid.UUID
	Names          []string
	TenantOrgs     []string
	TenantIDs      []uuid.UUID
	Fingerprints   []string
	Expires        *time.Time
	SearchQuery    *string
}

var _ bun.BeforeAppendModelHook = (*SSHKey)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (sk *SSHKey) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		sk.Created = db.GetCurTime()
		sk.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		sk.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SSHKey)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (a *SSHKey) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)
	return nil
}

// SSHKeyDAO is an interface for interacting with the SSHKey model
type SSHKeyDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SSHKeyCreateInput) (*SSHKey, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SSHKey, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyFilterInput, page paginator.PageInput, includeRelations []string) ([]SSHKey, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SSHKeyUpdateInput) (*SSHKey, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SSHKeySQLDAO is an implementation of the SSHKeyDAO interface
type SSHKeySQLDAO struct {
	dbSession *db.Session
	SSHKeyDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SSHKey from the given parameters
func (sksd SSHKeySQLDAO) Create(ctx context.Context, tx *db.Tx, input SSHKeyCreateInput) (_ *SSHKey, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyDAOSpan := sksd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyDAO.Create")
	if sshKeyDAOSpan != nil {
		defer func() {
			sshKeyDAOSpan.EndWith(retErr)
		}()

		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "name", input.Name)
	}

	id := uuid.New()
	if input.SSHKeyID != nil {
		id = *input.SSHKeyID
	}

	sk := &SSHKey{
		ID:          id,
		Name:        input.Name,
		Org:         input.TenantOrg,
		TenantID:    input.TenantID,
		PublicKey:   input.PublicKey,
		Fingerprint: input.Fingerprint,
		Expires:     input.Expires,
		CreatedBy:   input.CreatedBy,
	}

	_, err := db.GetIDB(tx, sksd.dbSession).NewInsert().Model(sk).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := sksd.GetByID(ctx, tx, sk.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a SSHKey by ID
// returns db.ErrDoesNotExist error if the record is not found
func (sksd SSHKeySQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SSHKey, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyDAOSpan := sksd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyDAO.GetByID")
	if sshKeyDAOSpan != nil {
		defer func() {
			sshKeyDAOSpan.EndWith(retErr)
		}()

		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "id", id.String())
	}

	sk := &SSHKey{}

	query := db.GetIDB(tx, sksd.dbSession).NewSelect().Model(sk).Where("sk.id = ?", id)

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

	return sk, nil
}

// GetAll returns all SSHKeys with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SSHKeyOrderByDefault in ascending order
func (sksd SSHKeySQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyFilterInput, page paginator.PageInput, includeRelations []string) (_ []SSHKey, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyDAOSpan := sksd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyDAO.GetAll")
	if sshKeyDAOSpan != nil {
		defer func() {
			sshKeyDAOSpan.EndWith(retErr)
		}()
	}

	sks := []SSHKey{}

	query := db.GetIDB(tx, sksd.dbSession).NewSelect().Model(&sks)

	if filter.Names != nil {
		query = query.Where("sk.name IN (?)", bun.In(filter.Names))
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "name", filter.Names)
	}
	if filter.TenantOrgs != nil {
		query = query.Where("sk.org IN (?)", bun.In(filter.TenantOrgs))
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "org", filter.TenantOrgs)
	}
	if filter.TenantIDs != nil {
		query = query.Where("sk.tenant_id IN (?)", bun.In(filter.TenantIDs))
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "tenant_id", filter.TenantIDs)
	}
	if filter.SSHKeyIDs != nil {
		query = query.Where("sk.id IN (?)", bun.In(filter.SSHKeyIDs))
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "id", filter.SSHKeyIDs)
	}
	// if sshKeyGroupId is not nil, then we need to join the ssh_key_group_association table and filter by the ssh_key_group_id
	if filter.SSHKeyGroupIDs != nil {
		query = query.Join("JOIN ssh_key_association as ska").
			JoinOn("sk.id = ska.ssh_key_id").
			JoinOn("ska.deleted IS NULL").
			JoinOn("ska.sshkey_group_id IN (?)", bun.In(filter.SSHKeyGroupIDs)).Distinct()
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "ssh_key_group_id", filter.SSHKeyGroupIDs)
	}
	if filter.Fingerprints != nil {
		query = query.Where("sk.fingerprint IN (?)", bun.In(filter.Fingerprints))
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "fingerprint", filter.Fingerprints)
	}
	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', sk.name) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("sk.name ILIKE ?", "%"+searchQuery+"%")
		})
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SSHKeyOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SSHKeyOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return sks, paginator.Total, nil
}

// Update updates specified fields of an existing SSHKey
// The updated fields are assumed to be set to non-null values
func (sksd SSHKeySQLDAO) Update(ctx context.Context, tx *db.Tx, input SSHKeyUpdateInput) (_ *SSHKey, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyDAOSpan := sksd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyDAO.Update")
	if sshKeyDAOSpan != nil {
		defer func() {
			sshKeyDAOSpan.EndWith(retErr)
		}()

		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "id", input.SSHKeyID)
	}

	sk := &SSHKey{
		ID: input.SSHKeyID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		sk.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "name", *input.Name)
	}
	if input.TenantOrg != nil {
		sk.Org = *input.TenantOrg
		updatedFields = append(updatedFields, "org")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "org", *input.TenantOrg)
	}
	if input.TenantID != nil {
		sk.TenantID = *input.TenantID
		updatedFields = append(updatedFields, "tenant_id")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "tenant_id", input.TenantID.String())
	}
	if input.PublicKey != nil {
		sk.PublicKey = *input.PublicKey
		updatedFields = append(updatedFields, "public_key")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "public_key", *input.PublicKey)
	}
	if input.Fingerprint != nil {
		sk.Fingerprint = input.Fingerprint
		updatedFields = append(updatedFields, "fingerprint")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "fingerprint", *input.Fingerprint)
	}
	if input.Expires != nil {
		sk.Expires = input.Expires
		updatedFields = append(updatedFields, "expires")
		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "expires", *input.Expires)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, sksd.dbSession).NewUpdate().Model(sk).Column(updatedFields...).Where("sk.id = ?", input.SSHKeyID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := sksd.GetByID(ctx, tx, sk.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an SSHKey by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (sksd SSHKeySQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyDAOSpan := sksd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyDAO.DeleteByID")
	if sshKeyDAOSpan != nil {
		defer func() {
			sshKeyDAOSpan.EndWith(retErr)
		}()

		sksd.tracerSpan.SetAttribute(sshKeyDAOSpan, "id", id.String())
	}

	it := &SSHKey{
		ID: id,
	}

	_, err := sksd.GetByID(ctx, tx, id, nil)
	if err == nil {
		// clear the publicKey column upon soft-delete
		_, err := sksd.Update(ctx, tx, SSHKeyUpdateInput{SSHKeyID: id, PublicKey: cutil.GetPtr("")})
		if err != nil {
			return err
		}
	}

	_, err = db.GetIDB(tx, sksd.dbSession).NewDelete().Model(it).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSSHKeyDAO returns a new SSHKeyDAO
func NewSSHKeyDAO(dbSession *db.Session) SSHKeyDAO {
	return &SSHKeySQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
