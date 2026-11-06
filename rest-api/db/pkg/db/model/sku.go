// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"go.opentelemetry.io/otel/attribute"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// SkuRelationName is the relation name for the Sku model
	SkuRelationName = "Sku"
	// names of order by fields
	SkuOrderByID      = "id"
	SkuOrderByCreated = "created"
	skuOrderByUpdated = "updated"
	// SkuOrderByDefault default field to be used for ordering when none specified
	SkuOrderByDefault = SkuOrderByCreated
)

var (
	// SkuOrderByFields is a list of valid order by fields for the SKU model
	SkuOrderByFields = []string{SkuOrderByID, SkuOrderByCreated, skuOrderByUpdated}
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

// Equal compares protobuf field values, ignoring runtime bookkeeping such as
// cached message state. A nil wrapper is treated as a nil underlying proto.
func (s *SkuComponents) Equal(other *SkuComponents) bool {
	var sp, op *corev1.SkuComponents
	if s != nil {
		sp = s.SkuComponents
	}
	if other != nil {
		op = other.SkuComponents
	}
	return proto.Equal(sp, op)
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

// SkuDeletion is the private recovery point for one local SKU incarnation.
// Nil means no deletion. WorkflowID is a unique attempt token, independent of the
// mutable Created/Updated metadata. RunID is absent until the original execution
// is acknowledged or verified from history. Confirmed survives local cleanup
// failure; LastObserved orders recovery fairly and never expires an attempt.
// These JSON keys are persisted across process restarts and rolling upgrades.
type SkuDeletion struct {
	WorkflowID   string     `json:"workflowId"`
	RunID        *string    `json:"runId"`
	Confirmed    bool       `json:"confirmed"`
	LastObserved *time.Time `json:"lastObserved"`
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
	// Deletion fences one Core attempt, including an unknown result. New rows have no attempt.
	Deletion *SkuDeletion `bun:"deletion,type:jsonb"`
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
	// ExpectedUpdated requires a matching stored `Updated` when non-nil.
	// Nil omits only the version check; writes still require no deletion intent.
	// No-field calls only read and ignore both write predicates.
	ExpectedUpdated *time.Time
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
	// Update writes supplied fields and stamps `Updated` only without a deletion
	// intent, checking `ExpectedUpdated` when set. Missing, pending-deletion and
	// version-mismatched rows all return `db.ErrDoesNotExist`. Nil ExpectedUpdated
	// omits only the version check. With no fields it reads without these predicates.
	Update(ctx context.Context, tx *db.Tx, input SkuUpdateInput) (*SKU, error)
	// Delete is the Site teardown path: it omits intent/version predicates but keeps FK checks.
	Delete(ctx context.Context, tx *db.Tx, skuID string) error
	// DeleteIfUnchanged requires the captured Site, no deletion intent and exact Updated.
	// False combines missing, foreign-Site, changed and pending-deletion rows
	// without distinguishing them.
	DeleteIfUnchanged(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, updated time.Time) (bool, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter SkuFilterInput, page paginator.PageInput) ([]SKU, int, error)
	// GetForAttachment takes KEY SHARE in ID order until tx completes, omitting
	// missing, foreign-Site and pending-deletion rows. A transaction is required.
	GetForAttachment(ctx context.Context, tx *db.Tx, siteID uuid.UUID, skuIDs []string) ([]SKU, error)
	// GetForUpdate reloads and locks one SKU in a required transaction.
	GetForUpdate(ctx context.Context, tx *db.Tx, skuID string) (*SKU, error)
	// HasExpectedMachineReferences checks public and private references globally.
	HasExpectedMachineReferences(ctx context.Context, tx *db.Tx, skuID string) (bool, error)
	// SetDeletion writes or clears private deletion state for the locked incarnation.
	SetDeletion(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, expectedWorkflowID *string, deletion *SkuDeletion) (bool, error)
	// GetPendingDeletions orders attempts fairly, with unobserved attempts first.
	GetPendingDeletions(ctx context.Context, siteID uuid.UUID) ([]SKU, error)
	// BindDeletionRun persists the acknowledged original run without replacing a binding.
	BindDeletionRun(ctx context.Context, skuID string, siteID uuid.UUID, workflowID, runID string) error
	// MarkDeletionObserved records a fair observation attempt and reloads its current state.
	MarkDeletionObserved(ctx context.Context, skuID string, siteID uuid.UUID, workflowID string) (*SKU, error)
	// ResolveDeletion confirms and cleans up an attempt, or clears a verified rejection.
	ResolveDeletion(ctx context.Context, skuID string, siteID uuid.UUID, workflowID string, confirmed bool) (bool, error)
	// DeleteConfirmed removes only the recorded, confirmed local incarnation.
	DeleteConfirmed(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, workflowID string) (bool, error)
	// Get returns row for specified ID
	Get(ctx context.Context, tx *db.Tx, skuID string) (*SKU, error)
}

// SkuSQLDAO is an implementation of the SkuDAO interface
type SkuSQLDAO struct {
	dbSession *db.Session
	SkuDAO
}

// Create creates a new SKU from the given parameters
// SKU comes from NICo, so SkuID is required
func (ssd SkuSQLDAO) Create(ctx context.Context, tx *db.Tx, input SkuCreateInput) (_ *SKU, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.Create")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()

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
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.Get")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", id))

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

// GetForAttachment returns requested SKUs belonging to the Site and holds them
// until the transaction completes. Missing, foreign-Site and pending-deletion
// SKUs are omitted after row locking. An empty ID list selects nothing; a
// transaction is always required.
func (ssd SkuSQLDAO) GetForAttachment(ctx context.Context, tx *db.Tx, siteID uuid.UUID, skuIDs []string) (_ []SKU, retErr error) {
	if tx == nil {
		return nil, errors.New("transaction is required to lock SKU attachments")
	}
	if len(skuIDs) == 0 {
		return []SKU{}, nil
	}
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.GetForAttachment")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()

	// `Update` never changes `SiteID`, so moving a SKU requires deletion and
	// recreation. KEY SHARE makes that deletion wait until this transaction
	// ends while ordinary metadata updates can proceed. Lock rows in ID order.
	skus := []SKU{}
	err := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(&skus).
		Where("sk.site_id = ?", siteID).
		Where("sk.id IN (?)", bun.In(skuIDs)).
		OrderExpr("sk.id ASC").For("KEY SHARE").Scan(ctx)
	if err != nil {
		return nil, err
	}
	// Check after KEY SHARE was acquired. A pending deletion must not admit a
	// reference even when the query began before the deletion intent committed.
	available := skus[:0]
	for _, sku := range skus {
		if sku.Deletion == nil {
			available = append(available, sku)
		}
	}
	return available, nil
}

// setQueryWithFilter populates the lookup query based on specified filter
func (ssd SkuSQLDAO) setQueryWithFilter(filter SkuFilterInput, query *bun.SelectQuery, skuDAOSpan otrace.Span) (*bun.SelectQuery, error) {
	if len(filter.SiteIDs) > 0 {
		query = query.Where("site_id IN (?)", bun.In(filter.SiteIDs))
	}
	if len(filter.SkuIDs) > 0 {
		query = query.Where("id IN (?)", bun.In(filter.SkuIDs))
	}
	if len(filter.DeviceTypes) > 0 {
		query = query.Where("device_type IN (?)", bun.In(filter.DeviceTypes))
	}
	if len(filter.AssociatedMachineIds) > 0 {
		// For array type, use overlap '&&' with a typed array literal to work with COUNT.
		query = query.Where("sk.associated_machines && ARRAY[?]::text[]", bun.In(filter.AssociatedMachineIds))
	}

	return query, nil
}

// GetAll returns all SKUs with optional filters
// If orderBy is nil, then records are ordered by column specified
// in SkuOrderByDefault in ascending order
func (ssd SkuSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SkuFilterInput, page paginator.PageInput) (_ []SKU, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.GetAll")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()

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

// Update writes supplied fields and stamps `Updated` only without a deletion
// intent, checking `ExpectedUpdated` when set. Missing, pending-deletion and
// version-mismatched rows all return `db.ErrDoesNotExist`. Nil ExpectedUpdated
// omits only the version check. With no fields it reads without these predicates.
func (ssd SkuSQLDAO) Update(ctx context.Context, tx *db.Tx, input SkuUpdateInput) (_ *SKU, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.Update")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", input.SkuID))

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
		updatedFields = append(updatedFields, "updated")
		query := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(sk).Column(updatedFields...).Where("sk.id = ?", input.SkuID).Where("sk.deletion IS NULL")
		if input.ExpectedUpdated != nil {
			query = query.Where("sk.updated = ?", *input.ExpectedUpdated)
		}
		result, err := query.Exec(ctx)
		if err != nil {
			return nil, err
		}
		count, err := result.RowsAffected()
		if err != nil {
			return nil, err
		}
		if count == 0 {
			return nil, db.ErrDoesNotExist
		}
	}

	return ssd.Get(ctx, tx, sk.ID)
}

// Delete removes a SKU by ID, including pending attempts during Site teardown.
// HTTP deletion and inventory use the conditional deletion methods instead.
func (ssd SkuSQLDAO) Delete(ctx context.Context, tx *db.Tx, id string) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.Delete")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", id))

	sk := &SKU{ID: id}

	_, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model(sk).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteIfUnchanged deletes only an unfenced row on the captured Site with matching Updated.
// False combines missing, foreign-Site, changed and pending-deletion rows
// without distinguishing them.
func (ssd SkuSQLDAO) DeleteIfUnchanged(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, updated time.Time) (_ bool, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.DeleteIfUnchanged")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	result, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model((*SKU)(nil)).
		Where("id = ?", skuID).Where("site_id = ?", siteID).Where("updated = ?", updated).Where("deletion IS NULL").Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count > 0, err
}

// NewSkuDAO returns a new SkuDAO
func NewSkuDAO(dbSession *db.Session) SkuDAO {
	return &SkuSQLDAO{
		dbSession: dbSession,
	}
}

// GetForUpdate conflicts with attachment admission's KEY SHARE lock. Callers
// acquire the Site row first so Site teardown and SKU mutations use one order.
func (ssd SkuSQLDAO) GetForUpdate(ctx context.Context, tx *db.Tx, skuID string) (_ *SKU, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.GetForUpdate")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	if tx == nil {
		return nil, errors.New("transaction is required to lock a SKU")
	}
	sku := &SKU{}
	err := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(sku).Where("sk.id = ?", skuID).For("UPDATE").Scan(ctx)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, db.ErrDoesNotExist
	}
	return sku, err
}

func (ssd SkuSQLDAO) HasExpectedMachineReferences(ctx context.Context, tx *db.Tx, skuID string) (_ bool, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.HasExpectedMachineReferences")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	var referenced bool
	err := db.GetIDB(tx, ssd.dbSession).NewRaw(`SELECT EXISTS (SELECT 1 FROM expected_machine WHERE sku_id = ?) OR EXISTS (SELECT 1 FROM pending_expected_machine WHERE sku_id = ?)`, skuID, skuID).Scan(ctx, &referenced)
	return referenced, err
}

func (ssd SkuSQLDAO) SetDeletion(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, expectedWorkflowID *string, deletion *SkuDeletion) (_ bool, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.SetDeletion")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	if tx == nil {
		return false, errors.New("transaction is required to write SKU deletion state")
	}
	query := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(&SKU{Deletion: deletion}).Column("deletion", "updated").Where("sk.id = ?", skuID).Where("sk.site_id = ?", siteID)
	if expectedWorkflowID == nil {
		query = query.Where("sk.deletion IS NULL")
	} else {
		query = query.Where("sk.deletion->>'workflowId' = ?", *expectedWorkflowID)
	}
	result, err := query.Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count > 0, err
}

func (ssd SkuSQLDAO) GetPendingDeletions(ctx context.Context, siteID uuid.UUID) (_ []SKU, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.GetPendingDeletions")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("site_id", siteID.String()))

	skus := []SKU{}
	err := ssd.dbSession.DB.NewSelect().Model(&skus).Where("sk.site_id = ?", siteID).Where("sk.deletion IS NOT NULL").OrderExpr("(sk.deletion->>'lastObserved')::timestamptz ASC NULLS FIRST, sk.id ASC").Scan(ctx)
	return skus, err
}

func (ssd SkuSQLDAO) DeleteConfirmed(ctx context.Context, tx *db.Tx, skuID string, siteID uuid.UUID, workflowID string) (_ bool, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.DeleteConfirmed")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	result, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model((*SKU)(nil)).Where("id = ?", skuID).Where("site_id = ?", siteID).Where("deletion->>'workflowId' = ?", workflowID).Where("deletion->>'confirmed' = 'true'").Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count > 0, err
}

// withDeletion locks Site before SKU, matching Site teardown's row order. A
// deleted Site must never be restored by a delayed deletion completion.
func (ssd SkuSQLDAO) withDeletion(ctx context.Context, skuID string, siteID uuid.UUID, workflowID string, apply func(*db.Tx, *SKU) error) error {
	return db.WithTx(ctx, ssd.dbSession, func(tx *db.Tx) error {
		_, err := NewSiteDAO(ssd.dbSession).GetForShare(ctx, tx, siteID)
		if err != nil {
			return err
		}
		err = tx.TryAcquireAdvisoryLock(ctx, db.GetAdvisoryLockIDFromString(skuID), nil)
		if err != nil {
			return err
		}
		sku, err := ssd.GetForUpdate(ctx, tx, skuID)
		if err != nil {
			return err
		}
		if sku.SiteID != siteID || sku.Deletion == nil || sku.Deletion.WorkflowID != workflowID {
			return db.ErrDoesNotExist
		}
		return apply(tx, sku)
	})
}

// BindDeletionRun acknowledges only the run started for this exact incarnation.
func (ssd SkuSQLDAO) BindDeletionRun(ctx context.Context, skuID string, siteID uuid.UUID, workflowID, runID string) (retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.BindDeletionRun")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	if runID == "" {
		return errors.New("original Core run ID is required")
	}
	return ssd.withDeletion(ctx, skuID, siteID, workflowID, func(tx *db.Tx, sku *SKU) error {
		if sku.Deletion.RunID != nil {
			if *sku.Deletion.RunID == runID {
				return nil
			}
			return errors.New("SKU deletion is bound to another original run")
		}
		deletion := *sku.Deletion
		deletion.RunID = &runID
		applied, err := ssd.SetDeletion(ctx, tx, skuID, siteID, &workflowID, &deletion)
		if err == nil && !applied {
			return db.ErrDoesNotExist
		}
		return err
	})
}

// MarkDeletionObserved rotates attempts before any history I/O, so unavailable
// histories cannot keep later attempts behind them on every inventory activity.
func (ssd SkuSQLDAO) MarkDeletionObserved(ctx context.Context, skuID string, siteID uuid.UUID, workflowID string) (_ *SKU, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.MarkDeletionObserved")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	var current *SKU
	err := ssd.withDeletion(ctx, skuID, siteID, workflowID, func(tx *db.Tx, sku *SKU) error {
		now := db.GetCurTime()
		deletion := *sku.Deletion
		deletion.LastObserved = &now
		applied, err := ssd.SetDeletion(ctx, tx, skuID, siteID, &workflowID, &deletion)
		if err == nil && !applied {
			return db.ErrDoesNotExist
		}
		if err != nil {
			return err
		}
		sku.Deletion = &deletion
		current = sku
		return nil
	})
	return current, err
}

// ResolveDeletion commits confirmed Core completion even if local FK cleanup
// fails. Later callers can retry that cleanup without a Temporal client. A
// rejection clears only its own unconfirmed attempt, never a newer incarnation.
func (ssd SkuSQLDAO) ResolveDeletion(ctx context.Context, skuID string, siteID uuid.UUID, workflowID string, confirmed bool) (_ bool, retErr error) {
	ctx, skuDAOSpan := cotel.StartSpan(ctx, "SkuDAO.ResolveDeletion")
	defer func() { cotel.EndSpan(skuDAOSpan, retErr) }()
	cotel.SetAttribute(skuDAOSpan, attribute.String("id", skuID))

	var cleanupErr error
	err := ssd.withDeletion(ctx, skuID, siteID, workflowID, func(tx *db.Tx, sku *SKU) error {
		if !confirmed && !sku.Deletion.Confirmed {
			applied, err := ssd.SetDeletion(ctx, tx, skuID, siteID, &workflowID, nil)
			if err == nil && !applied {
				return db.ErrDoesNotExist
			}
			return err
		}
		deletion := *sku.Deletion
		deletion.Confirmed = true
		applied, err := ssd.SetDeletion(ctx, tx, skuID, siteID, &workflowID, &deletion)
		if err == nil && !applied {
			return db.ErrDoesNotExist
		}
		if err != nil {
			return err
		}
		cleanupErr = tx.WithSavepoint(ctx, func(cleanupTx *db.Tx) error {
			applied, err := ssd.DeleteConfirmed(ctx, cleanupTx, skuID, siteID, workflowID)
			if err == nil && !applied {
				return db.ErrDoesNotExist
			}
			return err
		})
		if errors.Is(cleanupErr, db.ErrTransactionSavepoint) {
			return cleanupErr
		}
		return nil
	})
	if errors.Is(err, db.ErrDoesNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return true, cleanupErr
}
