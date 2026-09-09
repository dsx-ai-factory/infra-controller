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

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	// SSHKeyGroupSiteAssociationOrderByFields is a list of valid order by fields for the SSHKeyGroupSiteAssociation model
	SSHKeyGroupSiteAssociationOrderByFields = []string{"status", "created", "updated"}

	// SSHKeyGroupSiteAssociationRelatedEntities is a list of valid relation by fields for the SSHKeyGroupSiteAssociation model
	SSHKeyGroupSiteAssociationRelatedEntities = map[string]bool{
		SSHKeyGroupRelationName: true,
	}

	// SSHKeyGroupSiteAssociationEntityTypes is a list of valid choices for the EntityType field
	SSHKeyGroupSiteAssociationEntityTypes = map[string]bool{
		SiteRelationName:        true,
		SSHKeyGroupRelationName: true,
	}
)

const (
	// SSHKeyGroupSiteAssociationStatusSyncing status is syncing
	SSHKeyGroupSiteAssociationStatusSyncing = "Syncing"
	// SSHKeyGroupSiteAssociationStatusSynced status is synced
	SSHKeyGroupSiteAssociationStatusSynced = "Synced"
	// SSHKeyGroupSiteAssociationStatusError status is error
	SSHKeyGroupSiteAssociationStatusError = "Error"
	// SSHKeyGroupSiteAssociationStatusDeleting status is deleting
	SSHKeyGroupSiteAssociationStatusDeleting = "Deleting"

	// SSHKeyGroupSiteAssociationOrderByDefault default field to be used for ordering when none specified
	SSHKeyGroupSiteAssociationOrderByDefault = "created"
)

var (
	// SSHKeyGroupSiteAssociationStatusSyncingMap is a list of valid status for the SSHKeyGroupSiteAssociation model
	SSHKeyGroupSiteAssociationStatusSyncingMap = map[string]bool{
		SSHKeyGroupSiteAssociationStatusSyncing:  true,
		SSHKeyGroupSiteAssociationStatusSynced:   true,
		SSHKeyGroupSiteAssociationStatusError:    true,
		SSHKeyGroupSiteAssociationStatusDeleting: true,
	}
)

// SSHKeyGroupSiteAssociation associates a user sshkey group with different Sites
type SSHKeyGroupSiteAssociation struct {
	bun.BaseModel `bun:"table:ssh_key_group_site_association,alias:skgsa"`

	ID              uuid.UUID    `bun:"type:uuid,pk"`
	SSHKeyGroupID   uuid.UUID    `bun:"sshkey_group_id,type:uuid,notnull"`
	SSHKeyGroup     *SSHKeyGroup `bun:"rel:belongs-to,join:sshkey_group_id=id"`
	SiteID          uuid.UUID    `bun:"site_id,type:uuid,notnull"`
	Site            *Site        `bun:"rel:belongs-to,join:site_id=id"`
	Version         *string      `bun:"version"`
	Status          string       `bun:"status,notnull"`
	IsMissingOnSite bool         `bun:"is_missing_on_site,notnull"`
	Created         time.Time    `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time    `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time   `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID    `bun:"created_by,type:uuid,notnull"`
}

// SSHKeyGroupSiteAssociationCreateInput input parameters for Create method
type SSHKeyGroupSiteAssociationCreateInput struct {
	SSHKeyGroupID uuid.UUID
	SiteID        uuid.UUID
	Version       *string
	Status        string
	CreatedBy     uuid.UUID
}

// SSHKeyGroupSiteAssociationUpdateInput input parameters for Update method
type SSHKeyGroupSiteAssociationUpdateInput struct {
	ID              uuid.UUID
	SSHKeyGroupID   *uuid.UUID
	SiteID          *uuid.UUID
	Version         *string
	Status          *string
	IsMissingOnSite *bool
}

// SSHKeyGroupSiteAssociationFilterInput input parameters for GetAll filters
type SSHKeyGroupSiteAssociationFilterInput struct {
	SSHKeyGroupIDs []uuid.UUID
	SiteID         *uuid.UUID
	Version        *string
	Status         *string
}

// ToKeysetIdentifierProto builds the workflow proto identifier for this
// association's SSH Key Group, scoped to the owning organization. The
// SSHKeyGroup relation must be loaded for Org to be present.
func (skgsa *SSHKeyGroupSiteAssociation) ToKeysetIdentifierProto() *corev1.TenantKeysetIdentifier {
	var org string
	if skgsa.SSHKeyGroup != nil {
		org = skgsa.SSHKeyGroup.Org
	}
	return &corev1.TenantKeysetIdentifier{
		KeysetId:       skgsa.SSHKeyGroupID.String(),
		OrganizationId: org,
	}
}

// ToProto builds the canonical workflow proto representation of this
// SSH Key Group's per-Site association. `content` carries the synced
// public-key material because it isn't stored on the association
// record (SSH keys live on `SSHKeyAssociation` rows and are loaded by
// the caller); a nil `content` is fine and produces a Keyset with no
// content message.
//
// Request-shape protos (create / update / delete) are layered on top
// of this method and source the canonical wire fields from here so
// the per-method translations stay focused.
func (skgsa *SSHKeyGroupSiteAssociation) ToProto(content *corev1.TenantKeysetContent) *corev1.TenantKeyset {
	var version string
	if skgsa.Version != nil {
		version = *skgsa.Version
	}
	return &corev1.TenantKeyset{
		KeysetIdentifier: skgsa.ToKeysetIdentifierProto(),
		KeysetContent:    content,
		Version:          version,
	}
}

// FromProto populates this association from its workflow proto
// representation. A nil proto is a no-op. This is the inverse of
// `ToProto` and exists for convention symmetry — currently no code
// path on the cloud side reconstructs a full association entity
// from a `corev1.TenantKeyset` (the site is the destination, not
// the source), but the method is provided so future reconciliation
// flows have a single canonical entry point.
//
// Field-level contract:
//   - `skgsa.SSHKeyGroupID` is preserved on a missing or unparseable
//     `proto.KeysetIdentifier.KeysetId`, because callers pre-validate
//     UUIDs before calling.
//   - `Version` is cleared when the proto carries an empty value, so
//     `FromProto` is a clean reset rather than a partial merge.
//   - SSH key content is intentionally not materialized onto the
//     association: SSH keys are persisted on `SSHKeyAssociation`
//     rows, and reconstruction of those rows is the responsibility
//     of a higher-level reconciliation flow.
func (skgsa *SSHKeyGroupSiteAssociation) FromProto(proto *corev1.TenantKeyset) {
	if proto == nil {
		return
	}
	if proto.KeysetIdentifier != nil {
		if id, err := uuid.Parse(proto.KeysetIdentifier.KeysetId); err == nil {
			skgsa.SSHKeyGroupID = id
		}
	}
	if proto.Version != "" {
		v := proto.Version
		skgsa.Version = &v
	} else {
		skgsa.Version = nil
	}
}

// ToCreateRequestProto builds the workflow request that asks a Site to
// create this Tenant Keyset. content carries the synced public-key
// material (built by the caller from SSH Key Associations); the
// canonical wire fields are sourced from `ToProto`.
func (skgsa *SSHKeyGroupSiteAssociation) ToCreateRequestProto(content *corev1.TenantKeysetContent) *corev1.CreateTenantKeysetRequest {
	tk := skgsa.ToProto(content)
	return &corev1.CreateTenantKeysetRequest{
		KeysetIdentifier: tk.KeysetIdentifier,
		KeysetContent:    tk.KeysetContent,
		Version:          tk.Version,
	}
}

// ToUpdateRequestProto builds the workflow request that asks a Site to
// update this Tenant Keyset. See ToCreateRequestProto for content
// semantics.
func (skgsa *SSHKeyGroupSiteAssociation) ToUpdateRequestProto(content *corev1.TenantKeysetContent) *corev1.UpdateTenantKeysetRequest {
	tk := skgsa.ToProto(content)
	return &corev1.UpdateTenantKeysetRequest{
		KeysetIdentifier: tk.KeysetIdentifier,
		KeysetContent:    tk.KeysetContent,
		Version:          tk.Version,
	}
}

// ToDeletionRequestProto builds the workflow request that asks a Site
// to delete this Tenant Keyset.
func (skgsa *SSHKeyGroupSiteAssociation) ToDeletionRequestProto() *corev1.DeleteTenantKeysetRequest {
	return &corev1.DeleteTenantKeysetRequest{
		KeysetIdentifier: skgsa.ToKeysetIdentifierProto(),
	}
}

var _ bun.BeforeAppendModelHook = (*SSHKeyGroupSiteAssociation)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (skgsa *SSHKeyGroupSiteAssociation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		skgsa.Created = db.GetCurTime()
		skgsa.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		skgsa.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SSHKeyGroupSiteAssociation)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (skgsa *SSHKeyGroupSiteAssociation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("sshkey_group_id") REFERENCES "sshkey_group" ("id")`)
	return nil
}

// SSHKeyGroupSiteAssociationDAO is an interface for interacting with the SSHKeyGroupSiteAssociation model
type SSHKeyGroupSiteAssociationDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SSHKeyGroupSiteAssociationCreateInput) (*SSHKeyGroupSiteAssociation, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SSHKeyGroupSiteAssociation, error)
	//
	GetBySSHKeyGroupIDAndSiteID(ctx context.Context, tx *db.Tx, sshKeyGroupID uuid.UUID, siteID uuid.UUID, includeRelations []string) (*SSHKeyGroupSiteAssociation, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) ([]SSHKeyGroupSiteAssociation, int, error)
	//
	GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, ID uuid.UUID) (*SSHKeyGroupSiteAssociation, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SSHKeyGroupSiteAssociationUpdateInput) (*SSHKeyGroupSiteAssociation, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SSHKeyGroupSiteAssociationSQLDAO is an implementation of the SSHKeyGroupSiteAssociationDAO interface
type SSHKeyGroupSiteAssociationSQLDAO struct {
	dbSession *db.Session
	SSHKeyGroupSiteAssociationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SSHKeyGroupSiteAssociation from the given parameters
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) Create(
	ctx context.Context, tx *db.Tx,
	input SSHKeyGroupSiteAssociationCreateInput,
) (_ *SSHKeyGroupSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.Create")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()
	}

	skgsa := &SSHKeyGroupSiteAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: input.SSHKeyGroupID,
		SiteID:        input.SiteID,
		Version:       input.Version,
		Status:        input.Status,
		CreatedBy:     input.CreatedBy,
	}

	_, err := db.GetIDB(tx, skgsasd.dbSession).NewInsert().Model(skgsa).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := skgsasd.GetByID(ctx, tx, skgsa.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a SSHKeyGroupSiteAssociation by ID
// returns db.ErrDoesNotExist error if the record is not found
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SSHKeyGroupSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.GetByID")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()

		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "id", id.String())
	}

	skgsa := &SSHKeyGroupSiteAssociation{}

	query := db.GetIDB(tx, skgsasd.dbSession).NewSelect().Model(skgsa).Where("skgsa.id = ?", id)

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

	return skgsa, nil
}

// GetBySSHKeyGroupIDAndSiteID returns a SSHKeyGroupSiteAssociation by SSHKeyGroupID and SiteID
// returns db.ErrDoesNotExist error if the record is not found
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) GetBySSHKeyGroupIDAndSiteID(ctx context.Context, tx *db.Tx, sshKeyGroupID uuid.UUID, siteID uuid.UUID, includeRelations []string) (_ *SSHKeyGroupSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.GetBySSHKeyGroupIDAndSiteID")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()

		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "ssh_key_group_id", sshKeyGroupID.String())
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "site_id", siteID.String())
	}

	skgsa := &SSHKeyGroupSiteAssociation{}

	query := db.GetIDB(tx, skgsasd.dbSession).NewSelect().Model(skgsa).Where("skgsa.sshkey_group_id = ?", sshKeyGroupID.String()).Where("skgsa.site_id = ?", siteID.String())

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

	return skgsa, nil
}

// GetAll returns all SSHKeyGroupSiteAssociation with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SSHKeyGroupSiteAssociationOrderByDefault in ascending order
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) (_ []SSHKeyGroupSiteAssociation, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.GetAll")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()
	}

	skgsas := []SSHKeyGroupSiteAssociation{}

	query := db.GetIDB(tx, skgsasd.dbSession).NewSelect().Model(&skgsas)
	if filter.SSHKeyGroupIDs != nil {
		query = query.Where("skgsa.sshkey_group_id IN (?)", bun.In(filter.SSHKeyGroupIDs))
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "sshkey_group_id", filter.SSHKeyGroupIDs)
	}
	if filter.SiteID != nil {
		query = query.Where("skgsa.site_id = ?", *filter.SiteID)
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "site_id", filter.SiteID.String())
	}
	if filter.Version != nil {
		query = query.Where("skgsa.version = ?", filter.Version)
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "version", *filter.Version)
	}
	if filter.Status != nil {
		query = query.Where("skgsa.status = ?", *filter.Status)
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "status", *filter.Status)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SSHKeyGroupSiteAssociationOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SSHKeyGroupSiteAssociationOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return skgsas, paginator.Total, nil
}

// GenerateAndUpdateVersion is a utility function to generate latest version and update the SSHKeyGroupSiteAssociation
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) GenerateAndUpdateVersion(ctx context.Context, tx *db.Tx, id uuid.UUID) (*SSHKeyGroupSiteAssociation, error) {
	// Retrieve SSH Key Group Association details for calculating hash version based on SSHKeyGroupSiteAssociation ID
	dbskgsa, err := skgsasd.GetByID(ctx, tx, id, nil)
	if err != nil {
		return nil, err
	}

	// Initial has contains SSHKeyGroupID
	hash := sha1.New()
	hash.Write([]byte(dbskgsa.SSHKeyGroupID.String()))

	// Retrieve SSH Key Association details for calculating hash version based on SSH Key ID
	skaDAO := NewSSHKeyAssociationDAO(skgsasd.dbSession)
	dbska, _, err := skaDAO.GetAll(ctx, tx, SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{dbskgsa.SSHKeyGroupID}}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit),
		OrderBy: &paginator.OrderBy{
			Field: "created",
			Order: paginator.OrderAscending,
		},
	}, nil)
	if err != nil {
		return nil, err
	}

	// Update hash based on SSH Key ID
	for _, ska := range dbska {
		hash.Write([]byte(ska.SSHKeyID.String()))
	}

	version := hex.EncodeToString(hash.Sum(nil))

	// Update SSHKeyGroupSiteAssociation with new version
	uskgsa, err := skgsasd.Update(ctx, tx, SSHKeyGroupSiteAssociationUpdateInput{
		ID:      id,
		Version: &version,
	})
	if err != nil {
		return nil, err
	}

	return uskgsa, nil
}

// Update updates specified fields of an existing SSHKeyGroupSiteAssociation
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) Update(
	ctx context.Context, tx *db.Tx,
	input SSHKeyGroupSiteAssociationUpdateInput,
) (_ *SSHKeyGroupSiteAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.Update")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "id", input.ID.String())
	}

	skgsa := &SSHKeyGroupSiteAssociation{
		ID: input.ID,
	}

	updatedFields := []string{}

	if input.SSHKeyGroupID != nil {
		skgsa.SSHKeyGroupID = *input.SSHKeyGroupID
		updatedFields = append(updatedFields, "sshkey_group_id")
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "sshkey_group_id", input.SSHKeyGroupID.String())
	}
	if input.SiteID != nil {
		skgsa.SiteID = *input.SiteID
		updatedFields = append(updatedFields, "site_id")
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "site_id", input.SiteID.String())
	}
	if input.Version != nil {
		skgsa.Version = input.Version
		updatedFields = append(updatedFields, "version")
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "version", *input.Version)
	}
	if input.Status != nil {
		skgsa.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		skgsa.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, skgsasd.dbSession).NewUpdate().Model(skgsa).Column(updatedFields...).Where("skgsa.id = ?", input.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := skgsasd.GetByID(ctx, tx, skgsa.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an SSHKeyGroupSiteAssociation by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (skgsasd SSHKeyGroupSiteAssociationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupSiteAssociationDAOSpan := skgsasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupSiteAssociationDAO.Delete")
	if SSHKeyGroupSiteAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupSiteAssociationDAOSpan.EndWith(retErr)
		}()
		skgsasd.tracerSpan.SetAttribute(SSHKeyGroupSiteAssociationDAOSpan, "id", id.String())
	}

	skgsa := &SSHKeyGroupSiteAssociation{
		ID: id,
	}

	_, err := db.GetIDB(tx, skgsasd.dbSession).NewDelete().Model(skgsa).Where("skgsa.id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSSHKeyGroupSiteAssociationDAO returns a new SSHKeyGroupSiteAssociationDAO
func NewSSHKeyGroupSiteAssociationDAO(dbSession *db.Session) SSHKeyGroupSiteAssociationDAO {
	return &SSHKeyGroupSiteAssociationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
