// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"

	"github.com/uptrace/bun"
)

const (
	// SiteStatusPending indicates that the site registration is pending
	SiteStatusPending = "Pending"
	// SiteStatusRegistered indicates that the site has been registered
	SiteStatusRegistered = "Registered"
	// SiteStatusError indicates that the site registration encountered errors
	SiteStatusError = "Error"

	// SiteRelationName is the relation name for the Site model
	SiteRelationName = "Site"

	// SiteOrderByDefault default field to be used for ordering when none specified
	SiteOrderByDefault = "created"
)

var (
	// SiteOrderByFields is a list of valid order by fields for the Site model
	SiteOrderByFields = []string{"name", "status", "created", "updated", "description", "location", "contact"}
	// SiteRelatedEntities is a list of valid relation by fields for the Site model
	SiteRelatedEntities = map[string]bool{InfrastructureProviderRelationName: true}
	// SiteStatusMap is a list of valid status for the Site model
	SiteStatusMap = map[string]bool{
		SiteStatusPending:    true,
		SiteStatusRegistered: true,
		SiteStatusError:      true,
	}
)

// Config stays flat so PostgreSQL JSONB concatenation can apply partial
// updates without replacing unrelated settings.
type SiteConfig struct {
	NetworkSecurityGroup             bool `json:"network_security_group"`
	NativeNetworking                 bool `json:"native_networking"`
	VpcSlaac                         bool `json:"vpc_slaac"`
	NVLinkPartition                  bool `json:"nvlink_partition"`
	Flow                             bool `json:"flow"`
	ImageBasedOperatingSystem        bool `json:"image_based_operating_system"`
	DPSPowerManagement               bool `json:"dps_power_management"`
	MaxNetworkSecurityGroupRuleCount *int `json:"max_network_security_group_rule_count"`
}

// Site represents entries in the site table
type Site struct {
	bun.BaseModel `bun:"table:site,alias:st"`

	ID                            uuid.UUID               `bun:"type:uuid,pk"`
	Name                          string                  `bun:"name,notnull"`
	DisplayName                   *string                 `bun:"display_name"`
	Description                   *string                 `bun:"description"`
	Org                           string                  `bun:"org,notnull"`
	InfrastructureProviderID      uuid.UUID               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider        *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	SiteControllerVersion         *string                 `bun:"site_controller_version"`
	SiteAgentVersion              *string                 `bun:"site_agent_version"`
	RegistrationToken             *string                 `bun:"registration_token"`
	RegistrationTokenExpiration   *time.Time              `bun:"registration_token_expiration"`
	SerialConsoleHostname         *string                 `bun:"serial_console_hostname"`
	IsSerialConsoleEnabled        bool                    `bun:"is_serial_console_enabled,notnull"`
	SerialConsoleIdleTimeout      *int                    `bun:"serial_console_idle_timeout"`
	SerialConsoleMaxSessionLength *int                    `bun:"serial_console_max_session_length"`
	IsInfinityEnabled             bool                    `bun:"is_infinity_enabled,notnull"`
	InventoryReceived             *time.Time              `bun:"inventory_received"`
	InventoryIntervalSeconds      *int                    `bun:"inventory_interval_seconds"`
	Status                        string                  `bun:"status,notnull"`
	Created                       time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                       time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                       *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy                     uuid.UUID               `bun:"type:uuid,notnull"`
	Location                      *SiteLocation           `bun:"location"` // since this is a json object, type of the column will be JSONB automatically
	Contact                       *SiteContact            `bun:"contact"`  // since this is a json object, type of the column will be JSONB automatically
	AgentCertExpiry               *time.Time              `bun:"agent_cert_expiry"`
	Config                        *SiteConfig             `bun:"config,type:jsonb"`
}

type SiteLocation struct {
	City    string `json:"city"`
	State   string `json:"state"`
	Country string `json:"country"`
}

type SiteContact struct {
	Email string `json:"email"`
}

// IsTimeWithinStaleInventoryThreshold reports whether actionTime is recent enough that an
// arriving inventory may predate it, which means the inventory should not be acted on for that
// object. The threshold follows the collection interval the Site Agent reports, and falls back
// to the default for a Site that has not reported one yet, so an unreported Site keeps the
// protection it had before the field existed. The Site Agent refuses a schedule slower than
// cutil.MaxInventoryReceiptInterval, so the reported interval is followed as given.
func (st *Site) IsTimeWithinStaleInventoryThreshold(actionTime time.Time) bool {
	interval := cutil.DefaultInventoryReceiptInterval
	if st != nil && st.InventoryIntervalSeconds != nil && *st.InventoryIntervalSeconds > 0 {
		interval = time.Duration(*st.InventoryIntervalSeconds) * time.Second
	}
	return time.Since(actionTime) < interval+cutil.StaleInventoryBuffer
}

type SiteCreateInput struct {
	Name                          string
	DisplayName                   *string
	Description                   *string
	Org                           string
	InfrastructureProviderID      uuid.UUID
	SiteControllerVersion         *string
	SiteAgentVersion              *string
	RegistrationToken             *string
	RegistrationTokenExpiration   *time.Time
	SerialConsoleHostname         *string
	IsSerialConsoleEnabled        bool
	SerialConsoleIdleTimeout      *int
	SerialConsoleMaxSessionLength *int
	IsInfinityEnabled             bool
	InventoryReceived             *time.Time
	Status                        string
	CreatedBy                     uuid.UUID
	Location                      *SiteLocation
	Contact                       *SiteContact
	Config                        SiteConfig
}

type SiteConfigUpdateInput struct {
	NetworkSecurityGroup             *bool `json:"network_security_group,omitempty"`
	NativeNetworking                 *bool `json:"native_networking,omitempty"`
	VpcSlaac                         *bool `json:"vpc_slaac,omitempty"`
	NVLinkPartition                  *bool `json:"nvlink_partition,omitempty"`
	Flow                             *bool `json:"flow,omitempty"`
	ImageBasedOperatingSystem        *bool `json:"image_based_operating_system,omitempty"`
	DPSPowerManagement               *bool `json:"dps_power_management,omitempty"`
	MaxNetworkSecurityGroupRuleCount *int  `json:"max_network_security_group_rule_count,omitempty"`
}

type SiteUpdateInput struct {
	SiteID                        uuid.UUID
	Name                          *string
	DisplayName                   *string
	Description                   *string
	InfrastructureProviderID      uuid.UUID
	SiteControllerVersion         *string
	SiteAgentVersion              *string
	RegistrationToken             *string
	RegistrationTokenExpiration   *time.Time
	SerialConsoleHostname         *string
	IsSerialConsoleEnabled        *bool
	SerialConsoleIdleTimeout      *int
	SerialConsoleMaxSessionLength *int
	IsInfinityEnabled             *bool
	InventoryReceived             *time.Time
	InventoryIntervalSeconds      *int
	Status                        *string
	Location                      *SiteLocation
	Contact                       *SiteContact
	AgentCertExpiry               *time.Time
	Config                        *SiteConfigUpdateInput
}

type SiteConfigFilterInput struct {
	NetworkSecurityGroup             *bool `json:"network_security_group,omitempty"`
	NativeNetworking                 *bool `json:"native_networking,omitempty"`
	VpcSlaac                         *bool `json:"vpc_slaac,omitempty"`
	NVLinkPartition                  *bool `json:"nvlink_partition,omitempty"`
	Flow                             *bool `json:"flow,omitempty"`
	ImageBasedOperatingSystem        *bool `json:"image_based_operating_system,omitempty"`
	DPSPowerManagement               *bool `json:"dps_power_management,omitempty"`
	MaxNetworkSecurityGroupRuleCount *int  `json:"max_network_security_group_rule_count,omitempty"`
}

type SiteFilterInput struct {
	Name                      *string
	Org                       *string
	InfrastructureProviderIDs []uuid.UUID
	SiteIDs                   []uuid.UUID
	Config                    *SiteConfigFilterInput
	Statuses                  []string
	SearchQuery               *string
}

var _ bun.BeforeAppendModelHook = (*Site)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (st *Site) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		st.Created = db.GetCurTime()
		st.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		st.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Site)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (s *Site) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id") ON DELETE CASCADE`)
	return nil
}

// SiteDAO is the data access interface for Site
type SiteDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string, includeDeleted bool) (*Site, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SiteFilterInput, page paginator.PageInput, includeRelations []string) (sites []Site, total int, err error)
	// GetCount returns total count of rows for specified filter
	GetCount(ctx context.Context, tx *db.Tx, filter SiteFilterInput) (count int, err error)
	//
	Create(ctx context.Context, tx *db.Tx, input SiteCreateInput) (*Site, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SiteUpdateInput) (*Site, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SiteSQLDAO is the SQL data access object for Site
type SiteSQLDAO struct {
	dbSession *db.Session
	SiteDAO
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a Site by its ID
func (ssd SiteSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string, includeDeleted bool) (*Site, error) {
	// Create a child span and set the attributes for current request
	ctx, stDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.GetByID")
	if stDAOSpan != nil {
		defer stDAOSpan.End()

		ssd.tracerSpan.SetAttribute(stDAOSpan, "id", id.String())
	}

	st := &Site{}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(st).Where("st.id = ?", id)
	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	if includeDeleted {
		query = query.WhereDeleted()
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return st, nil
}

func (ssd SiteSQLDAO) setQueryWithFilter(filter SiteFilterInput, query *bun.SelectQuery, siteDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.Name != nil {
		query = query.Where("st.name = ?", *filter.Name)
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "name", *filter.Name)
	}

	if filter.Org != nil {
		query = query.Where("st.org = ?", *filter.Org)
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "org", *filter.Org)
	}

	if filter.InfrastructureProviderIDs != nil {
		query = query.Where("st.infrastructure_provider_id IN (?)", bun.In(filter.InfrastructureProviderIDs))
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "infrastructure_provider_ids", filter.InfrastructureProviderIDs)
	}

	if filter.SiteIDs != nil {
		query = query.Where("st.id IN (?)", bun.In(filter.SiteIDs))
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "id", filter.SiteIDs)
	}

	if filter.Config != nil {
		query = query.Where("st.config @> ?::jsonb", filter.Config)
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "config", fmt.Sprintf("%+v", filter.Config))
	}

	if filter.Statuses != nil {
		if len(filter.Statuses) == 1 {
			query = query.Where("st.status = ?", filter.Statuses[0])
		} else {
			query = query.Where("st.status IN (?)", bun.In(filter.Statuses))
		}
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "status", filter.Statuses)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(st.name, ' ') || ' ' || coalesce(st.description, ' ') || ' ' || "+
					"coalesce(st.status, ' ') || ' ' || coalesce(st.location::text, ' ') || ' ' || coalesce(st.contact::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("st.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("st.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("st.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("st.location::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("st.contact::text ILIKE ?", "%"+searchQuery+"%")
		})
		ssd.tracerSpan.SetAttribute(siteDAOSpan, "search_query", searchQuery)
	}
	return query, nil
}

// GetAll returns all Sites for given params
// if orderBy is nil, then records are ordered by column specified in SiteOrderByDefault in ascending order
func (ssd SiteSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SiteFilterInput, page paginator.PageInput, includeRelations []string) (sites []Site, total int, err error) {
	// Create a child span and set the attributes for current request
	ctx, stDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.GetAll")
	if stDAOSpan != nil {
		defer stDAOSpan.End()
	}

	sts := []Site{}

	if filter.SiteIDs != nil && len(filter.SiteIDs) == 0 {
		return sts, 0, nil
	}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(&sts)

	query, err = ssd.setQueryWithFilter(filter, query, stDAOSpan)
	if err != nil {
		return sts, 0, err
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy == nil {
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(SiteOrderByDefault))
	} else {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		if page.OrderBy.Field != SiteOrderByDefault {
			multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(SiteOrderByDefault))
		}
	}

	paginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, SiteOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return sts, paginator.Total, nil
}

// GetCount returns count of sites for given params
func (ssd SiteSQLDAO) GetCount(ctx context.Context, tx *db.Tx, filter SiteFilterInput) (count int, err error) {
	// Create a child span and set the attributes for current request
	ctx, siteDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.GetCount")
	if siteDAOSpan != nil {
		defer siteDAOSpan.End()
	}
	sts := []Site{}

	if filter.SiteIDs != nil && len(filter.SiteIDs) == 0 {
		return 0, nil
	}

	query := db.GetIDB(tx, ssd.dbSession).NewSelect().Model(&sts)
	query, err = ssd.setQueryWithFilter(filter, query, siteDAOSpan)
	if err != nil {
		return 0, err
	}

	return query.Count(ctx)
}

// Create creates a Site from the given parameters
func (ssd SiteSQLDAO) Create(ctx context.Context, tx *db.Tx, input SiteCreateInput) (*Site, error) {
	// Create a child span and set the attributes for current request
	ctx, stDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.Create")
	if stDAOSpan != nil {
		defer stDAOSpan.End()

		ssd.tracerSpan.SetAttribute(stDAOSpan, "name", input.Name)
	}

	st := &Site{
		ID:                            uuid.New(),
		Name:                          input.Name,
		DisplayName:                   input.DisplayName,
		Description:                   input.Description,
		Org:                           input.Org,
		InfrastructureProviderID:      input.InfrastructureProviderID,
		SiteControllerVersion:         input.SiteControllerVersion,
		SiteAgentVersion:              input.SiteAgentVersion,
		RegistrationToken:             input.RegistrationToken,
		RegistrationTokenExpiration:   input.RegistrationTokenExpiration,
		IsInfinityEnabled:             input.IsInfinityEnabled,
		SerialConsoleHostname:         input.SerialConsoleHostname,
		IsSerialConsoleEnabled:        input.IsSerialConsoleEnabled,
		SerialConsoleIdleTimeout:      input.SerialConsoleIdleTimeout,
		SerialConsoleMaxSessionLength: input.SerialConsoleMaxSessionLength,
		Status:                        input.Status,
		CreatedBy:                     input.CreatedBy,
		Location:                      input.Location,
		Contact:                       input.Contact,
		Config:                        &input.Config,
	}
	_, err := db.GetIDB(tx, ssd.dbSession).NewInsert().Model(st).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nst, err := ssd.GetByID(ctx, tx, st.ID, nil, false)
	if err != nil {
		return nil, err
	}

	return nst, nil
}

// Update updates a Site from the given parameters
func (ssd SiteSQLDAO) Update(ctx context.Context, tx *db.Tx, input SiteUpdateInput) (*Site, error) {
	// Create a child span and set the attributes for current request
	ctx, stDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.Update")
	if stDAOSpan != nil {
		defer stDAOSpan.End()
		ssd.tracerSpan.SetAttribute(stDAOSpan, "id", input.SiteID.String())
	}

	updatedFields := []string{}

	// If Config is not nil, there's a chance we'll need to
	// do two separate UPDATEs, so we need to make sure we wrap
	// things in a txn if one wasn't provided.
	// If we end up with another case like this, we should either
	// simply create a txn if tx is nil or switch this function to
	// use SetColumn for all fields.

	// We're going to intentionally close over this variable.
	// If input.Config == nil OR we were handed a txn by the caller,
	// then this will do nothing.
	// Otherwise, this will control whether we should commit/rollback
	// the txn we created in this function.  We'll set this to true
	// as the very last step just before we return.
	commitInternalTx := false

	var err error
	if input.Config != nil && tx == nil {
		tx, err = db.BeginTx(ctx, ssd.dbSession, &sql.TxOptions{})
		if err != nil {
			return nil, err
		}

		defer func() {
			if commitInternalTx {
				tx.Commit()
			} else {
				tx.Rollback()
			}
		}()
	}

	st := &Site{
		ID: input.SiteID,
	}

	if input.Name != nil {
		st.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "name", *input.Name)
	}

	if input.DisplayName != nil {
		st.DisplayName = input.DisplayName
		updatedFields = append(updatedFields, "display_name")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "display_name", *input.DisplayName)
	}

	if input.Description != nil {
		st.Description = input.Description
		updatedFields = append(updatedFields, "description")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "description", *input.Description)
	}

	if input.SiteControllerVersion != nil {
		st.SiteControllerVersion = input.SiteControllerVersion
		updatedFields = append(updatedFields, "site_controller_version")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "site_controller_version", *input.SiteControllerVersion)
	}

	if input.SiteAgentVersion != nil {
		st.SiteAgentVersion = input.SiteAgentVersion
		updatedFields = append(updatedFields, "site_agent_version")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "site_agent_version", *input.SiteAgentVersion)
	}

	if input.RegistrationToken != nil {
		st.RegistrationToken = input.RegistrationToken
		updatedFields = append(updatedFields, "registration_token")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "registration_token", *input.RegistrationToken)
	}

	if input.RegistrationTokenExpiration != nil {
		st.RegistrationTokenExpiration = input.RegistrationTokenExpiration
		updatedFields = append(updatedFields, "registration_token_expiration")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "registration_token_expiration", *input.RegistrationTokenExpiration)
	}

	if input.IsInfinityEnabled != nil {
		st.IsInfinityEnabled = *input.IsInfinityEnabled
		updatedFields = append(updatedFields, "is_infinity_enabled")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "is_infinity_enabled", *input.IsInfinityEnabled)
	}

	if input.SerialConsoleHostname != nil {
		st.SerialConsoleHostname = input.SerialConsoleHostname
		updatedFields = append(updatedFields, "serial_console_hostname")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "serial_console_hostname", *input.SerialConsoleHostname)
	}

	if input.IsSerialConsoleEnabled != nil {
		st.IsSerialConsoleEnabled = *input.IsSerialConsoleEnabled
		updatedFields = append(updatedFields, "is_serial_console_enabled")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "is_serial_console_enabled", *input.IsSerialConsoleEnabled)
	}

	if input.SerialConsoleIdleTimeout != nil {
		st.SerialConsoleIdleTimeout = input.SerialConsoleIdleTimeout
		updatedFields = append(updatedFields, "serial_console_idle_timeout")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "serial_console_idle_timeout", *input.SerialConsoleIdleTimeout)
	}

	if input.SerialConsoleMaxSessionLength != nil {
		st.SerialConsoleMaxSessionLength = input.SerialConsoleMaxSessionLength
		updatedFields = append(updatedFields, "serial_console_max_session_length")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "serial_console_max_session_length", *input.SerialConsoleMaxSessionLength)
	}

	if input.InventoryReceived != nil {
		st.InventoryReceived = input.InventoryReceived
		updatedFields = append(updatedFields, "inventory_received")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "inventory_received", *input.InventoryReceived)
	}

	if input.InventoryIntervalSeconds != nil {
		st.InventoryIntervalSeconds = input.InventoryIntervalSeconds
		updatedFields = append(updatedFields, "inventory_interval_seconds")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "inventory_interval_seconds", *input.InventoryIntervalSeconds)
	}

	if input.Status != nil {
		st.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "status", *input.Status)
	}

	if input.Location != nil {
		st.Location = input.Location
		updatedFields = append(updatedFields, "location")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "location", *input.Location)
	}

	if input.Contact != nil {
		st.Contact = input.Contact
		updatedFields = append(updatedFields, "contact")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "contact", *input.Contact)
	}

	// AgentCertExpiry only handled on update as requested
	if input.AgentCertExpiry != nil {
		st.AgentCertExpiry = input.AgentCertExpiry
		updatedFields = append(updatedFields, "agent_cert_expiry")
		ssd.tracerSpan.SetAttribute(stDAOSpan, "agent_cert_expiry", input.AgentCertExpiry.String())
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(st).Column(updatedFields...).Where("id = ?", st.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	if input.Config != nil {
		_, err := db.GetIDB(tx, ssd.dbSession).NewUpdate().
			Model(st).
			Set("config = COALESCE(config, '{}'::jsonb) || ?::jsonb, updated = current_timestamp", input.Config).
			Where("id = ?", st.ID).
			Exec(ctx)

		if err != nil {
			return nil, err
		}
	}

	ust, err := ssd.GetByID(ctx, tx, st.ID, nil, false)
	if err != nil {
		return nil, err
	}

	commitInternalTx = true

	return ust, nil
}

// Delete deletes a Site by its ID
func (ssd SiteSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, stDAOSpan := ssd.tracerSpan.CreateChildInCurrentContext(ctx, "SiteDAO.DeleteByID")
	if stDAOSpan != nil {
		defer stDAOSpan.End()

		ssd.tracerSpan.SetAttribute(stDAOSpan, "id", id.String())
	}

	_, err := db.GetIDB(tx, ssd.dbSession).NewDelete().Model((*Site)(nil)).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSiteDAO returns a new SiteDAO
func NewSiteDAO(dbSession *db.Session) SiteDAO {
	return &SiteSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
