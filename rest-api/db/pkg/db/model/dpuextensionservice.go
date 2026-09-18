// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// DpuExtensionServiceTimeFormat is the time format used on Site for version info creation time
	DpuExtensionServiceTimeFormat = "2006-01-02 15:04:05.000000 UTC"

	// FabricRelationName is the relation name for the Fabric model
	DpuExtensionServiceRelationName = "DpuExtensionService"
)

const (
	// DpuExtensionServiceStatusPending indicates that the DpuExtensionService request was received but not yet processed
	DpuExtensionServiceStatusPending = "Pending"
	// DpuExtensionServiceStatusReady indicates that the DpuExtensionService is ready on the Site
	DpuExtensionServiceStatusReady = "Ready"
	// DpuExtensionServiceStatusError is the status of a DpuExtensionService that is in error mode
	DpuExtensionServiceStatusError = "Error"
	// DpuExtensionServiceStatusDeleting indicates that the DpuExtensionService is being deleted
	DpuExtensionServiceStatusDeleting = "Deleting"

	// DpuExtensionServiceOrderByDefault default field to be used for ordering when none specified
	DpuExtensionServiceOrderByDefault = "created"
)

var (
	// DpuExtensionServiceStatusMap is a list of valid status for the DpuExtensionService model
	DpuExtensionServiceStatusMap = map[string]bool{
		DpuExtensionServiceStatusPending:  true,
		DpuExtensionServiceStatusReady:    true,
		DpuExtensionServiceStatusError:    true,
		DpuExtensionServiceStatusDeleting: true,
	}

	// DpuExtensionServiceOrderByFields is a list of valid order by fields for the DpuExtensionService model
	DpuExtensionServiceOrderByFields = []string{"id", "name", "status", "created", "updated"}
	// DpuExtensionServiceRelatedEntities is a list of valid relation by fields for the DpuExtensionService model
	DpuExtensionServiceRelatedEntities = map[string]bool{
		SiteRelationName:     true,
		TenantRelationName:   true,
		InstanceRelationName: true,
	}

	// DpuExtensionServiceServiceTypeKubernetesPod indicates an extension service running as a Kubernetes pod
	DpuExtensionServiceServiceTypeKubernetesPod = "KubernetesPod"

	// DpuExtensionServiceServiceTypeMap is a map of valid service types for the DpuExtensionService model
	DpuExtensionServiceServiceTypeMap = map[string]bool{
		DpuExtensionServiceServiceTypeKubernetesPod: true,
	}
)

// DpuExtensionServiceVersionInfo is a data structure to capture information for a specific DPU Extension Service version
type DpuExtensionServiceVersionInfo struct {
	Version        string                            `json:"version"`
	Data           string                            `json:"data"`
	HasCredentials bool                              `json:"has_credentials"`
	Created        time.Time                         `json:"created"`
	Observability  *DpuExtensionServiceObservability `json:"observability,omitempty"`
}

// A light wrapper around the protobuf so
// that we can implement our own marshal/unmarshal
// that understands how to work with protobuf messages
type DpuExtensionServiceObservability struct {
	*corev1.DpuExtensionServiceObservability
}

func (o *DpuExtensionServiceObservability) UnmarshalJSON(b []byte) error {
	if o.DpuExtensionServiceObservability == nil {
		o.DpuExtensionServiceObservability = &corev1.DpuExtensionServiceObservability{}
	}

	// protoJsonUnmarshalOptions is set to ignore unknown fields.
	// This means a record created on site that uses a new feature
	// won't break things in cloud,
	// BUT it also means that a user could
	// create something on site and then update it in cloud without realizing
	// that the new property for the new feature isn't in the cloud data.
	// If they then save the change, the record on site would lose the detail.
	// NOTE: Similar to what we do in other places (e.g., NSGs), we aren't
	// ignoring the error here because the wrapper is on the entire observability
	// object and not individual configs.
	return protoJsonUnmarshalOptions.Unmarshal(b, o)
}

func (o *DpuExtensionServiceObservability) MarshalJSON() ([]byte, error) {
	return protojson.Marshal(o)
}

// FromProto populates version info from the site-agent protobuf form.
func (vi *DpuExtensionServiceVersionInfo) FromProto(protoVersionInfo *corev1.DpuExtensionServiceVersionInfo, fallbackTime time.Time) {
	if vi == nil || protoVersionInfo == nil {
		return
	}

	created, err := time.Parse(DpuExtensionServiceTimeFormat, protoVersionInfo.Created)
	if err != nil {
		// NOTE: This is not accurate but without a timestamp, this is the best approximation.
		created = fallbackTime
	}

	var observability *DpuExtensionServiceObservability
	if protoVersionInfo.Observability != nil {
		observability = &DpuExtensionServiceObservability{
			DpuExtensionServiceObservability: protoVersionInfo.Observability,
		}
	}

	*vi = DpuExtensionServiceVersionInfo{
		Version:        protoVersionInfo.Version,
		Data:           protoVersionInfo.Data,
		HasCredentials: protoVersionInfo.HasCredential,
		Created:        created,
		Observability:  observability,
	}
}

// DpuExtensionService represents a DPU extension service
type DpuExtensionService struct {
	bun.BaseModel `bun:"table:dpu_extension_service,alias:des"`

	ID              uuid.UUID                       `bun:"id,type:uuid,unique,pk"`
	Name            string                          `bun:"name,notnull"`
	Description     *string                         `bun:"description"`
	ServiceType     string                          `bun:"service_type,notnull"`
	SiteID          uuid.UUID                       `bun:"site_id,type:uuid,notnull,pk"`
	Site            *Site                           `bun:"rel:belongs-to,join:site_id=id"`
	TenantID        uuid.UUID                       `bun:"tenant_id,type:uuid,notnull"`
	Tenant          *Tenant                         `bun:"rel:belongs-to,join:tenant_id=id"`
	Version         *string                         `bun:"version"`
	VersionInfo     *DpuExtensionServiceVersionInfo `bun:"version_info,type:jsonb"`
	ActiveVersions  []string                        `bun:"active_versions,type:text[],default:'{}'"`
	Status          string                          `bun:"status,notnull"`
	IsMissingOnSite bool                            `bun:"is_missing_on_site,notnull,default:false"`
	Created         time.Time                       `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time                       `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time                      `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID                       `bun:"created_by,type:uuid,notnull"`
}

// ToDeletionRequestProto builds the workflow request that asks a Site to
// delete this DPU Extension Service.
func (des *DpuExtensionService) ToDeletionRequestProto() *corev1.DeleteDpuExtensionServiceRequest {
	return &corev1.DeleteDpuExtensionServiceRequest{
		ServiceId: des.ID.String(),
	}
}

// ToVersionDeletionRequestProto builds the workflow request that asks a
// Site to delete a single version of this DPU Extension Service. Shares
// the `DeleteDpuExtensionServiceRequest` proto with the whole-service
// delete; the populated `Versions` field is what scopes the request.
func (des *DpuExtensionService) ToVersionDeletionRequestProto(versionID string) *corev1.DeleteDpuExtensionServiceRequest {
	return &corev1.DeleteDpuExtensionServiceRequest{
		ServiceId: des.ID.String(),
		Versions:  []string{versionID},
	}
}

var _ bun.BeforeAppendModelHook = (*DpuExtensionService)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (des *DpuExtensionService) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		des.Created = db.GetCurTime()
		des.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		des.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*DpuExtensionService)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (des *DpuExtensionService) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)
	return nil
}

// DpuExtensionServiceCreateInput is used to create a new DpuExtensionService
type DpuExtensionServiceCreateInput struct {
	DpuExtensionServiceID *uuid.UUID
	Name                  string
	Description           *string
	ServiceType           string
	SiteID                uuid.UUID
	TenantID              uuid.UUID
	Version               *string
	VersionInfo           *DpuExtensionServiceVersionInfo
	ActiveVersions        []string
	Status                string
	CreatedBy             uuid.UUID
}

// DpuExtensionServiceFilterInput is used to filter the DpuExtensionService objects
type DpuExtensionServiceFilterInput struct {
	DpuExtensionServiceIDs []uuid.UUID
	Names                  []string
	ServiceTypes           []string
	SiteIDs                []uuid.UUID
	TenantIDs              []uuid.UUID
	Versions               []string
	Statuses               []string
	SearchQuery            *string
}

// DpuExtensionServiceUpdateInput is used to update a DpuExtensionService object
type DpuExtensionServiceUpdateInput struct {
	DpuExtensionServiceID uuid.UUID
	Name                  *string
	Description           *string
	Version               *string
	VersionInfo           *DpuExtensionServiceVersionInfo
	ActiveVersions        []string
	Status                *string
	IsMissingOnSite       *bool
}

// DpuExtensionServiceClearInput is used to clear a DpuExtensionService object
type DpuExtensionServiceClearInput struct {
	DpuExtensionServiceID uuid.UUID
	Description           bool
	Version               bool
	VersionInfo           bool
}

// DpuExtensionServiceDAO is an interface for interacting with the DpuExtensionService model
type DpuExtensionServiceDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input DpuExtensionServiceCreateInput) (*DpuExtensionService, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*DpuExtensionService, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter DpuExtensionServiceFilterInput, page paginator.PageInput, includeRelations []string) ([]DpuExtensionService, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input DpuExtensionServiceUpdateInput) (*DpuExtensionService, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input DpuExtensionServiceClearInput) (*DpuExtensionService, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// DpuExtensionServiceSQLDAO is an implementation of the DpuExtensionServiceDAO interface
type DpuExtensionServiceSQLDAO struct {
	dbSession *db.Session
	DpuExtensionServiceDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new DpuExtensionService
func (dessd DpuExtensionServiceSQLDAO) Create(ctx context.Context, tx *db.Tx, input DpuExtensionServiceCreateInput) (*DpuExtensionService, error) {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.Create")
	if desDAOSpan != nil {
		defer desDAOSpan.End()

		dessd.tracerSpan.SetAttribute(desDAOSpan, "name", input.Name)
	}

	var id uuid.UUID
	if input.DpuExtensionServiceID != nil {
		id = *input.DpuExtensionServiceID
	} else {
		id = uuid.New()
	}

	des := &DpuExtensionService{
		ID:             id,
		Name:           input.Name,
		Description:    input.Description,
		ServiceType:    input.ServiceType,
		SiteID:         input.SiteID,
		TenantID:       input.TenantID,
		Version:        input.Version,
		VersionInfo:    input.VersionInfo,
		ActiveVersions: input.ActiveVersions,
		Status:         input.Status,
		CreatedBy:      input.CreatedBy,
	}

	_, err := db.GetIDB(tx, dessd.dbSession).NewInsert().Model(des).Exec(ctx)
	if err != nil {
		return nil, err
	}

	ndes, err := dessd.GetByID(ctx, tx, des.ID, nil)
	if err != nil {
		return nil, err
	}

	return ndes, nil
}

// GetByID returns a DpuExtensionService by ID and SiteID
// returns db.ErrDoesNotExist error if the record is not found
func (dessd DpuExtensionServiceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*DpuExtensionService, error) {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.GetByID")
	if desDAOSpan != nil {
		defer desDAOSpan.End()

		dessd.tracerSpan.SetAttribute(desDAOSpan, "id", id.String())
	}

	des := &DpuExtensionService{}

	query := db.GetIDB(tx, dessd.dbSession).NewSelect().Model(des).Where("des.id = ?", id)

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

	return des, nil
}

// GetAll returns all DpuExtensionServices with filtering and pagination
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if page.OrderBy is nil, then records are ordered by column specified in DpuExtensionServiceOrderByDefault in ascending order
func (dessd DpuExtensionServiceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter DpuExtensionServiceFilterInput, page paginator.PageInput, includeRelations []string) ([]DpuExtensionService, int, error) {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.GetAll")
	if desDAOSpan != nil {
		defer desDAOSpan.End()
	}

	dess := []DpuExtensionService{}
	if filter.DpuExtensionServiceIDs != nil && len(filter.DpuExtensionServiceIDs) == 0 {
		return dess, 0, nil
	}

	query := db.GetIDB(tx, dessd.dbSession).NewSelect().Model(&dess)

	if filter.DpuExtensionServiceIDs != nil {
		query = query.Where("des.id IN (?)", bun.In(filter.DpuExtensionServiceIDs))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "dpu_extension_service_ids", len(filter.DpuExtensionServiceIDs))
		}
	}

	if len(filter.Names) > 0 {
		query = query.Where("des.name IN (?)", bun.In(filter.Names))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "names", filter.Names)
		}
	}

	if len(filter.ServiceTypes) > 0 {
		query = query.Where("des.service_type IN (?)", bun.In(filter.ServiceTypes))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "service_types", filter.ServiceTypes)
		}
	}

	if len(filter.SiteIDs) > 0 {
		query = query.Where("des.site_id IN (?)", bun.In(filter.SiteIDs))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "site_ids", len(filter.SiteIDs))
		}
	}

	if len(filter.TenantIDs) > 0 {
		query = query.Where("des.tenant_id IN (?)", bun.In(filter.TenantIDs))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "tenant_ids", len(filter.TenantIDs))
		}
	}

	if len(filter.Versions) > 0 {
		query = query.Where("des.version IN (?)", bun.In(filter.Versions))
	}

	if len(filter.Statuses) > 0 {
		query = query.Where("des.status IN (?)", bun.In(filter.Statuses))

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "statuses", len(filter.Statuses))
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(des.name, ' ') || ' ' || coalesce(des.description, ' ') || ' ' || coalesce(des.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("des.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("des.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("des.status ILIKE ?", "%"+searchQuery+"%")
		})

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "search_query", searchQuery)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	orderBy := page.OrderBy
	if orderBy == nil {
		orderBy = paginator.NewDefaultOrderBy(DpuExtensionServiceOrderByDefault)
	}

	paginatorObj, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, orderBy, DpuExtensionServiceOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginatorObj.Query.Limit(paginatorObj.Limit).Offset(paginatorObj.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return dess, paginatorObj.Total, nil
}

// Update updates specified fields of an existing DpuExtensionService
// The updated fields are assumed to be set to non-null values
func (dessd DpuExtensionServiceSQLDAO) Update(ctx context.Context, tx *db.Tx, input DpuExtensionServiceUpdateInput) (*DpuExtensionService, error) {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.Update")
	if desDAOSpan != nil {
		defer desDAOSpan.End()

		dessd.tracerSpan.SetAttribute(desDAOSpan, "id", input.DpuExtensionServiceID.String())
	}

	des := &DpuExtensionService{
		ID: input.DpuExtensionServiceID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		des.Name = *input.Name
		updatedFields = append(updatedFields, "name")

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "name", *input.Name)
		}
	}

	if input.Description != nil {
		des.Description = input.Description
		updatedFields = append(updatedFields, "description")
	}

	if input.Version != nil {
		des.Version = input.Version
		updatedFields = append(updatedFields, "version")

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "version", *input.Version)
		}
	}

	if input.VersionInfo != nil {
		des.VersionInfo = input.VersionInfo
		updatedFields = append(updatedFields, "version_info")
	}

	if input.ActiveVersions != nil {
		des.ActiveVersions = input.ActiveVersions
		updatedFields = append(updatedFields, "active_versions")
	}

	if input.Status != nil {
		des.Status = *input.Status
		updatedFields = append(updatedFields, "status")

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "status", input.Status)
		}
	}

	if input.IsMissingOnSite != nil {
		des.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")

		if desDAOSpan != nil {
			dessd.tracerSpan.SetAttribute(desDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
		}
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, dessd.dbSession).NewUpdate().Model(des).Column(updatedFields...).Where("id = ?", input.DpuExtensionServiceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	udes, err := dessd.GetByID(ctx, tx, des.ID, nil)
	if err != nil {
		return nil, err
	}

	return udes, nil
}

// Clear clears the specified fields of a DpuExtensionService object
func (dessd DpuExtensionServiceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input DpuExtensionServiceClearInput) (*DpuExtensionService, error) {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.Clear")
	if desDAOSpan != nil {
		defer desDAOSpan.End()
	}

	des := &DpuExtensionService{
		ID: input.DpuExtensionServiceID,
	}

	updatedFields := []string{}

	if input.Description {
		des.Description = nil
		updatedFields = append(updatedFields, "description")
	}

	if input.Version {
		des.Version = nil
		updatedFields = append(updatedFields, "version")
	}

	if input.VersionInfo {
		des.VersionInfo = nil
		updatedFields = append(updatedFields, "version_info")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, dessd.dbSession).NewUpdate().Model(des).Column(updatedFields...).Where("id = ?", input.DpuExtensionServiceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	udes, err := dessd.GetByID(ctx, tx, des.ID, nil)
	if err != nil {
		return nil, err
	}

	return udes, nil
}

// Delete deletes a DpuExtensionService by ID
// error is returned only if there is a db error
// if the object being deleted doesn't exist, error is not returned
func (dessd DpuExtensionServiceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, desDAOSpan := dessd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDAO.Delete")
	if desDAOSpan != nil {
		defer desDAOSpan.End()

		dessd.tracerSpan.SetAttribute(desDAOSpan, "id", id.String())
	}

	_, err := db.GetIDB(tx, dessd.dbSession).NewDelete().Model((*DpuExtensionService)(nil)).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewDpuExtensionServiceDAO returns a new DpuExtensionServiceDAO
func NewDpuExtensionServiceDAO(dbSession *db.Session) DpuExtensionServiceDAO {
	return &DpuExtensionServiceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
