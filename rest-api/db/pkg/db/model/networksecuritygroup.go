// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"slices"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/uptrace/bun"
	"google.golang.org/protobuf/encoding/protojson"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const NetworkSecurityGroupInitialVersion = "V0-T0"

const (
	// NetworkSecurityGroupStatusPending status is pending
	NetworkSecurityGroupStatusPending = "Pending"
	// NetworkSecurityGroupStatusProvisioning status is provisioning
	NetworkSecurityGroupStatusProvisioning = "Provisioning"
	// NetworkSecurityGroupStatusReady status is ready
	NetworkSecurityGroupStatusReady = "Ready"
	// NetworkSecurityGroupStatusDeleting indicates that the security group is being deleted
	NetworkSecurityGroupStatusDeleting = "Deleting"
	// NetworkSecurityGroupStatusError status is error
	NetworkSecurityGroupStatusError = "Error"
	// NetworkSecurityGroupRelationName is the relation name for the NetworkSecurityGroup model
	NetworkSecurityGroupRelationName = "NetworkSecurityGroup"

	// NetworkSecurityGroupOrderByDefault default field to be used for ordering when none specified
	NetworkSecurityGroupOrderByDefault = "created"
)

var (
	// NetworkSecurityGroupOrderByFields is a list of valid order by fields for the NetworkSecurityGroup model
	NetworkSecurityGroupOrderByFields = []string{"name", "status", "created", "updated"}
	// NetworkSecurityGroupRelatedEntities is a list of valid relation by fields for the NetworkSecurityGroup model
	NetworkSecurityGroupRelatedEntities = map[string]bool{
		SiteRelationName:   true,
		TenantRelationName: true,
	}
	// NetworkSecurityGroupStatusMap is a list of valid status for the NetworkSecurityGroup model
	NetworkSecurityGroupStatusMap = map[string]bool{
		NetworkSecurityGroupStatusPending:      true,
		NetworkSecurityGroupStatusProvisioning: true,
		NetworkSecurityGroupStatusDeleting:     true,
		NetworkSecurityGroupStatusReady:        true,
		NetworkSecurityGroupStatusError:        true,
	}
)

// NetworkSecurityGroup is used to create a firewall for instances
type NetworkSecurityGroup struct {
	bun.BaseModel `bun:"table:network_security_group,alias:nsg"`

	ID             string                      `bun:"id,pk"`
	Name           string                      `bun:"name,notnull"`
	Description    *string                     `bun:"description"`
	SiteID         uuid.UUID                   `bun:"site_id,type:uuid,notnull"`
	Site           *Site                       `bun:"rel:belongs-to,join:site_id=id"`
	TenantOrg      string                      `bun:"tenant_org,notnull"`
	TenantID       uuid.UUID                   `bun:"tenant_id,type:uuid,notnull"`
	Tenant         *Tenant                     `bun:"rel:belongs-to,join:tenant_id=id"`
	Status         string                      `bun:"status,notnull"`
	Created        time.Time                   `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated        time.Time                   `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted        *time.Time                  `bun:"deleted,soft_delete"`
	CreatedBy      uuid.UUID                   `bun:"type:uuid,notnull"`
	UpdatedBy      uuid.UUID                   `bun:"type:uuid,notnull"`
	Version        string                      `bun:"version"`
	Labels         Labels                      `bun:"labels,type:jsonb"`
	StatefulEgress bool                        `bun:"stateful_egress,notnull"`
	Rules          []*NetworkSecurityGroupRule `bun:"rules,type:jsonb"`
}

func (s *NetworkSecurityGroup) GetRulesAsProtoRefs() []*corev1.NetworkSecurityGroupRuleAttributes {
	if s.Rules == nil {
		return nil
	}

	// Convert our list of wrappers to a list of proto messages
	rules := make([]*corev1.NetworkSecurityGroupRuleAttributes, len(s.Rules))
	for i, rule := range s.Rules {
		if rule == nil {
			rules[i] = nil
		} else {
			rules[i] = rule.NetworkSecurityGroupRuleAttributes
		}
	}

	return rules
}

// toMetadataProto builds a workflow Metadata proto from the
// NetworkSecurityGroup's Name, Description, and Labels. Description
// defaults to the empty string when the receiver's pointer is nil to
// match the previous inline handler behaviour. Labels conversion is
// delegated to the typed `Labels.ToProto()` helper per the
// "Named types own their proto behavior" convention.
func (s *NetworkSecurityGroup) toMetadataProto() *corev1.Metadata {
	md := &corev1.Metadata{
		Name:        s.Name,
		Description: "",
		Labels:      s.Labels.ToProto(),
	}
	if s.Description != nil {
		md.Description = *s.Description
	}
	return md
}

// ToProto converts this NetworkSecurityGroup into its workflow proto
// representation. Used as the canonical entity-to-proto conversion;
// request-shape protos (create / update) are produced by `ToProto`
// methods on the corresponding API request types in
// api/pkg/api/model/networksecuritygroup.go.
//
// The proto's Attributes carry the rule list rebuilt from the embedded
// proto attributes on each persisted rule wrapper.
func (s *NetworkSecurityGroup) ToProto() *corev1.NetworkSecurityGroup {
	return &corev1.NetworkSecurityGroup{
		Id:                   s.ID,
		TenantOrganizationId: s.TenantOrg,
		Metadata:             s.toMetadataProto(),
		Version:              s.Version,
		Attributes: &corev1.NetworkSecurityGroupAttributes{
			StatefulEgress: s.StatefulEgress,
			Rules:          s.GetRulesAsProtoRefs(),
		},
	}
}

// FromProto populates this NetworkSecurityGroup from its workflow proto
// representation. A nil proto is a no-op. This is the inverse of
// `ToProto` and exists for convention symmetry — currently no code
// path on the cloud side reconstructs a full NSG entity from a
// `corev1.NetworkSecurityGroup` (the site is the destination, not the
// source), but the method is provided so future reconciliation flows
// have a single canonical entry point.
//
// Field-level contract:
//   - `s.ID` is preserved when `proto.Id` is empty.
//   - `Name` is sourced from `proto.Metadata.Name` when set, falling
//     back to leaving the receiver's name untouched if the proto has
//     no metadata.
//   - Description / Labels are reset from `proto.Metadata` so the
//     receiver ends up as a clean replacement rather than a partial
//     merge.
//   - Rule attributes are rewrapped from `proto.Attributes.Rules`;
//     a nil Attributes leaves the rule list nil.
func (s *NetworkSecurityGroup) FromProto(proto *corev1.NetworkSecurityGroup) {
	if proto == nil {
		return
	}
	if proto.Id != "" {
		s.ID = proto.Id
	}
	s.TenantOrg = proto.TenantOrganizationId
	if proto.Version != "" {
		s.Version = proto.Version
	}
	// Reset metadata-derived fields up front so the `clean reset rather
	// than a partial merge` contract holds when proto.Metadata is nil
	// or omits a field.
	s.Name = ""
	s.Description = nil
	s.Labels = nil
	if proto.Metadata != nil {
		if proto.Metadata.Name != "" {
			s.Name = proto.Metadata.Name
		}
		if proto.Metadata.Description != "" {
			desc := proto.Metadata.Description
			s.Description = &desc
		}
		s.Labels.FromProto(proto.Metadata.GetLabels())
	}
	// Attributes fields are reset before the conditional populate so the
	// "clean replacement" contract holds even when proto.Attributes is nil
	// (otherwise StatefulEgress / Rules would silently keep stale values).
	s.StatefulEgress = false
	s.Rules = nil
	if proto.Attributes != nil {
		s.StatefulEgress = proto.Attributes.StatefulEgress
		if proto.Attributes.Rules != nil {
			rules := make([]*NetworkSecurityGroupRule, len(proto.Attributes.Rules))
			for i, r := range proto.Attributes.Rules {
				if r == nil {
					rules[i] = nil
				} else {
					rules[i] = &NetworkSecurityGroupRule{NetworkSecurityGroupRuleAttributes: r}
				}
			}
			s.Rules = rules
		} else {
			s.Rules = nil
		}
	}
}

// ToDeletionRequestProto builds the workflow request that asks a Site
// to delete this NetworkSecurityGroup.
func (s *NetworkSecurityGroup) ToDeletionRequestProto() *corev1.DeleteNetworkSecurityGroupRequest {
	return &corev1.DeleteNetworkSecurityGroupRequest{
		Id:                   s.ID,
		TenantOrganizationId: s.TenantOrg,
	}
}

// A light wrapper around the protobuf so
// that we can implement our own marshal/unmarshal
// that understands how to work with protobuf messages
type NetworkSecurityGroupRule struct {
	*corev1.NetworkSecurityGroupRuleAttributes
}

func (s *NetworkSecurityGroupRule) UnmarshalJSON(b []byte) error {
	if s.NetworkSecurityGroupRuleAttributes == nil {
		s.NetworkSecurityGroupRuleAttributes = &corev1.NetworkSecurityGroupRuleAttributes{}
	}

	// protoJsonUnmarshalOptions is set to ignore unknown fields.
	// This means a record created on site that uses a new feature
	// won't break things in cloud,
	// BUT it also means that a user could
	// create something on site and then update it in cloud without realizing
	// that the new property for the new feature isn't in the cloud data.
	// If they then save the change, the record on site would lose the detail.
	_ = protoJsonUnmarshalOptions.Unmarshal(b, s)

	return nil
}

func (s *NetworkSecurityGroupRule) MarshalJSON() ([]byte, error) {
	return protojson.Marshal(s)
}

// A light wrapper around the protobuf so
// that we can implement our own marshal/unmarshal
// that understands how to work with protobuf messages
type NetworkSecurityGroupPropagationDetails struct {
	FriendlyStatus string `json:"friendlyStatus"`
	*corev1.NetworkSecurityGroupPropagationObjectStatus
}

func (s *NetworkSecurityGroupPropagationDetails) UnmarshalJSON(b []byte) error {
	if s.NetworkSecurityGroupPropagationObjectStatus == nil {
		s.NetworkSecurityGroupPropagationObjectStatus = &corev1.NetworkSecurityGroupPropagationObjectStatus{}
	}

	return protoJsonUnmarshalOptions.Unmarshal(b, s)
}

func (s *NetworkSecurityGroupPropagationDetails) MarshalJSON() ([]byte, error) {
	return protojson.Marshal(s)
}

// Equal reports whether two `*NetworkSecurityGroupPropagationDetails`
// wrappers carry the same wire-relevant state. Used by the cloud-sync
// activity to skip no-op DAO updates when the cloud copy already
// matches what the Site reported. Treats two nil receivers as equal.
// Declared as a method (per the Go convention used by `time.Time.Equal`,
// `cmp.Equal`, etc.) so callers can write `pd.Equal(other)` instead of
// reaching for a free helper.
func (s *NetworkSecurityGroupPropagationDetails) Equal(other *NetworkSecurityGroupPropagationDetails) bool {
	if s == nil || other == nil {
		return s == other
	}
	// `Details` is the only pointer-typed field we compare; inline the
	// nil-aware check so this method doesn't depend on the workflow util
	// layer.
	detailsEqual := (s.Details == nil) == (other.Details == nil) &&
		(s.Details == nil || *s.Details == *other.Details)
	return s.Status.Number() == other.Status.Number() &&
		detailsEqual &&
		slices.Equal(s.UnpropagatedInstanceIds, other.UnpropagatedInstanceIds) &&
		slices.Equal(s.RelatedInstanceIds, other.RelatedInstanceIds)
}

// NetworkSecurityGroupCreateInput input parameters for Create method
type NetworkSecurityGroupCreateInput struct {
	NetworkSecurityGroupID *string
	Name                   string
	Description            *string
	SiteID                 uuid.UUID
	TenantID               uuid.UUID
	TenantOrg              string
	Version                *string
	StatefulEgress         bool
	Rules                  []*NetworkSecurityGroupRule
	Labels                 map[string]string
	Status                 string
	CreatedByID            uuid.UUID
}

// NetworkSecurityGroupUpdateInput input parameters for Update method
type NetworkSecurityGroupUpdateInput struct {
	NetworkSecurityGroupID string
	Name                   *string
	Description            *string
	Version                *string
	StatefulEgress         *bool
	Rules                  []*NetworkSecurityGroupRule
	Labels                 map[string]string
	Status                 *string
	UpdatedByID            uuid.UUID
}

// NetworkSecurityGroupClearInput input parameters for Clear method
type NetworkSecurityGroupClearInput struct {
	NetworkSecurityGroupID string
	Description            bool
	Labels                 bool
	Rules                  bool
	UpdatedByID            uuid.UUID
}

// NetworkSecurityGroupFilterInput input parameters for Filter method
type NetworkSecurityGroupFilterInput struct {
	Name                    *string
	NetworkSecurityGroupIDs []string
	TenantOrgs              []string
	TenantIDs               []uuid.UUID
	SiteIDs                 []uuid.UUID
	Statuses                []string
	SearchQuery             *string
}

// NetworkSecurityGroupDeleteInput input parameters for Delete method
type NetworkSecurityGroupDeleteInput struct {
	NetworkSecurityGroupID string
	UpdatedByID            uuid.UUID
}

var _ bun.BeforeAppendModelHook = (*NetworkSecurityGroup)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (nsg *NetworkSecurityGroup) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		nsg.Created = db.GetCurTime()
		nsg.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		nsg.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*NetworkSecurityGroup)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (it *NetworkSecurityGroup) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)

	return nil
}

// NetworkSecurityGroupDAO is an interface for interacting with the NetworkSecurityGroup model
type NetworkSecurityGroupDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupCreateInput) (*NetworkSecurityGroup, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id string, includeRelations []string) (*NetworkSecurityGroup, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter NetworkSecurityGroupFilterInput, page paginator.PageInput, includeRelations []string) ([]NetworkSecurityGroup, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupUpdateInput) (*NetworkSecurityGroup, error)
	//
	Delete(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupDeleteInput) error
}

// NetworkSecurityGroupSQLDAO is an implementation of the NetworkSecurityGroupDAO interface
type NetworkSecurityGroupSQLDAO struct {
	dbSession *db.Session
	NetworkSecurityGroupDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new NetworkSecurityGroup from the given parameters
// The returned NetworkSecurityGroup will not have any related structs (InfrastructureProvider/Site) filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (sgsd NetworkSecurityGroupSQLDAO) Create(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupCreateInput) (_ *NetworkSecurityGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, networkSecurityGroupDAOSpan := sgsd.tracerSpan.CreateChildInCurrentContext(ctx, "NetworkSecurityGroupDAO.Create")
	if networkSecurityGroupDAOSpan != nil {
		defer func() {
			networkSecurityGroupDAOSpan.EndWith(retErr)
		}()

		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "name", input.Name)
	}

	for _, rule := range input.Rules {
		if rule == nil {
			return nil, errors.New("found nil rule in Rules")
		}
	}

	id := uuid.NewString()

	if input.NetworkSecurityGroupID != nil {
		id = *input.NetworkSecurityGroupID
	}

	version := NetworkSecurityGroupInitialVersion
	if input.Version != nil {
		version = *input.Version
	}

	nsg := &NetworkSecurityGroup{
		ID:             id,
		Name:           input.Name,
		Description:    input.Description,
		SiteID:         input.SiteID,
		TenantOrg:      input.TenantOrg,
		TenantID:       input.TenantID,
		Status:         input.Status,
		Labels:         input.Labels,
		StatefulEgress: input.StatefulEgress,
		Rules:          input.Rules,
		CreatedBy:      input.CreatedByID,
		UpdatedBy:      input.CreatedByID,
		Version:        version,
	}

	_, err := db.GetIDB(tx, sgsd.dbSession).NewInsert().Model(nsg).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := sgsd.GetByID(ctx, tx, nsg.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a NetworkSecurityGroup by ID
// Returns db.ErrDoesNotExist error if the record is not found
func (sgsd NetworkSecurityGroupSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id string, includeRelations []string) (_ *NetworkSecurityGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, networkSecurityGroupDAOSpan := sgsd.tracerSpan.CreateChildInCurrentContext(ctx, "NetworkSecurityGroupDAO.GetByID")
	if networkSecurityGroupDAOSpan != nil {
		defer func() {
			networkSecurityGroupDAOSpan.EndWith(retErr)
		}()

		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "id", id)
	}

	it := &NetworkSecurityGroup{}

	query := db.GetIDB(tx, sgsd.dbSession).NewSelect().Model(it).Where("nsg.id = ?", id)

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

	return it, nil
}

// GetAll returns all NetworkSecurityGroups with various optional filters
// If no records found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified
// in NetworkSecurityGroupOrderByDefault in ascending order
func (sgsd NetworkSecurityGroupSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter NetworkSecurityGroupFilterInput, page paginator.PageInput, includeRelations []string) (_ []NetworkSecurityGroup, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, networkSecurityGroupDAOSpan := sgsd.tracerSpan.CreateChildInCurrentContext(ctx, "NetworkSecurityGroupDAO.GetAll")
	if networkSecurityGroupDAOSpan != nil {
		defer func() {
			networkSecurityGroupDAOSpan.EndWith(retErr)
		}()
	}

	sgs := []NetworkSecurityGroup{}

	var query *bun.SelectQuery

	query = db.GetIDB(tx, sgsd.dbSession).NewSelect().Model(&sgs)

	if filter.Name != nil {
		query = query.Where("nsg.name = ?", *filter.Name)
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "name", filter.Name)
	}

	// Single-item lists with IN are optimized by the query planner
	// to an =

	if filter.TenantOrgs != nil {
		query = query.Where("nsg.tenant_org IN (?)", bun.In(filter.TenantOrgs))
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "tenant_organization_ids", filter.TenantOrgs)
	}
	if filter.TenantIDs != nil {
		query = query.Where("nsg.tenant_id IN (?)", bun.In(filter.TenantIDs))
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "tenant_organization_ids", filter.TenantIDs)
	}

	if filter.SiteIDs != nil {
		query = query.Where("nsg.site_id IN (?)", bun.In(filter.SiteIDs))
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "site_ids", filter.SiteIDs)
	}

	if filter.NetworkSecurityGroupIDs != nil {
		query = query.Where("nsg.id IN (?)", bun.In(filter.NetworkSecurityGroupIDs))
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "network_security_group_ids", filter.NetworkSecurityGroupIDs)
	}

	if filter.Statuses != nil {
		query = query.Where("nsg.status IN (?)", bun.In(filter.Statuses))
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "tenant_organization_ids", filter.Statuses)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(nsg.name, ' ') || ' ' || coalesce(nsg.status, ' ') || ' ' || coalesce(nsg.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("nsg.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nsg.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nsg.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nsg.labels::text ILIKE ?", "%"+searchQuery+"%")
		})
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to ensure consistent ordering for pagination.
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(VpcOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, VpcOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return sgs, paginator.Total, nil
}

// Update updates specified fields of an existing NetworkSecurityGroup
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// Since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction.
func (sgsd NetworkSecurityGroupSQLDAO) Update(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupUpdateInput) (_ *NetworkSecurityGroup, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, networkSecurityGroupDAOSpan := sgsd.tracerSpan.CreateChildInCurrentContext(ctx, "NetworkSecurityGroupDAO.Update")
	if networkSecurityGroupDAOSpan != nil {
		defer func() {
			networkSecurityGroupDAOSpan.EndWith(retErr)
		}()

		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "id", input.NetworkSecurityGroupID)
	}

	for _, rule := range input.Rules {
		if rule == nil {
			return nil, errors.New("found nil rule in Rules")
		}
	}

	updatedFields := []string{}

	it := &NetworkSecurityGroup{
		ID: input.NetworkSecurityGroupID,
	}

	if input.Name != nil {
		it.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "name", *input.Name)
	}
	if input.Description != nil {
		it.Description = input.Description
		updatedFields = append(updatedFields, "description")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "description", *input.Description)
	}
	if input.Status != nil {
		it.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "status", *input.Status)
	}
	if input.Labels != nil {
		it.Labels = input.Labels
		updatedFields = append(updatedFields, "labels")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "labels", input.Labels)
	}
	if input.StatefulEgress != nil {
		it.StatefulEgress = *input.StatefulEgress
		updatedFields = append(updatedFields, "stateful_egress")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "stateful_egress", *input.StatefulEgress)
	}
	if input.Rules != nil {
		it.Rules = input.Rules
		updatedFields = append(updatedFields, "rules")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "rules", input.Rules)
	}
	if input.Version != nil {
		it.Version = *input.Version
		updatedFields = append(updatedFields, "version")
		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "version", input.Version)
	}

	it.UpdatedBy = input.UpdatedByID
	updatedFields = append(updatedFields, "updated_by")

	sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "updated_by", input.UpdatedByID)

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, sgsd.dbSession).NewUpdate().Model(it).Column(updatedFields...).Where("nsg.id = ?", input.NetworkSecurityGroupID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := sgsd.GetByID(ctx, tx, it.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an NetworkSecurityGroup
// If the object being deleted doesnt exist,
// error is not returned (idempotent delete)
func (sgsd NetworkSecurityGroupSQLDAO) Delete(ctx context.Context, tx *db.Tx, input NetworkSecurityGroupDeleteInput) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, networkSecurityGroupDAOSpan := sgsd.tracerSpan.CreateChildInCurrentContext(ctx, "NetworkSecurityGroupDAO.DeleteByID")
	if networkSecurityGroupDAOSpan != nil {
		defer func() {
			networkSecurityGroupDAOSpan.EndWith(retErr)
		}()

		sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "id", input.NetworkSecurityGroupID)
	}

	it := &NetworkSecurityGroup{
		ID: input.NetworkSecurityGroupID,
	}

	it.UpdatedBy = input.UpdatedByID
	sgsd.tracerSpan.SetAttribute(networkSecurityGroupDAOSpan, "updated_by", input.UpdatedByID)

	_, err := db.GetIDB(tx, sgsd.dbSession).NewDelete().Model(it).Where("id = ?", input.NetworkSecurityGroupID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewNetworkSecurityGroupDAO returns a new NetworkSecurityGroupDAO
func NewNetworkSecurityGroupDAO(dbSession *db.Session) NetworkSecurityGroupDAO {
	return &NetworkSecurityGroupSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
