// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"

	"github.com/uptrace/bun"
)

const (
	// InfrastructureProviderRelationName is the relation name for the InfrastructureProvider model
	InfrastructureProviderRelationName = "InfrastructureProvider"
)

// InfrastructureProvider is the object for the infrastructure_provider table
type InfrastructureProvider struct {
	bun.BaseModel `bun:"table:infrastructure_provider,alias:ip"`

	ID             uuid.UUID  `bun:"type:uuid,pk"`
	Name           string     `bun:"name,notnull"`
	DisplayName    *string    `bun:"display_name"`
	Org            string     `bun:"org,notnull"`
	OrgDisplayName *string    `bun:"org_display_name"`
	Created        time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated        time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted        *time.Time `bun:"deleted,soft_delete"`
	CreatedBy      uuid.UUID  `bun:"type:uuid,notnull"`
}

// InfrastructureProviderCreateInput input parameters for Create method
type InfrastructureProviderCreateInput struct {
	Name           string
	DisplayName    *string
	Org            string
	OrgDisplayName *string
	CreatedBy      uuid.UUID
}

// InfrastructureProviderUpdateInput input parameters for Update method
type InfrastructureProviderUpdateInput struct {
	InfrastructureProviderID uuid.UUID
	Name                     *string
	DisplayName              *string
	OrgDisplayName           *string
}

var _ bun.BeforeAppendModelHook = (*InfrastructureProvider)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ip *InfrastructureProvider) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ip.Created = db.GetCurTime()
		ip.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ip.Updated = db.GetCurTime()
	}
	return nil
}

// InfrastructureProviderDAO is the data access interface for InfrastructureProvider
type InfrastructureProviderDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*InfrastructureProvider, error)
	//
	GetAllByOrg(ctx context.Context, tx *db.Tx, org string, includeRelations []string) ([]InfrastructureProvider, error)
	//
	Create(ctx context.Context, tx *db.Tx, input InfrastructureProviderCreateInput) (*InfrastructureProvider, error)
	//
	Update(ctx context.Context, tx *db.Tx, input InfrastructureProviderUpdateInput) (*InfrastructureProvider, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// InfrastructureProviderSQLDAO implements InfrastructureProviderDAO interface for SQL
type InfrastructureProviderSQLDAO struct {
	dbSession *db.Session
	InfrastructureProviderDAO
	tracerSpan *stracer.TracerSpan
}

// GetByID returns the InfrastructureProvider with the given ID
func (ipsd InfrastructureProviderSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *InfrastructureProvider, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ipDAOSpan := ipsd.tracerSpan.CreateChildInCurrentContext(ctx, "InfrastructureProviderDAO.GetByID")
	if ipDAOSpan != nil {
		defer func() {
			ipDAOSpan.EndWith(retErr)
		}()

		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "id", id.String())
	}

	ip := &InfrastructureProvider{}

	query := db.GetIDB(tx, ipsd.dbSession).NewSelect().Model(ip).Where("id = ?", id)

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

	return ip, nil
}

// GetAllByOrg returns the InfrastructureProviders with the given org
func (ipsd InfrastructureProviderSQLDAO) GetAllByOrg(ctx context.Context, tx *db.Tx, org string, includeRelations []string) (_ []InfrastructureProvider, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ipDAOSpan := ipsd.tracerSpan.CreateChildInCurrentContext(ctx, "InfrastructureProviderDAO.GetAllByOrg")
	if ipDAOSpan != nil {
		defer func() {
			ipDAOSpan.EndWith(retErr)
		}()
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "org", org)
	}

	var ips []InfrastructureProvider

	query := db.GetIDB(tx, ipsd.dbSession).NewSelect().Model(&ips).Where("ip.org = ?", org)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)

	if err != nil {
		return nil, err
	}

	return ips, nil
}

// Create creates a new InfrastructureProvider from the given parameters
func (ipsd InfrastructureProviderSQLDAO) Create(ctx context.Context, tx *db.Tx, input InfrastructureProviderCreateInput) (_ *InfrastructureProvider, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ipDAOSpan := ipsd.tracerSpan.CreateChildInCurrentContext(ctx, "InfrastructureProviderSQLDAO.Create")
	if ipDAOSpan != nil {
		defer func() {
			ipDAOSpan.EndWith(retErr)
		}()
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "name", input.Name)
	}

	ip := &InfrastructureProvider{
		ID:             uuid.New(),
		Name:           input.Name,
		DisplayName:    input.DisplayName,
		Org:            input.Org,
		OrgDisplayName: input.OrgDisplayName,
		CreatedBy:      input.CreatedBy,
	}

	_, err := db.GetIDB(tx, ipsd.dbSession).NewInsert().Model(ip).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nip, err := ipsd.GetByID(ctx, tx, ip.ID, nil)
	if err != nil {
		return nil, err
	}

	return nip, nil
}

// Update updates the InfrastructureProvider with the given parameters
func (ipsd InfrastructureProviderSQLDAO) Update(ctx context.Context, tx *db.Tx, input InfrastructureProviderUpdateInput) (_ *InfrastructureProvider, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ipDAOSpan := ipsd.tracerSpan.CreateChildInCurrentContext(ctx, "InfrastructureProviderSQLDAO.Update")
	if ipDAOSpan != nil {
		defer func() {
			ipDAOSpan.EndWith(retErr)
		}()
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "id", input.InfrastructureProviderID.String())
	}

	ip := &InfrastructureProvider{
		ID: input.InfrastructureProviderID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		ip.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "name", *input.Name)
	}

	if input.DisplayName != nil {
		ip.DisplayName = input.DisplayName
		updatedFields = append(updatedFields, "display_name")
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "display_name", *input.DisplayName)
	}

	if input.OrgDisplayName != nil {
		ip.OrgDisplayName = input.OrgDisplayName
		updatedFields = append(updatedFields, "org_display_name")
		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "org_display_name", *input.OrgDisplayName)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ipsd.dbSession).NewUpdate().Model(ip).Where("id = ?", input.InfrastructureProviderID).Column(updatedFields...).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	uip, err := ipsd.GetByID(ctx, tx, ip.ID, nil)
	if err != nil {
		return nil, err
	}

	return uip, nil
}

// Delete deletes the InfrastructureProvider with the given ID
func (ipsd InfrastructureProviderSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, ipDAOSpan := ipsd.tracerSpan.CreateChildInCurrentContext(ctx, "InfrastructureProviderSQLDAO.Delete")
	if ipDAOSpan != nil {
		defer func() {
			ipDAOSpan.EndWith(retErr)
		}()

		ipsd.tracerSpan.SetAttribute(ipDAOSpan, "id", id.String())
	}

	_, err := db.GetIDB(tx, ipsd.dbSession).NewDelete().Model((*InfrastructureProvider)(nil)).Where("id = ?", id).Exec(ctx)

	if err != nil {
		return err
	}

	return nil
}

// NewInfrastructureProviderDAO creates and returns a new data access object for InfrastructureProvider
func NewInfrastructureProviderDAO(dbSession *db.Session) InfrastructureProviderDAO {
	return InfrastructureProviderSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
