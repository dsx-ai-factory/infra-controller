// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"reflect"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/types/known/timestamppb"
)

const (
	// SkuRelationName is the relation name for the Sku model
	SkuRelationName = "Sku"
	// names of order by fields
	SkuOrderByCreated = "created"
	skuOrderByUpdated = "updated"
	// SkuOrderByDefault default field to be used for ordering when none specified
	SkuOrderByDefault = SkuOrderByCreated
)

var (
	// SkuOrderByFields is a list of valid order by fields for the SKU model
	SkuOrderByFields = []string{SkuOrderByCreated, skuOrderByUpdated}
	// SkuRelatedEntities is a list of valid relation by fields for the Sku model
	SkuRelatedEntities = map[string]bool{
		SiteRelationName: true,
	}
)

// SkuComponents is a light wrapper around the protobuf so
// that we can implement our own marshal/unmarshal
// that understands how to work with protobuf messages
type SkuComponents struct {
	*corev1.SkuComponents
}

// Equal reports whether two `*SkuComponents` wrappers carry the same
// underlying proto, treating a nil wrapper as carrying a nil proto.
// Avoids dereferencing a nil wrapper to read `.SkuComponents`. Declared
// as a method (per the Go convention used by `time.Time.Equal`,
// `cmp.Equal`, etc.) so callers can write `s.Equal(other)` instead of
// reaching for a free helper.
func (s *SkuComponents) Equal(other *SkuComponents) bool {
	var sp, op *corev1.SkuComponents
	if s != nil {
		sp = s.SkuComponents
	}
	if other != nil {
		op = other.SkuComponents
	}
	return reflect.DeepEqual(sp, op)
}

func (s *SkuComponents) UnmarshalJSON(b []byte) error {
	if s.SkuComponents == nil {
		s.SkuComponents = &corev1.SkuComponents{}
	}

	return protoJsonUnmarshalOptions.Unmarshal(b, s)
}

func (s *SkuComponents) MarshalJSON() ([]byte, error) {
	return protojson.Marshal(s)
}

// SKU represents entries in the sku table
type SKU struct {
	bun.BaseModel `bun:"table:sku,alias:sk"`

	ID                   string         `bun:"id,pk"`
	SiteID               uuid.UUID      `bun:"site_id,type:uuid,notnull"`
	Site                 *Site          `bun:"rel:belongs-to,join:site_id=id"`
	Description          string         `bun:"description,notnull,default:''"`
	SchemaVersion        uint32         `bun:"schema_version,notnull,default:0"`
	DeviceType           *string        `bun:"device_type"`
	Components           *SkuComponents `bun:"components,type:jsonb"`
	AssociatedMachineIds []string       `bun:"associated_machines,type:text[],default:'{}'"`
	Created              time.Time      `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated              time.Time      `bun:"updated,nullzero,notnull,default:current_timestamp"`
}

// ToProto converts the REST database projection into its Core SKU representation.
//
// SiteID is omitted because it is not carried by the Core SKU message; callers
// supply it separately to FromProto.
func (sk *SKU) ToProto() *corev1.Sku {
	proto := &corev1.Sku{
		Id:            sk.ID,
		Description:   &sk.Description,
		SchemaVersion: sk.SchemaVersion,
		DeviceType:    sk.DeviceType,
	}
	if !sk.Created.IsZero() {
		proto.Created = timestamppb.New(sk.Created)
	}
	if sk.Components != nil {
		proto.Components = sk.Components.SkuComponents
	}
	if sk.AssociatedMachineIds != nil {
		machineIDs := make([]*corev1.MachineId, 0, len(sk.AssociatedMachineIds))
		for _, id := range sk.AssociatedMachineIds {
			machineIDs = append(machineIDs, &corev1.MachineId{Id: id})
		}
		proto.AssociatedMachineIds = machineIDs
	}
	return proto
}

// FromProto populates the REST database projection from a Core SKU returned by
// inventory synchronization or an immediate REST mutation. A nil proto is a
// no-op. This is the inverse of ToProto; siteID is supplied by the caller
// because it is not carried on the Core SKU message.
//
// Field-level contract:
//   - `sk.ID` is overwritten with `proto.Id` (callers pre-validate
//     non-empty IDs at the activity layer).
//   - `Description` uses the protobuf default when absent, so a nil Core
//     description clears any stale REST projection value.
//   - `Created` is normalized to REST DB precision when Core supplies a valid
//     timestamp and is zero otherwise so create and update callers can apply
//     their respective fallback semantics.
//   - `Components` mirrors the proto: stays nil when `proto.Components`
//     is nil, otherwise wraps it, so the activity layer can distinguish
//     "not provided" from "explicitly set".
//   - `AssociatedMachineIds` is built from `proto.AssociatedMachineIds`,
//     skipping entries with empty IDs. Stays nil when the proto's list
//     is nil so the activity layer can distinguish "not provided" from
//     "explicitly empty".
func (sk *SKU) FromProto(proto *corev1.Sku, siteID uuid.UUID) {
	if proto == nil {
		return
	}
	sk.ID = proto.Id
	sk.SiteID = siteID
	sk.Description = proto.GetDescription()
	sk.SchemaVersion = proto.SchemaVersion
	sk.DeviceType = proto.DeviceType
	sk.Created = time.Time{}
	if proto.Created != nil && proto.Created.IsValid() {
		sk.Created = proto.Created.AsTime().UTC().Round(time.Microsecond)
	}
	if proto.Components != nil {
		sk.Components = &SkuComponents{SkuComponents: proto.Components}
	} else {
		sk.Components = nil
	}
	if proto.AssociatedMachineIds == nil {
		sk.AssociatedMachineIds = nil
	} else {
		machineIDs := []string{}
		for _, amid := range proto.AssociatedMachineIds {
			if strID := amid.GetId(); strID != "" {
				machineIDs = append(machineIDs, strID)
			}
		}
		sk.AssociatedMachineIds = machineIDs
	}
}

// SkuCreateInput input parameters for Create method
type SkuCreateInput struct {
	SkuID                string // Core is authoritative, so the ID must be provided when creating its REST projection.
	SiteID               uuid.UUID
	Description          string
	SchemaVersion        uint32
	Components           *SkuComponents
	DeviceType           *string
	AssociatedMachineIds []string
	Created              *time.Time
}

// SkuUpdateInput input parameters for Update method
type SkuUpdateInput struct {
	SkuID                string
	Description          *string
	SchemaVersion        *uint32
	Components           *SkuComponents
	DeviceType           *string
	AssociatedMachineIds []string
	Created              *time.Time
}

// SkuFilterInput input parameters for Filter method
type SkuFilterInput struct {
	SiteIDs              []uuid.UUID
	SkuIDs               []string
	DeviceTypes          []string
	AssociatedMachineIds []string
}

var _ bun.BeforeAppendModelHook = (*SKU)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (s *SKU) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		now := db.GetCurTime()
		if s.Created.IsZero() {
			s.Created = now
		}
		s.Updated = now
	case *bun.UpdateQuery:
		s.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SKU)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (s *SKU) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// SkuDAO is an interface for interacting with the SKU model
type SkuDAO interface {
	// Create used to create new row
	Create(ctx context.Context, tx *db.Tx, input SkuCreateInput) (*SKU, error)
	// Update used to update row
	Update(ctx context.Context, tx *db.Tx, input SkuUpdateInput) (*SKU, error)
	// Delete used to delete row
	Delete(ctx context.Context, tx *db.Tx, skuID string) error
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter SkuFilterInput, page paginator.PageInput) ([]SKU, int, error)
	// Get returns row for specified ID
	Get(ctx context.Context, tx *db.Tx, skuID string) (*SKU, error)
}

// SkuSQLDAO is an implementation of the SkuDAO interface
type SkuSQLDAO struct {
	dbSession *db.Session
	SkuDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SKU from the given parameters
// SKU comes from NICo, so SkuID is required
func (ssd SkuSQLDAO) Create(ctx context.Context, tx *db.Tx, input SkuCreateInput) (_ *SKU, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SkuDAO.Create")
	if skuDAOSpan != nil {
		defer func() {
			skuDAOSpan.EndWith(retErr)
		}()
	}

	sk := &SKU{
		ID:                   input.SkuID,
		SiteID:               input.SiteID,
		Description:          input.Description,
		SchemaVersion:        input.SchemaVersion,
		DeviceType:           input.DeviceType,
		Components:           input.Components,
		AssociatedMachineIds: input.AssociatedMachineIds,
	}
	if input.Created != nil && !input.Created.IsZero() {
		sk.Created = input.Created.UTC().Round(time.Microsecond)
	}

	_, err := db.GetIDB(tx, ssd.dbSession).NewInsert().Model(sk).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return ssd.Get(ctx, tx, sk.ID)
}

// Get returns a SKU by ID
// returns db.ErrDoesNotExist error if the record is not found
func (ssd SkuSQLDAO) Get(ctx context.Context, tx *db.Tx, id string) (_ *SKU, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SkuDAO.Get")
	if skuDAOSpan != nil {
		defer func() {
			skuDAOSpan.EndWith(retErr)
		}()
		ssd.tracerSpan.SetAttribute(skuDAOSpan, "id", id)
	}

	sk := &SKU{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(sk).Where("sk.id = ?", id)

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return sk, nil
}

// setQueryWithFilter populates the lookup query based on specified filter
func (ssd SkuSQLDAO) setQueryWithFilter(filter SkuFilterInput, query *bun.SelectQuery, skuDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if len(filter.SiteIDs) > 0 {
		query = query.Where("site_id IN (?)", bun.In(filter.SiteIDs))
		if skuDAOSpan != nil {
			ssd.tracerSpan.SetAttribute(skuDAOSpan, "site_ids", filter.SiteIDs)
		}
	}
	if len(filter.SkuIDs) > 0 {
		query = query.Where("id IN (?)", bun.In(filter.SkuIDs))
		if skuDAOSpan != nil {
			ssd.tracerSpan.SetAttribute(skuDAOSpan, "sku_ids", filter.SkuIDs)
		}
	}
	if len(filter.DeviceTypes) > 0 {
		query = query.Where("device_type IN (?)", bun.In(filter.DeviceTypes))
		if skuDAOSpan != nil {
			ssd.tracerSpan.SetAttribute(skuDAOSpan, "device_types", filter.DeviceTypes)
		}
	}
	if len(filter.AssociatedMachineIds) > 0 {
		// For array type, use overlap '&&' with a typed array literal to work with COUNT.
		query = query.Where("sk.associated_machines && ARRAY[?]::text[]", bun.In(filter.AssociatedMachineIds))
		if skuDAOSpan != nil {
			ssd.tracerSpan.SetAttribute(skuDAOSpan, "associated_machine_ids", filter.AssociatedMachineIds)
		}
	}

	return query, nil
}

// GetAll returns all SKUs with optional filters
// If orderBy is nil, then records are ordered by column specified
// in SkuOrderByDefault in ascending order
func (ssd SkuSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SkuFilterInput, page paginator.PageInput) (_ []SKU, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SkuDAO.GetAll")
	if skuDAOSpan != nil {
		defer func() {
			skuDAOSpan.EndWith(retErr)
		}()
	}

	skus := []SKU{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(&skus)

	query, err := ssd.setQueryWithFilter(filter, query, skuDAOSpan)
	if err != nil {
		return skus, 0, err
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SkuOrderByDefault)
	}

	pager, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SkuOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = pager.Query.Limit(pager.Limit).Offset(pager.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return skus, pager.Total, nil
}

// Update updates specified fields of an existing SKU
func (ssd SkuSQLDAO) Update(ctx context.Context, tx *db.Tx, input SkuUpdateInput) (_ *SKU, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SkuDAO.Update")
	if skuDAOSpan != nil {
		defer func() {
			skuDAOSpan.EndWith(retErr)
		}()
		ssd.tracerSpan.SetAttribute(skuDAOSpan, "id", input.SkuID)
	}

	sk := &SKU{ID: input.SkuID}
	updatedFields := []string{}
	if input.Created != nil && !input.Created.IsZero() {
		sk.Created = input.Created.UTC().Round(time.Microsecond)
		updatedFields = append(updatedFields, "created")
	}

	if input.Description != nil {
		sk.Description = *input.Description
		updatedFields = append(updatedFields, "description")
	}

	if input.SchemaVersion != nil {
		sk.SchemaVersion = *input.SchemaVersion
		updatedFields = append(updatedFields, "schema_version")
	}

	if input.Components != nil {
		sk.Components = input.Components
		updatedFields = append(updatedFields, "components")
	}

	if input.DeviceType != nil {
		sk.DeviceType = input.DeviceType
		updatedFields = append(updatedFields, "device_type")
	}

	if input.AssociatedMachineIds != nil {
		// Write empty array instead of NULL
		if len(input.AssociatedMachineIds) == 0 {
			sk.AssociatedMachineIds = []string{}
		} else {
			sk.AssociatedMachineIds = input.AssociatedMachineIds
		}
		updatedFields = append(updatedFields, "associated_machines")
	}

	if len(updatedFields) > 0 {
		_, err := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(sk).Column(updatedFields...).Where("sk.id = ?", input.SkuID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	return ssd.Get(ctx, tx, sk.ID)
}

// Delete deletes a SKU by ID
func (ssd SkuSQLDAO) Delete(ctx context.Context, tx *db.Tx, id string) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SkuDAO.Delete")
	if skuDAOSpan != nil {
		defer func() {
			skuDAOSpan.EndWith(retErr)
		}()
		ssd.tracerSpan.SetAttribute(skuDAOSpan, "id", id)
	}

	sk := &SKU{ID: id}

	_, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model(sk).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSkuDAO returns a new SkuDAO
func NewSkuDAO(dbSession *db.Session) SkuDAO {
	return &SkuSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
