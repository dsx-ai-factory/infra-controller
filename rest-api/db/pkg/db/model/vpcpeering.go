// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	"github.com/uptrace/bun"
)

const (
	// VpcPeering status is pending
	VpcPeeringStatusPending = "Pending"
	// VpcPeering status is configuring
	VpcPeeringStatusConfiguring = "Configuring"
	// VpcPeering status is requested, the requester has issued request to the owner of the peer VPC
	VpcPeeringStatusRequested = "Requested"
	// VpcPeering status is ready
	VpcPeeringStatusReady = "Ready"
	// VpcPeering status is deleting
	VpcPeeringStatusDeleting = "Deleting"
	// VpcPeering status is error
	VpcPeeringStatusError = "Error"

	// VpcPeeringRelationName is the relation name for the VpcPeering model
	VpcPeeringRelationName = "VpcPeering"
	// Vpc1RelationName is the relation name for Vpc1 field in VpcPeering
	Vpc1RelationName = "Vpc1"
	// Vpc2RelationName is the relation name for Vpc2 field in VpcPeering
	Vpc2RelationName = "Vpc2"

	// VpcPeering default field to be used for ordering when non specified
	VpcPeeringOrderByDefault = "created"
)

var VpcPeeringStatusMap = map[string]bool{
	VpcPeeringStatusPending:     true,
	VpcPeeringStatusConfiguring: true,
	VpcPeeringStatusRequested:   true,
	VpcPeeringStatusReady:       true,
	VpcPeeringStatusDeleting:    true,
	VpcPeeringStatusError:       true,
}

var (
	VpcPeeringOrderByFields = []string{"id", "vpc1_id", "vpc2_id", "site_id", "created", "updated"}
	// VpcPeeringRelatedEntities is a list of valid relation by fields for the VpcPeering model
	VpcPeeringRelatedEntities = map[string]bool{
		Vpc1RelationName:                   true,
		Vpc2RelationName:                   true,
		SiteRelationName:                   true,
		InfrastructureProviderRelationName: true,
		TenantRelationName:                 true,
	}
)

type VpcPeering struct {
	bun.BaseModel `bun:"table:vpc_peering,alias:vp"`

	ID     uuid.UUID `bun:"id,type:uuid,pk,default:gen_random_uuid(),notnull"`
	Vpc1ID uuid.UUID `bun:"vpc1_id,type:uuid,notnull"`
	Vpc1   *Vpc      `bun:"rel:belongs-to,join:vpc1_id=id"`
	Vpc2ID uuid.UUID `bun:"vpc2_id,type:uuid,notnull"`
	Vpc2   *Vpc      `bun:"rel:belongs-to,join:vpc2_id=id"`
	SiteID uuid.UUID `bun:"site_id,type:uuid,notnull"`
	Site   *Site     `bun:"rel:belongs-to,join:site_id=id"`

	IsMultiTenant bool `bun:"is_multi_tenant,notnull"`

	InfrastructureProviderID *uuid.UUID              `bun:"infrastructure_provider_id,type:uuid"`
	InfrastructureProvider   *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	TenantID                 *uuid.UUID              `bun:"tenant_id,type:uuid"`
	Tenant                   *Tenant                 `bun:"rel:belongs-to,join:tenant_id=id"`

	Status string `bun:"status,notnull"`

	Created   time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated   time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted   *time.Time `bun:"deleted,soft_delete"`
	CreatedBy uuid.UUID  `bun:"type:uuid,notnull"`
}

// ToProto converts this VpcPeering into its workflow proto representation.
// Used as the canonical entity-to-proto conversion; the request-shape
// proto for create is produced by `APIVpcPeeringCreateRequest.ToProto`
// in api/pkg/api/model/vpcpeering.go, which sources its canonical wire
// fields from this method.
//
// The peering's Vpc1ID/Vpc2ID are sent through directly: the Site
// treats them as opaque identifiers, and any ControllerVpcID
// indirection has already been applied by the time the peering rows
// reference these UUIDs.
func (vp *VpcPeering) ToProto() *corev1.VpcPeering {
	return &corev1.VpcPeering{
		Id:        &corev1.VpcPeeringId{Value: vp.ID.String()},
		VpcId:     &corev1.VpcId{Value: vp.Vpc1ID.String()},
		PeerVpcId: &corev1.VpcId{Value: vp.Vpc2ID.String()},
	}
}

// FromProto populates this VpcPeering from its workflow proto
// representation. A nil proto is a no-op. This is the inverse of
// `ToProto` and exists for convention symmetry — currently no code
// path on the cloud side reconstructs a full VpcPeering entity from a
// `corev1.VpcPeering` (the site is the destination, not the source),
// but the method is provided so future reconciliation flows have a
// single canonical entry point.
//
// Field-level contract:
//   - `vp.ID` is preserved on a missing or unparseable `proto.Id`,
//     because callers pre-validate the UUID before calling.
//   - `vp.Vpc1ID` / `vp.Vpc2ID` are likewise preserved when the proto
//     omits the matching `VpcId` / `PeerVpcId` or carries an
//     unparseable value, so `FromProto` cannot silently zero out a
//     required foreign key.
func (vp *VpcPeering) FromProto(proto *corev1.VpcPeering) {
	if proto == nil {
		return
	}
	if proto.Id != nil {
		if id, err := uuid.Parse(proto.Id.Value); err == nil {
			vp.ID = id
		}
	}
	if proto.VpcId != nil {
		if id, err := uuid.Parse(proto.VpcId.Value); err == nil {
			vp.Vpc1ID = id
		}
	}
	if proto.PeerVpcId != nil {
		if id, err := uuid.Parse(proto.PeerVpcId.Value); err == nil {
			vp.Vpc2ID = id
		}
	}
}

// ToDeletionRequestProto builds the workflow request that asks a Site to
// delete this VpcPeering. Stays on the entity because the delete-by-id
// flow has no API request body.
func (vp *VpcPeering) ToDeletionRequestProto() *corev1.VpcPeeringDeletionRequest {
	return &corev1.VpcPeeringDeletionRequest{
		Id: &corev1.VpcPeeringId{Value: vp.ID.String()},
	}
}

type VpcPeeringCreateInput struct {
	VpcPeeringID             *uuid.UUID
	Vpc1ID                   uuid.UUID
	Vpc2ID                   uuid.UUID
	SiteID                   uuid.UUID
	IsMultiTenant            bool
	InfrastructureProviderID *uuid.UUID
	TenantID                 *uuid.UUID
	Status                   string
	CreatedByID              uuid.UUID
}

// VpcPeeringClearInput input parameters for Clear method
type VpcPeeringClearInput struct {
	VpcPeeringID uuid.UUID
	// Deleted clears the soft-delete timestamp (undelete).
	Deleted bool
}

type VpcPeeringFilterInput struct {
	IDs                       []uuid.UUID
	VpcIDs                    []uuid.UUID // Return record if vpc1_id IN (VpcIDs) OR vpc2_id IN (VpcIDs)
	SiteIDs                   []uuid.UUID
	IsMultiTenant             *bool
	InfrastructureProviderIDs []uuid.UUID
	TenantIDs                 []uuid.UUID
	// PeerTenantIDs filters peerings where vpc1 OR vpc2 belongs to any of the specified
	// tenants. This is a user-requested filter and is distinct from TenantIDs, which
	// scopes results to the caller's authorization context.
	PeerTenantIDs []uuid.UUID
	Statuses      []string
	// IncludeDeleted returns soft-deleted rows in addition to active ones.
	IncludeDeleted bool
}

func (vp *VpcPeering) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	// No "ON DELETE CASCADE" because both vpc and vpc peering are soft delete
	query.
		ForeignKey(`("vpc1_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("vpc2_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)

	return nil
}

type VpcPeeringDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input VpcPeeringCreateInput) (*VpcPeering, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter VpcPeeringFilterInput, page paginator.PageInput, includeRelations []string) ([]VpcPeering, int, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*VpcPeering, error)
	//
	UpdateStatusByID(ctx context.Context, tx *db.Tx, id uuid.UUID, newStatus string) error
	//
	Clear(ctx context.Context, tx *db.Tx, input VpcPeeringClearInput) (*VpcPeering, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	DeleteByVpcID(ctx context.Context, tx *db.Tx, vpcID uuid.UUID) error
}

type VpcPeeringSQLDAO struct {
	dbSession *db.Session
	VpcPeeringDAO
	tracerSpan *stracer.TracerSpan
}

// Create inserts a new VPC peering record into the database without checking for duplicates.
// It bypasses any uniqueness constraints, so duplicate detection must be handled
// externally, such as within the cloud API handler before calling this method.
func (vpsd VpcPeeringSQLDAO) Create(
	ctx context.Context,
	tx *db.Tx,
	input VpcPeeringCreateInput,
) (*VpcPeering, error) {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.Create")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()
	}

	vpc1ID, vpc2ID := input.Vpc1ID, input.Vpc2ID

	if vpc1ID == vpc2ID {
		return nil, errors.New("cannot peer a VPC with itself")
	}

	vpcPeeringID := uuid.New()
	if input.VpcPeeringID != nil {
		vpcPeeringID = *input.VpcPeeringID
	}

	status := input.Status
	if status == "" {
		status = VpcPeeringStatusPending
	}

	vp := &VpcPeering{
		ID:                       vpcPeeringID,
		Vpc1ID:                   vpc1ID,
		Vpc2ID:                   vpc2ID,
		SiteID:                   input.SiteID,
		IsMultiTenant:            input.IsMultiTenant,
		InfrastructureProviderID: input.InfrastructureProviderID,
		TenantID:                 input.TenantID,
		Status:                   status,
		Created:                  db.GetCurTime(),
		Updated:                  db.GetCurTime(),
		CreatedBy:                input.CreatedByID,
	}

	_, err := db.GetIDB(tx, vpsd.dbSession).NewInsert().Model(vp).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return vp, nil
}

func (vpsd VpcPeeringSQLDAO) GetAll(
	ctx context.Context,
	tx *db.Tx,
	filter VpcPeeringFilterInput,
	page paginator.PageInput,
	includeRelations []string,
) ([]VpcPeering, int, error) {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.GetAll")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()
	}

	vps := []VpcPeering{}
	query := db.GetIDB(tx, vpsd.dbSession).NewSelect().Model(&vps)
	if filter.IncludeDeleted {
		query = query.WhereAllWithDeleted()
	}
	query, err := vpsd.setQueryWithFilter(filter, query, vpDAOSpan)
	if err != nil {
		return vps, 0, err
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy == nil {
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(VpcPeeringOrderByDefault))
	} else {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		if page.OrderBy.Field != VpcPeeringOrderByDefault {
			multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(VpcPeeringOrderByDefault))
		}
	}

	paginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, VpcPeeringOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return vps, paginator.Total, nil
}

func (vpsd VpcPeeringSQLDAO) setQueryWithFilter(filter VpcPeeringFilterInput, query *bun.SelectQuery, vpDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.IDs != nil {
		query = query.Where("vp.id IN (?)", bun.In(filter.IDs))
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "id", filter.IDs)
	}

	if len(filter.VpcIDs) > 0 {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				WhereOr("vp.vpc1_id IN (?)", bun.In(filter.VpcIDs)).
				WhereOr("vp.vpc2_id IN (?)", bun.In(filter.VpcIDs))
		})
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "vpc_ids", filter.VpcIDs)
	}

	if filter.Statuses != nil {
		query = query.Where("vp.status IN (?)", bun.In(filter.Statuses))
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "status", filter.Statuses)
	}

	if filter.SiteIDs != nil {
		query = query.Where("vp.site_id IN (?)", bun.In(filter.SiteIDs))
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "site_ids", filter.SiteIDs)
	}

	if filter.IsMultiTenant != nil {
		query = query.Where("vp.is_multi_tenant = ?", *filter.IsMultiTenant)
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "is_multi_tenant", *filter.IsMultiTenant)
	}

	hasProviderIDs := len(filter.InfrastructureProviderIDs) > 0
	hasTenantIDs := len(filter.TenantIDs) > 0
	if hasProviderIDs && hasTenantIDs {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				WhereOr("vp.infrastructure_provider_id IN (?)", bun.In(filter.InfrastructureProviderIDs)).
				WhereOr("vp.tenant_id IN (?)", bun.In(filter.TenantIDs)).
				WhereOr("vp.vpc1_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.TenantIDs)).
				WhereOr("vp.vpc2_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.TenantIDs))
		})
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "infrastructure_provider_ids", filter.InfrastructureProviderIDs)
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "tenant_ids", filter.TenantIDs)
	} else if hasProviderIDs {
		query = query.Where("vp.infrastructure_provider_id IN (?)", bun.In(filter.InfrastructureProviderIDs))
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "infrastructure_provider_ids", filter.InfrastructureProviderIDs)
	} else if hasTenantIDs {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				WhereOr("vp.tenant_id IN (?)", bun.In(filter.TenantIDs)).
				WhereOr("vp.vpc1_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.TenantIDs)).
				WhereOr("vp.vpc2_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.TenantIDs))
		})
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "tenant_ids", filter.TenantIDs)
	}

	if len(filter.PeerTenantIDs) > 0 {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				WhereOr("vp.vpc1_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.PeerTenantIDs)).
				WhereOr("vp.vpc2_id IN (SELECT id FROM vpc WHERE tenant_id IN (?))", bun.In(filter.PeerTenantIDs))
		})
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "peer_tenant_ids", filter.PeerTenantIDs)
	}

	return query, nil
}

func (vpsd VpcPeeringSQLDAO) GetByID(
	ctx context.Context,
	tx *db.Tx,
	id uuid.UUID,
	includeRelations []string,
) (*VpcPeering, error) {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.GetByID")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()

		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "id", id)
	}

	vp := &VpcPeering{}

	query := db.GetIDB(tx, vpsd.dbSession).NewSelect().Model(vp).Where("vp.id = ?", id)

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

	return vp, nil
}

func (vpsd VpcPeeringSQLDAO) UpdateStatusByID(
	ctx context.Context,
	tx *db.Tx,
	id uuid.UUID,
	newStatus string,
) error {
	// Disallow undefined VPC peering status
	if !VpcPeeringStatusMap[newStatus] {
		return db.ErrInvalidValue
	}

	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.UpdateStatusByID")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "id", id)
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "status", newStatus)
	}

	_, err := db.GetIDB(tx, vpsd.dbSession).
		NewUpdate().
		Model((*VpcPeering)(nil)).
		Set("status = ?", newStatus).
		Set("updated = ?", db.GetCurTime()).
		Where("id = ?", id).
		Exec(ctx)

	return err
}

// Clear clears VpcPeering attributes based on provided arguments
func (vpsd VpcPeeringSQLDAO) Clear(ctx context.Context, tx *db.Tx, input VpcPeeringClearInput) (*VpcPeering, error) {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.Clear")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()

		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "id", input.VpcPeeringID)
	}

	if input.Deleted {
		_, err := db.GetIDB(tx, vpsd.dbSession).
			NewUpdate().
			Model((*VpcPeering)(nil)).
			Set("deleted = NULL").
			Set("updated = ?", db.GetCurTime()).
			Where("id = ?", input.VpcPeeringID).
			WhereAllWithDeleted().
			Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	vpcPeering, err := vpsd.GetByID(ctx, tx, input.VpcPeeringID, nil)
	if err != nil {
		return nil, err
	}
	return vpcPeering, nil
}

func (vpsd VpcPeeringSQLDAO) Delete(
	ctx context.Context,
	tx *db.Tx,
	id uuid.UUID,
) error {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.Delete")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()
	}

	vp := &VpcPeering{
		ID: id,
	}

	_, err := db.GetIDB(tx, vpsd.dbSession).NewDelete().Model(vp).Where("id = ?", id).Exec(ctx)

	return err
}

func (vpsd VpcPeeringSQLDAO) DeleteByVpcID(
	ctx context.Context,
	tx *db.Tx,
	vpcID uuid.UUID,
) error {
	ctx, vpDAOSpan := vpsd.tracerSpan.CreateChildInCurrentContext(ctx, "VpcPeeringDAO.DeleteByVpcID")
	if vpDAOSpan != nil {
		defer vpDAOSpan.End()
		vpsd.tracerSpan.SetAttribute(vpDAOSpan, "vpcID", vpcID)
	}

	_, err := db.GetIDB(tx, vpsd.dbSession).
		NewDelete().
		Model((*VpcPeering)(nil)).
		Where("vpc1_id = ? OR vpc2_id = ?", vpcID, vpcID).
		Exec(ctx)

	return err
}

func NewVpcPeeringDAO(dbSession *db.Session) VpcPeeringDAO {
	return &VpcPeeringSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
