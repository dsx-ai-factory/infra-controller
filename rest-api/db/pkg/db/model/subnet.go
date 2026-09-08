// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/netip"
	"strconv"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// SubnetStatusPending status is pending
	SubnetStatusPending = "Pending"
	// SubnetStatusProvisioning status is provisioning
	SubnetStatusProvisioning = "Provisioning"
	// SubnetStatusReady status is ready
	SubnetStatusReady = "Ready"
	// SubnetStatusError status is error
	SubnetStatusError = "Error"
	// SubnetStatusDeleting indicates that the subnet is being deleted
	SubnetStatusDeleting = "Deleting"
	// SubnetStatusDeleted indicates that the subnet has been deleted
	SubnetStatusDeleted = "Deleted"
	// SubnetStatusUnknown indicates that the subnet status is unknown
	SubnetStatusUnknown = "Unknown"

	// SubnetRelationName is the relation name for the Subnet model
	SubnetRelationName = "Subnet"

	// SubnetOrderByDefault default field to be used for ordering when none specified
	SubnetOrderByDefault = "created"
)

var (
	// SubnetOrderByFields is a list of valid order by fields for the Subnet model
	SubnetOrderByFields = []string{"name", "status", "created", "updated"}
	// SubnetRelatedEntities is a list of valid relation by fields for the Subnet model
	SubnetRelatedEntities = map[string]bool{
		SiteRelationName:      true,
		VpcRelationName:       true,
		TenantRelationName:    true,
		IPv4BlockRelationName: true,
		IPv6BlockRelationName: true,
	}
	// SubnetStatusMap is a list of valid status for the Subnet model
	SubnetStatusMap = map[string]bool{
		SubnetStatusPending:      true,
		SubnetStatusProvisioning: true,
		SubnetStatusReady:        true,
		SubnetStatusDeleting:     true,
		SubnetStatusDeleted:      true,
		SubnetStatusError:        true,
	}
	errSubnetNoIPv4Prefix = errors.New("subnet has no IPv4 prefix")
)

// Subnet is a network construct for bare-metal machines
type Subnet struct {
	bun.BaseModel `bun:"table:subnet,alias:su"`

	ID                         uuid.UUID  `bun:"type:uuid,pk"`
	Name                       string     `bun:"name,notnull"`
	Description                *string    `bun:"description"`
	Org                        string     `bun:"org,notnull"`
	SiteID                     uuid.UUID  `bun:"site_id,type:uuid,notnull"`
	Site                       *Site      `bun:"rel:belongs-to,join:site_id=id"`
	VpcID                      uuid.UUID  `bun:"vpc_id,type:uuid,notnull"`
	Vpc                        *Vpc       `bun:"rel:belongs-to,join:vpc_id=id"`
	DomainID                   *uuid.UUID `bun:"domain_id,type:uuid"`
	Domain                     *Domain    `bun:"rel:belongs-to,join:domain_id=id"`
	TenantID                   uuid.UUID  `bun:"tenant_id,type:uuid"`
	Tenant                     *Tenant    `bun:"rel:belongs-to,join:tenant_id=id"`
	ControllerNetworkSegmentID *uuid.UUID `bun:"controller_network_segment_id,type:uuid"`
	RoutingType                *string    `bun:"routing_type"`
	IPv4Prefix                 *string    `bun:"ipv4_prefix"`
	IPv4Gateway                *string    `bun:"ipv4_gateway"`
	IPv4BlockID                *uuid.UUID `bun:"ipv4_block_id,type:uuid"`
	IPv4Block                  *IPBlock   `bun:"rel:belongs-to,join:ipv4_block_id=id"`
	IPv6Prefix                 *string    `bun:"ipv6_prefix"`
	IPv6Gateway                *string    `bun:"ipv6_gateway"`
	IPv6BlockID                *uuid.UUID `bun:"ipv6_block_id,type:uuid"`
	IPv6Block                  *IPBlock   `bun:"rel:belongs-to,join:ipv6_block_id=id"`
	PrefixLength               int        `bun:"prefix_length,notnull"`
	MTU                        *int       `bun:"mtu"`
	Status                     string     `bun:"status,notnull"`
	IsMissingOnSite            bool       `bun:"is_missing_on_site,notnull"`
	Created                    time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                    time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                    *time.Time `bun:"deleted,soft_delete"`
	CreatedBy                  uuid.UUID  `bun:"type:uuid,notnull"`
}

// GetSiteID returns the Subnet ID to use when communicating with the
// Site: the controller-supplied ControllerNetworkSegmentID when present,
// otherwise the Subnet's own ID. The Site treats both as opaque
// identifiers.
func (s *Subnet) GetSiteID() *uuid.UUID {
	if s.ControllerNetworkSegmentID != nil {
		return s.ControllerNetworkSegmentID
	}
	return &s.ID
}

func (s *Subnet) toMetadataProto() *corev1.Metadata {
	md := &corev1.Metadata{
		Name:        s.Name,
		Description: "",
	}
	if s.Description != nil {
		md.Description = *s.Description
	}
	return md
}

// ToProto converts this Subnet into its workflow proto representation.
// Used as the canonical entity-to-proto conversion; request-shape protos
// (create) are produced by `ToProto` methods on the corresponding API
// request types in api/pkg/api/model/subnet.go.
//
// Desired configuration is emitted via the structured `config` field and
// identity metadata via `metadata`. The deprecated flat mirror fields are
// no longer populated.
//
// `Prefixes` is built from `IPv4Prefix` + `PrefixLength` when
// `IPv4Prefix` is set; otherwise the field is left nil. The Site-facing
// `ReserveFirst` count is a deployment policy that does not live on the
// entity, so this method emits prefixes with a zero `ReserveFirst` and
// the request-shape `ToProto` overlays the policy value.
func (s *Subnet) ToProto() *corev1.NetworkSegment {
	config := &corev1.NetworkSegmentConfig{
		VpcId: &corev1.VpcId{Value: s.VpcID.String()},
	}
	if s.DomainID != nil {
		config.SubdomainId = &corev1.DomainId{Value: s.DomainID.String()}
	}
	if s.MTU != nil {
		mtu := int32(*s.MTU)
		config.Mtu = &mtu
	}
	if s.IPv4Prefix != nil {
		config.Prefixes = []*corev1.NetworkPrefix{
			{
				Gateway: s.IPv4Gateway,
				Prefix:  fmt.Sprintf("%s/%d", *s.IPv4Prefix, s.PrefixLength),
			},
		}
	}

	return &corev1.NetworkSegment{
		Id:       &corev1.NetworkSegmentId{Value: s.GetSiteID().String()},
		Metadata: s.toMetadataProto(),
		Config:   config,
	}
}

// FromProto populates this Subnet from its workflow proto representation.
// A nil proto is a no-op. This is the inverse of `ToProto` and exists
// for convention symmetry — currently no code path on the cloud side
// reconstructs a full Subnet entity from a `corev1.NetworkSegment` (the
// site is the destination, not the source), but the method is provided
// so future reconciliation flows have a single canonical entry point.
//
// Field-level contract:
//   - `s.ID` is preserved on a missing or unparseable `proto.Id`,
//     because callers pre-validate the UUID before calling.
//   - `Name` is sourced from the structured `proto.Metadata.Name`.
//   - Desired-configuration fields (VpcID, DomainID, MTU, prefixes) are
//     read from the structured `config` only. Deprecated flat mirrors are
//     not consulted during entity reconstruction.
//   - Optional pointer fields (DomainID, MTU, IPv4Prefix, IPv4Gateway)
//     are cleared when the proto omits them OR when the proto value is
//     invalid (e.g. an unparseable UUID, an unparseable prefix). This
//     makes `FromProto` a clean reset rather than a partial merge.
func (s *Subnet) FromProto(proto *corev1.NetworkSegment) {
	if proto == nil {
		return
	}
	if proto.Id != nil {
		if id, err := uuid.Parse(proto.Id.Value); err == nil {
			s.ID = id
		}
	}
	s.Name = ""
	s.Description = nil
	if proto.Metadata != nil {
		if proto.Metadata.Name != "" {
			s.Name = proto.Metadata.Name
		}
		if proto.Metadata.Description != "" {
			s.Description = &proto.Metadata.Description
		}
	}

	cfg := proto.Config
	if cfg == nil {
		cfg = &corev1.NetworkSegmentConfig{}
	}
	if cfg.VpcId != nil {
		if id, err := uuid.Parse(cfg.VpcId.Value); err == nil {
			s.VpcID = id
		}
	}
	s.DomainID = nil
	if cfg.SubdomainId != nil {
		if id, err := uuid.Parse(cfg.SubdomainId.Value); err == nil {
			s.DomainID = &id
		}
	}
	s.MTU = nil
	if cfg.Mtu != nil {
		m := int(*cfg.Mtu)
		s.MTU = &m
	}
	s.IPv4Prefix = nil
	s.IPv4Gateway = nil
	s.PrefixLength = 0
	if len(cfg.Prefixes) > 0 && cfg.Prefixes[0] != nil {
		// The proto carries `<prefix>/<length>` as a single string; split
		// it back into the entity's two fields. A malformed value clears
		// both rather than leaving the receiver in a partial state.
		prefix, length, ok := splitNetworkPrefix(cfg.Prefixes[0].Prefix)
		if ok {
			s.IPv4Prefix = &prefix
			s.PrefixLength = length
			s.IPv4Gateway = cfg.Prefixes[0].Gateway
		}
	}
}

// subnetValidStatuses holds the `SubnetStatusMap` keys as `[]any` for
// reuse by `validation.In(...)` -- built once at init so `Validate`
// avoids the map iteration + slice allocation on every call.
var subnetValidStatuses = func() []any {
	out := make([]any, 0, len(SubnetStatusMap))
	for st := range SubnetStatusMap {
		out = append(out, st)
	}
	return out
}()

// Validate checks that the populated Subnet is wire-safe -- gates
// against half-states `FromProto` may leave behind on malformed input
// (notably nil `IPv4Prefix` after a failed prefix split) and mirrors
// the API-side rules at the entity boundary. Mirrors the
// MachineCapability / InfiniBandPartition pattern.
func (s *Subnet) Validate() error {
	return validation.ValidateStruct(s,
		validation.Field(&s.Name,
			validation.Required.Error("Subnet Name must be specified"),
			validation.Length(2, 256).Error("Subnet Name must be at least 2 characters and maximum 256 characters"),
			validation.By(validateSubnetNameWhitespace)),
		validation.Field(&s.Status,
			validation.Required.Error("Subnet Status must be specified"),
			validation.In(subnetValidStatuses...).Error(fmt.Sprintf("invalid Subnet Status: %q", s.Status))),
		validation.Field(&s.VpcID,
			validation.By(validateSubnetVpcIDRequired)),
		validation.Field(&s.IPv4Prefix,
			validation.Required.Error("Subnet IPv4Prefix must be specified")),
	)
}

// validateSubnetVpcIDRequired rejects a `uuid.Nil` VpcID. `FromProto`
// preserves the receiver's `VpcID` when the proto's value is missing or
// unparseable (the "required ID fields are preserved" rule), so a
// zero-initialised receiver could otherwise leak `uuid.Nil` past the
// entity-boundary gate.
func validateSubnetVpcIDRequired(value interface{}) error {
	id, ok := value.(uuid.UUID)
	if !ok || id == uuid.Nil {
		return errors.New("Subnet VpcID must be set")
	}
	return nil
}

// validateSubnetNameWhitespace rejects Names with leading or trailing
// whitespace, mirroring the API-side `util.ValidateNameCharacters`
// rule so the wire-bound DB-model gate matches the API one.
func validateSubnetNameWhitespace(value interface{}) error {
	s, ok := value.(string)
	if !ok {
		return errors.New("Subnet Name must be a string")
	}
	if strings.TrimSpace(s) != s {
		return errors.New("Subnet Name must not contain leading or trailing whitespace")
	}
	return nil
}

// splitNetworkPrefix splits a `<prefix>/<length>` string into its parts.
// Returns false when the value is missing the separator or the length
// is not a non-negative integer.
func splitNetworkPrefix(s string) (string, int, bool) {
	prefix, lengthStr, ok := strings.Cut(s, "/")
	if !ok || prefix == "" || lengthStr == "" {
		return "", 0, false
	}
	length, err := strconv.Atoi(lengthStr)
	if err != nil || length < 0 {
		return "", 0, false
	}
	return prefix, length, true
}

// SubnetCreateInput parameters for Create method
type SubnetCreateInput struct {
	SubnetID                   *uuid.UUID
	Name                       string
	Description                *string
	Org                        string
	SiteID                     uuid.UUID
	VpcID                      uuid.UUID
	DomainID                   *uuid.UUID
	TenantID                   uuid.UUID
	ControllerNetworkSegmentID *uuid.UUID
	RoutingType                *string
	IPv4Prefix                 *string
	IPv4Gateway                *string
	IPv4BlockID                *uuid.UUID
	IPv6Prefix                 *string
	IPv6Gateway                *string
	IPv6BlockID                *uuid.UUID
	PrefixLength               int
	Mtu                        *int
	Status                     string
	CreatedBy                  uuid.UUID
}

// SubnetUpdateInput parameters for Update method
type SubnetUpdateInput struct {
	SubnetId                   uuid.UUID
	Name                       *string
	Description                *string
	Org                        *string
	SiteID                     *uuid.UUID
	VpcID                      *uuid.UUID
	DomainID                   *uuid.UUID
	TenantID                   *uuid.UUID
	ControllerNetworkSegmentID *uuid.UUID
	IPv4Prefix                 *string
	IPv4Gateway                *string
	IPv4BlockID                *uuid.UUID
	IPv6Prefix                 *string
	IPv6Gateway                *string
	IPv6BlockID                *uuid.UUID
	PrefixLength               *int
	Mtu                        *int
	Status                     *string
	IsMissingOnSite            *bool
}

// SubnetClearInput parameters for Clear method
type SubnetClearInput struct {
	SubnetId                   uuid.UUID
	Description                bool
	DomainID                   bool
	ControllerNetworkSegmentID bool
	IPv4Prefix                 bool
	IPv4Gateway                bool
	IPv4BlockID                bool
	IPv6Prefix                 bool
	IPv6Gateway                bool
	IPv6BlockID                bool
	Mtu                        bool
	// Deleted clears the soft-delete timestamp (undelete).
	Deleted bool
}

// SubnetFilterInput input parameters for GetAll method
type SubnetFilterInput struct {
	SubnetIDs    []uuid.UUID
	Names        []string
	SiteIDs      []uuid.UUID
	VpcIDs       []uuid.UUID
	DomainIDs    []uuid.UUID
	TenantIDs    []uuid.UUID
	IPv4BlockIDs []uuid.UUID
	IPv6BlockIDs []uuid.UUID
	Statuses     []string
	SearchQuery  *string
	// IncludeDeleted returns soft-deleted rows in addition to active ones.
	IncludeDeleted bool
}

var _ bun.BeforeAppendModelHook = (*Subnet)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (su *Subnet) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		su.Created = db.GetCurTime()
		su.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		su.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Subnet)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (it *Subnet) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("vpc_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("domain_id") REFERENCES "domain" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("ipv4_block_id") REFERENCES "ip_block" ("id")`).
		ForeignKey(`("ipv6_block_id") REFERENCES "ip_block" ("id")`)
	return nil
}

// SubnetDAO is an interface for interacting with the Subnet model
type SubnetDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SubnetCreateInput) (*Subnet, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Subnet, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SubnetFilterInput, page paginator.PageInput, includeRelations []string) ([]Subnet, int, error)
	//
	GetCountByStatus(ctx context.Context, tx *db.Tx, tenantID *uuid.UUID, vpcID *uuid.UUID) (map[string]int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SubnetUpdateInput) (*Subnet, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input SubnetClearInput) (*Subnet, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	// GetPrefixUsage returns IPv4 interface usage per subnet ID (in-memory IPAM simulation).
	// Subnets without an IPv4 prefix are omitted from the result map.
	GetPrefixUsage(ctx context.Context, tx *db.Tx, subnets ...*Subnet) (map[uuid.UUID]*cipam.Usage, error)
}

// SubnetSQLDAO is an implementation of the SubnetDAO interface
type SubnetSQLDAO struct {
	dbSession *db.Session
	SubnetDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Subnet from the given parameters
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (ssd SubnetSQLDAO) Create(ctx context.Context, tx *db.Tx, input SubnetCreateInput) (*Subnet, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.Create")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()

		ssd.tracerSpan.SetAttribute(sbDAOSpan, "name", input.Name)
	}

	id := input.SubnetID
	if id == nil {
		id = cutil.GetPtr(uuid.New())
	}

	s := &Subnet{
		ID:                         *id,
		Name:                       input.Name,
		Description:                input.Description,
		Org:                        input.Org,
		SiteID:                     input.SiteID,
		VpcID:                      input.VpcID,
		DomainID:                   input.DomainID,
		TenantID:                   input.TenantID,
		ControllerNetworkSegmentID: input.ControllerNetworkSegmentID,
		RoutingType:                input.RoutingType,
		IPv4Prefix:                 input.IPv4Prefix,
		IPv4Gateway:                input.IPv4Gateway,
		IPv4BlockID:                input.IPv4BlockID,
		IPv6Prefix:                 input.IPv6Prefix,
		IPv6Gateway:                input.IPv6Gateway,
		IPv6BlockID:                input.IPv6BlockID,
		PrefixLength:               input.PrefixLength,
		Status:                     input.Status,
		IsMissingOnSite:            false,
		CreatedBy:                  input.CreatedBy,
		MTU:                        input.Mtu,
	}

	_, err := db.GetIDB(tx, ssd.dbSession).NewInsert().Model(s).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := ssd.GetByID(ctx, tx, s.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a Subnet by ID
// includeRelation can be a subset of Vpc, Domain, Tenant
// returns db.ErrDoesNotExist error if the record is not found
func (ssd SubnetSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Subnet, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.GetByID")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()

		ssd.tracerSpan.SetAttribute(sbDAOSpan, "id", id.String())
	}

	s := &Subnet{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(s).Where("su.id = ?", id)

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

	return s, nil
}

// GetCountByStatus returns count of Subnets for given status
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned map is 0
func (ssd SubnetSQLDAO) GetCountByStatus(ctx context.Context, tx *db.Tx, tenantID *uuid.UUID, vpcID *uuid.UUID) (map[string]int, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.GetCountByStatus")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()
	}

	s := &Subnet{}
	var statusQueryResults []map[string]interface{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(s)
	if tenantID != nil {
		query = query.Where("su.tenant_id = ?", *tenantID)
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "tenant_id", tenantID.String())
	}
	if vpcID != nil {
		query = query.Where("su.vpc_id = ?", *vpcID)
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "vpc_id", vpcID.String())
	}

	err := query.Column("su.status").ColumnExpr("COUNT(*) AS total_count").GroupExpr("su.status").Scan(ctx, &statusQueryResults)
	if err != nil {
		return nil, err
	}

	// create results map by holding key as status value with total count
	results := map[string]int{
		"total":                  0,
		SubnetStatusDeleting:     0,
		SubnetStatusError:        0,
		SubnetStatusProvisioning: 0,
		SubnetStatusPending:      0,
		SubnetStatusReady:        0,
	}

	if len(statusQueryResults) > 0 {
		for _, statusMap := range statusQueryResults {
			results[statusMap["status"].(string)] = int(statusMap["total_count"].(int64))
			results["total"] += int(statusMap["total_count"].(int64))
		}
	}
	return results, nil
}

// GetAll returns all Subnets filtering by Vpc, Domain, Tenant
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SubnetOrderByDefault in ascending order
func (ssd SubnetSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SubnetFilterInput, page paginator.PageInput, includeRelations []string) ([]Subnet, int, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.GetAll")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()
	}

	ss := []Subnet{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(&ss)
	// Soft-deleted rows are excluded by default.
	if filter.IncludeDeleted {
		query = query.WhereAllWithDeleted()
	}
	if filter.SubnetIDs != nil {
		query = query.Where("su.id IN (?)", bun.In(filter.SubnetIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "subnet_ids", filter.SubnetIDs)
	}
	if filter.Names != nil {
		query = query.Where("su.name IN (?)", bun.In(filter.Names))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "name", filter.Names)
	}
	if filter.SiteIDs != nil {
		query = query.Where("su.site_id IN (?)", bun.In(filter.SiteIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.VpcIDs != nil {
		query = query.Where("su.vpc_id IN (?)", bun.In(filter.VpcIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "vpc_id", filter.VpcIDs)
	}
	if filter.DomainIDs != nil {
		query = query.Where("su.domain_id IN (?)", bun.In(filter.DomainIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "domain_id", filter.DomainIDs)
	}
	if filter.TenantIDs != nil {
		query = query.Where("su.tenant_id IN (?)", bun.In(filter.TenantIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "tenant_id", filter.TenantIDs)
	}
	if filter.IPv4BlockIDs != nil {
		query = query.Where("su.ipv4_block_id IN (?)", bun.In(filter.IPv4BlockIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv4_block_id", filter.IPv4BlockIDs)
	}
	if filter.IPv6BlockIDs != nil {
		query = query.Where("su.ipv6_block_id IN (?)", bun.In(filter.IPv6BlockIDs))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv6_block_id", filter.IPv6BlockIDs)
	}
	if filter.Statuses != nil {
		query = query.Where("su.status IN (?)", bun.In(filter.Statuses))
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "status", filter.Statuses)
	}
	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(su.name, ' ') || ' ' || coalesce(su.description, ' ') || ' ' || coalesce(su.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("su.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("su.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("su.status ILIKE ?", "%"+searchQuery+"%")
		})
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	orderBy := page.OrderBy
	if page.OrderBy == nil {
		orderBy = paginator.NewDefaultOrderBy(SubnetOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, orderBy, SubnetOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return ss, paginator.Total, nil
}

// Update updates specified fields of an existing Subnet
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (ssd SubnetSQLDAO) Update(ctx context.Context, tx *db.Tx, input SubnetUpdateInput) (*Subnet, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.Update")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()

		ssd.tracerSpan.SetAttribute(sbDAOSpan, "id", input.SubnetId.String())
	}

	s := &Subnet{
		ID: input.SubnetId,
	}
	updatedFields := []string{}

	if input.Name != nil {
		s.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "name", *input.Name)
	}
	if input.Description != nil {
		s.Description = input.Description
		updatedFields = append(updatedFields, "description")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "description", *input.Description)
	}
	if input.Org != nil {
		s.Org = *input.Org
		updatedFields = append(updatedFields, "org")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "org", *input.Org)
	}
	if input.SiteID != nil {
		s.SiteID = *input.SiteID
		updatedFields = append(updatedFields, "site_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "site_id", input.SiteID.String())
	}
	if input.VpcID != nil {
		s.VpcID = *input.VpcID
		updatedFields = append(updatedFields, "vpc_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "vpc_id", input.VpcID.String())
	}
	if input.DomainID != nil {
		s.DomainID = input.DomainID
		updatedFields = append(updatedFields, "domain_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "domain_id", input.DomainID.String())
	}
	if input.TenantID != nil {
		s.TenantID = *input.TenantID
		updatedFields = append(updatedFields, "tenant_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "tenant_id", input.TenantID.String())
	}
	if input.ControllerNetworkSegmentID != nil {
		s.ControllerNetworkSegmentID = input.ControllerNetworkSegmentID
		updatedFields = append(updatedFields, "controller_network_segment_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "controller_network_segment_id", input.ControllerNetworkSegmentID.String())
	}
	if input.IPv4Prefix != nil {
		s.IPv4Prefix = input.IPv4Prefix
		updatedFields = append(updatedFields, "ipv4_prefix")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv4_prefix", *input.IPv4Prefix)
	}
	if input.IPv4Gateway != nil {
		s.IPv4Gateway = input.IPv4Gateway
		updatedFields = append(updatedFields, "ipv4_gateway")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv4_gateway", *input.IPv4Gateway)
	}
	if input.IPv4BlockID != nil {
		s.IPv4BlockID = input.IPv4BlockID
		updatedFields = append(updatedFields, "ipv4_block_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv4_block_id", input.IPv4BlockID.String())
	}
	if input.IPv6Prefix != nil {
		s.IPv6Prefix = input.IPv6Prefix
		updatedFields = append(updatedFields, "ipv6_prefix")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv6_prefix", *input.IPv6Prefix)
	}
	if input.IPv6Gateway != nil {
		s.IPv6Gateway = input.IPv6Gateway
		updatedFields = append(updatedFields, "ipv6_gateway")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv6_gateway", *input.IPv6Gateway)
	}
	if input.IPv6BlockID != nil {
		s.IPv6BlockID = input.IPv6BlockID
		updatedFields = append(updatedFields, "ipv6_block_id")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "ipv6_block_id", input.IPv6BlockID.String())
	}
	if input.PrefixLength != nil {
		s.PrefixLength = *input.PrefixLength
		updatedFields = append(updatedFields, "prefix_length")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "prefix_length", *input.PrefixLength)
	}
	if input.Status != nil {
		s.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		s.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}
	if input.Mtu != nil {
		s.MTU = input.Mtu
		updatedFields = append(updatedFields, "mtu")
		ssd.tracerSpan.SetAttribute(sbDAOSpan, "mtu", *input.Mtu)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(s).Column(updatedFields...).Where("id = ?", input.SubnetId).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := ssd.GetByID(ctx, tx, s.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Clear sets parameters of an existing Subnet to null values in db
// parameters description, tenantID when true, the are set to null in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (ssd SubnetSQLDAO) Clear(ctx context.Context, tx *db.Tx, input SubnetClearInput) (*Subnet, error) {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.Clear")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()

		ssd.tracerSpan.SetAttribute(sbDAOSpan, "id", input.SubnetId.String())
	}

	s := &Subnet{
		ID: input.SubnetId,
	}

	updatedFields := []string{}

	if input.Description {
		s.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.DomainID {
		s.DomainID = nil
		updatedFields = append(updatedFields, "domain_id")
	}
	if input.ControllerNetworkSegmentID {
		s.ControllerNetworkSegmentID = nil
		updatedFields = append(updatedFields, "controller_network_segment_id")
	}
	if input.IPv4Prefix {
		s.IPv4Prefix = nil
		updatedFields = append(updatedFields, "ipv4_prefix")
	}
	if input.IPv4Gateway {
		s.IPv4Gateway = nil
		updatedFields = append(updatedFields, "ipv4_gateway")
	}
	if input.IPv4BlockID {
		s.IPv4BlockID = nil
		updatedFields = append(updatedFields, "ipv4_block_id")
	}
	if input.IPv6Prefix {
		s.IPv6Prefix = nil
		updatedFields = append(updatedFields, "ipv6_prefix")
	}
	if input.IPv6Gateway {
		s.IPv6Gateway = nil
		updatedFields = append(updatedFields, "ipv6_gateway")
	}
	if input.IPv6BlockID {
		s.IPv6BlockID = nil
		updatedFields = append(updatedFields, "ipv6_block_id")
	}
	if input.Mtu {
		s.MTU = nil
		updatedFields = append(updatedFields, "mtu")
	}
	if input.Deleted {
		s.Deleted = nil
		updatedFields = append(updatedFields, "deleted")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		query := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(s).Column(updatedFields...).Where("id = ?", input.SubnetId)
		// Soft-deleted rows are excluded by default; include them when undeleting.
		if input.Deleted {
			query = query.WhereAllWithDeleted()
		}
		_, err := query.Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := ssd.GetByID(ctx, tx, s.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an Subnet by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (ssd SubnetSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, sbDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SubnetDAO.Delete")
	if sbDAOSpan != nil {
		defer sbDAOSpan.End()

		ssd.tracerSpan.SetAttribute(sbDAOSpan, "id", id.String())
	}

	s := &Subnet{
		ID: id,
	}

	_, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model(s).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// GetIPv4CIDR returns the subnet's IPv4 CIDR string, or nil when IPv4Prefix is unset.
func (s *Subnet) GetIPv4CIDR() *string {
	if s.IPv4Prefix == nil || *s.IPv4Prefix == "" {
		return nil
	}
	if strings.Contains(*s.IPv4Prefix, "/") {
		return s.IPv4Prefix
	}
	cidr := fmt.Sprintf("%s/%d", *s.IPv4Prefix, s.PrefixLength)
	return &cidr
}

func subnetPrefixUsageFromInterfaces(ctx context.Context, cidr string, ifcCount int64, ips []string) (*cipam.Usage, error) {
	ipamer := cipam.New(ctx)
	ipamPrefix, err := ipamer.NewPrefix(ctx, cidr)
	if err != nil {
		return nil, err
	}

	validatedCidr := ipamPrefix.Cidr
	netIpPrefix, err := netip.ParsePrefix(validatedCidr)
	if err != nil {
		return nil, err
	}

	for _, ipStr := range ips {
		netIpAddr, ierr := netip.ParseAddr(strings.TrimSpace(ipStr))
		if ierr != nil || !netIpAddr.Is4() {
			continue
		}
		if !netIpPrefix.Contains(netIpAddr) {
			continue
		}
		_, ierr = ipamer.AcquireSpecificIP(ctx, validatedCidr, netIpAddr.String())
		if ierr != nil {
			continue
		}
	}

	ipamPrefix = ipamer.PrefixFrom(ctx, validatedCidr)
	if ipamPrefix == nil {
		return nil, fmt.Errorf("Prefix %q was not found in IPAM after loading IPs", validatedCidr)
	}

	usage := ipamPrefix.Usage()

	acquiredIPs := uint64(ifcCount) + 2
	if acquiredIPs > usage.AvailableIPs {
		acquiredIPs = usage.AvailableIPs
	}
	return &cipam.Usage{
		AvailableIPs:              usage.AvailableIPs,
		AcquiredIPs:               acquiredIPs,
		AvailableSmallestPrefixes: usage.AvailableSmallestPrefixes,
		AvailablePrefixes:         usage.AvailablePrefixes,
		AcquiredPrefixes:          usage.AcquiredPrefixes,
	}, nil
}

// GetPrefixUsage derives IPv4 interface usage stats for each Subnet via in-memory IPAM simulation.
func (ssd SubnetSQLDAO) GetPrefixUsage(ctx context.Context, tx *db.Tx, subnets ...*Subnet) (map[uuid.UUID]*cipam.Usage, error) {
	if len(subnets) == 0 {
		return map[uuid.UUID]*cipam.Usage{}, nil
	}

	subnetCIDRs := make(map[uuid.UUID]string, len(subnets))
	subnetIDs := make([]uuid.UUID, 0, len(subnets))
	for _, sn := range subnets {
		if sn == nil {
			return nil, fmt.Errorf("Failed to calculate usage stats for Subnet: nil argument specified")
		}
		cidr := sn.GetIPv4CIDR()
		if cidr == nil {
			continue
		}
		subnetCIDRs[sn.ID] = *cidr
		subnetIDs = append(subnetIDs, sn.ID)
	}
	if len(subnetIDs) == 0 {
		return map[uuid.UUID]*cipam.Usage{}, nil
	}

	idb := db.GetIDB(tx, ssd.dbSession)

	ifcCounts := make(map[uuid.UUID]int64, len(subnetIDs))
	ifcIPs := make(map[uuid.UUID][]string, len(subnetIDs))
	for _, id := range subnetIDs {
		ifcCounts[id] = 0
		ifcIPs[id] = nil
	}

	type row struct {
		SubnetID    uuid.UUID `bun:"subnet_id"`
		IPAddresses []string  `bun:"ip_addresses,array"`
	}
	var rows []row
	err := idb.NewRaw(
		`SELECT ifc.subnet_id, ifc.ip_addresses FROM "interface" AS ifc INNER JOIN instance AS inst ON inst.id = ifc.instance_id
		 WHERE ifc.subnet_id IN (?) AND ifc.deleted IS NULL AND inst.deleted IS NULL`,
		bun.In(subnetIDs),
	).Scan(ctx, &rows)
	if err != nil {
		return nil, err
	}
	for _, r := range rows {
		ifcCounts[r.SubnetID]++
		if len(r.IPAddresses) > 0 {
			ifcIPs[r.SubnetID] = append(ifcIPs[r.SubnetID], r.IPAddresses...)
		}
	}

	usageByID := make(map[uuid.UUID]*cipam.Usage, len(subnetIDs))
	for _, subnetID := range subnetIDs {
		usage, uerr := subnetPrefixUsageFromInterfaces(ctx, subnetCIDRs[subnetID], ifcCounts[subnetID], ifcIPs[subnetID])
		if uerr != nil {
			return nil, uerr
		}
		usageByID[subnetID] = usage
	}
	return usageByID, nil
}

// NewSubnetDAO returns a new SubnetDAO
func NewSubnetDAO(dbSession *db.Session) SubnetDAO {
	return &SubnetSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
