// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"go.opentelemetry.io/otel/attribute"

	"github.com/uptrace/bun"
)

const (
	// IssuerRelationName is the relation name for the Issuer model.
	IssuerRelationName = "Issuer"
	// IssuerOrderByDefault is the deterministic default for paginated issuer reads.
	IssuerOrderByDefault = "created_at"
)

// IssuerOrderByFields is the public orderBy field allowlist.
var IssuerOrderByFields = []string{"created_at", "issuer_url"}

const (
	// IssuerSourceConfig marks an issuer or claim mapping the ConfigMap defines.
	IssuerSourceConfig = "config"
	// IssuerSourceAPI marks an issuer or claim mapping written through the API.
	IssuerSourceAPI = "api"

	// IssuerStatusPending means no key set has been fetched yet.
	IssuerStatusPending = "Pending"
	// IssuerStatusReady means a key set is loaded and the last refresh succeeded.
	IssuerStatusReady = "Ready"
	// IssuerStatusDegraded means a key set is loaded but the last refresh failed, so
	// a token signed with a key not in the loaded set cannot be verified.
	IssuerStatusDegraded = "Degraded"
	// IssuerStatusSuperseded is recorded in the status history when the ConfigMap
	// takes over an API-created issuer. It never rests on the row itself.
	IssuerStatusSuperseded = "Superseded"
)

// ClaimMapping is one entry of an issuer's claim_mappings JSONB array. It is the
// persisted, JSON-tagged twin of auth/pkg/config.ClaimMapping (which carries only
// mapstructure tags for Viper), plus Source, which the auth registry does not need.
// The config bridge converts between them when (un)loading the live auth registry.
type ClaimMapping struct {
	// OrgAttribute is the JWT claim path to extract the org name (dynamic mapping).
	OrgAttribute string `json:"orgAttribute,omitempty"`
	// OrgDisplayAttribute is the JWT claim path for the org display name (dynamic mapping).
	OrgDisplayAttribute string `json:"orgDisplayAttribute,omitempty"`
	// OrgName is the fixed org name (static mapping).
	OrgName string `json:"orgName,omitempty"`
	// OrgDisplayName is the display name for a static org mapping.
	OrgDisplayName string `json:"orgDisplayName,omitempty"`
	// RolesAttribute is the JWT claim path to extract roles. Takes precedence over Roles.
	RolesAttribute string `json:"rolesAttribute,omitempty"`
	// Roles is the static role list.
	Roles []string `json:"roles,omitempty"`
	// Audiences is the optional per-mapping token audiences allowed to authorize this mapping.
	Audiences []string `json:"audiences,omitempty"`
	// Scopes is the optional per-mapping scope set the token must carry in full.
	Scopes []string `json:"scopes,omitempty"`
	// IsServiceAccount, when true, assigns admin roles (disconnected mode only).
	IsServiceAccount bool `json:"isServiceAccount,omitempty"`
	// Source is who defined this mapping, IssuerSourceConfig or IssuerSourceAPI. Set
	// by the server, never taken from a request body.
	Source string `json:"source,omitempty"`
}

// IssuerCreateOrUpdateInput is the complete operator-managed configuration used
// to create or replace an Issuer selected by IssuerURL.
type IssuerCreateOrUpdateInput struct {
	Origin         string
	IssuerURL      string
	JWKSUrl        string
	JWKSTimeout    string
	ServiceAccount bool
	Audiences      []string
	Scopes         []string
	ClaimMappings  []ClaimMapping
	// Source is IssuerSourceConfig for a row the ConfigMap sync owns, otherwise
	// IssuerSourceAPI (the default).
	Source string
	// ConfigRevision is the ConfigMap revision a config row was synced from.
	ConfigRevision *int
	CreatedBy      *uuid.UUID
	UpdatedBy      *uuid.UUID
}

// ToIssuer projects a create-or-update input onto the Issuer it would produce,
// applying the same defaults the table declares. Callers use the result both to
// validate the candidate and to detect idempotent PUT requests.
func (input IssuerCreateOrUpdateInput) ToIssuer() Issuer {
	claimMappings := input.ClaimMappings
	if claimMappings == nil {
		claimMappings = []ClaimMapping{}
	}
	audiences := input.Audiences
	if audiences == nil {
		audiences = []string{}
	}
	scopes := input.Scopes
	if scopes == nil {
		scopes = []string{}
	}
	jwksTimeout := input.JWKSTimeout
	if jwksTimeout == "" {
		jwksTimeout = "5s"
	}
	origin := input.Origin
	if origin == "" {
		origin = "custom"
	}
	source := input.Source
	if source == "" {
		source = IssuerSourceAPI
	}
	// A create names only its creator; that user is also the last updater.
	updatedBy := input.UpdatedBy
	if updatedBy == nil {
		updatedBy = input.CreatedBy
	}

	return Issuer{
		Origin:         origin,
		IssuerURL:      input.IssuerURL,
		JWKSUrl:        input.JWKSUrl,
		JWKSTimeout:    jwksTimeout,
		ServiceAccount: input.ServiceAccount,
		Audiences:      audiences,
		Scopes:         scopes,
		ClaimMappings:  claimMappings,
		Source:         source,
		ConfigRevision: input.ConfigRevision,
		Status:         IssuerStatusPending,
		CreatedBy:      input.CreatedBy,
		UpdatedBy:      updatedBy,
	}
}

// IssuerFilterInput are the optional filters for the GetAll method.
type IssuerFilterInput struct {
	IssuerURL *string
	Status    *string
}

// Issuer is a runtime-managed external JWT issuer. It is a persistent mirror of
// the static ConfigMap IssuerConfig shape and is loaded into the same auth
// registry (map issuer_url -> JwksConfig) as ConfigMap issuers.
type Issuer struct {
	bun.BaseModel `bun:"table:issuer,alias:iss"`

	ID             uuid.UUID      `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Origin         string         `bun:"origin,notnull,default:'custom'"`
	IssuerURL      string         `bun:"issuer_url,notnull"`
	JWKSUrl        string         `bun:"jwks_url,notnull"`
	JWKSTimeout    string         `bun:"jwks_timeout,notnull,default:'5s'"`
	ServiceAccount bool           `bun:"service_account,notnull"`
	Audiences      []string       `bun:"audiences,array,notnull"`
	Scopes         []string       `bun:"scopes,array,notnull"`
	ClaimMappings  []ClaimMapping `bun:"claim_mappings,type:jsonb,notnull,default:'[]'"`
	// JWKSKeys is the cached key set fetched from JWKSUrl, stored so a restarting
	// replica can serve tokens before reaching the IdP. It is deliberately absent from
	// Signature(): a refresh is not a configuration change, and counting it as one
	// would rebuild the live JwksConfig and discard the keys it just fetched.
	JWKSKeys json.RawMessage `bun:"jwks_keys,type:jsonb,nullzero"`
	// JWKSFetchedAt is when JWKSKeys was last successfully fetched. NULL means never.
	JWKSFetchedAt *time.Time `bun:"jwks_fetched_at,nullzero"`
	// Source is IssuerSourceConfig for a row the ConfigMap sync owns, IssuerSourceAPI
	// otherwise. A config row may still carry api-sourced claim mappings. Part of
	// Signature(): a takeover rebuilds the live entry.
	Source string `bun:"source,notnull,default:'api'"`
	// Status is the stored lifecycle state (IssuerStatusPending, Ready or Degraded)
	// and StatusMessage its explanation. Not part of Signature(): a status write is
	// not a configuration change, like JWKSKeys.
	Status        string  `bun:"status,notnull,default:'Pending'"`
	StatusMessage *string `bun:"status_message"`
	// ConfigRevision is the Helm release revision of the ConfigMap a config row was
	// last synced from, nil for api rows or when the deployment sets none.
	ConfigRevision *int `bun:"config_revision"`
	// JWKSLastAttemptAt and JWKSLastError describe the most recent refresh attempt,
	// successful or not. JWKSFetchedAt records the last success only.
	JWKSLastAttemptAt *time.Time `bun:"jwks_last_attempt_at,nullzero"`
	JWKSLastError     *string    `bun:"jwks_last_error"`
	CreatedAt         time.Time  `bun:"created_at,nullzero,notnull,default:current_timestamp"`
	UpdatedAt         time.Time  `bun:"updated_at,nullzero,notnull,default:current_timestamp"`
	CreatedBy         *uuid.UUID `bun:"created_by,type:uuid"`
	UpdatedBy         *uuid.UUID `bun:"updated_by,type:uuid"`
	Deleted           *time.Time `bun:"deleted,soft_delete"`
}

var _ bun.BeforeAppendModelHook = (*Issuer)(nil)

// BeforeAppendModel maintains the created_at/updated_at timestamps.
func (i *Issuer) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		i.CreatedAt = db.GetCurTime()
		i.UpdatedAt = db.GetCurTime()
	case *bun.UpdateQuery:
		i.UpdatedAt = db.GetCurTime()
	}
	return nil
}

// IssuerDAO is an interface for interacting with the Issuer model.
type IssuerDAO interface {
	Create(ctx context.Context, tx *db.Tx, input IssuerCreateOrUpdateInput) (*Issuer, error)
	Update(ctx context.Context, tx *db.Tx, id uuid.UUID, input IssuerCreateOrUpdateInput, clearJWKSCache bool) (*Issuer, error)
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID) (*Issuer, error)
	GetByIssuerURL(ctx context.Context, tx *db.Tx, issuerURL string) (*Issuer, error)
	GetAll(ctx context.Context, tx *db.Tx, filter IssuerFilterInput) ([]Issuer, error)
	GetPage(ctx context.Context, tx *db.Tx, filter IssuerFilterInput, page paginator.PageInput) ([]Issuer, int, error)
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	UpdateJWKSCache(ctx context.Context, tx *db.Tx, id uuid.UUID, jwksURL string, raw json.RawMessage, fetchedAt time.Time) error
	RecordJWKSAttempt(ctx context.Context, tx *db.Tx, id uuid.UUID, jwksURL string, at time.Time, fetchErr *string, status string, message *string) (bool, error)
	UpdateStatus(ctx context.Context, tx *db.Tx, id uuid.UUID, status string, message *string) (bool, error)
	MaxConfigRevision(ctx context.Context, tx *db.Tx) (int, error)
	StampConfigRevision(ctx context.Context, tx *db.Tx, revision int, includeDeleted bool) error
}

// IssuerSQLDAO is the SQL implementation of the IssuerDAO interface.
type IssuerSQLDAO struct {
	dbSession *db.Session
	IssuerDAO
}

// NewIssuerDAO returns a new IssuerDAO.
func NewIssuerDAO(dbSession *db.Session) IssuerDAO {
	return &IssuerSQLDAO{
		dbSession: dbSession,
	}
}

// Create inserts a new Issuer. Because there are two operations (INSERT, SELECT),
// this call must happen within a transaction.
func (isd IssuerSQLDAO) Create(ctx context.Context, tx *db.Tx, input IssuerCreateOrUpdateInput) (_ *Issuer, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.Create")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()

	i := input.ToIssuer()
	i.ID = uuid.New()

	_, err := db.GetIDB(tx, isd.dbSession).NewInsert().Model(&i).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return isd.GetByID(ctx, tx, i.ID)
}

// Update fully replaces the operator-managed configuration of an Issuer while
// preserving its identity and creation metadata. Cached signing keys remain valid
// across policy-only changes, but must be cleared when the JWKS endpoint changes.
func (isd IssuerSQLDAO) Update(
	ctx context.Context,
	tx *db.Tx,
	id uuid.UUID,
	input IssuerCreateOrUpdateInput,
	clearJWKSCache bool,
) (_ *Issuer, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.Update")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()
	cotel.SetAttribute(issuerDAOSpan, attribute.String("issuer_id", id.String()))

	i := input.ToIssuer()
	i.ID = id
	columns := []string{
		"origin",
		"jwks_url",
		"jwks_timeout",
		"service_account",
		"audiences",
		"scopes",
		"claim_mappings",
		"source",
		"config_revision",
		"updated_at",
		"updated_by",
	}
	// A new key source has no fetch outcome yet either.
	if clearJWKSCache {
		columns = append(columns, "jwks_keys", "jwks_fetched_at", "jwks_last_attempt_at", "jwks_last_error")
	}

	_, err := db.GetIDB(tx, isd.dbSession).NewUpdate().Model(&i).
		Column(columns...).
		Where("id = ? AND deleted IS NULL", id).
		Exec(ctx)
	if err != nil {
		return nil, err
	}

	return isd.GetByID(ctx, tx, id)
}

// MaxConfigRevision returns the highest ConfigMap revision any row, deleted rows
// included, was synced from, zero when none records one.
func (isd IssuerSQLDAO) MaxConfigRevision(ctx context.Context, tx *db.Tx) (int, error) {
	var revision int
	err := db.GetIDB(tx, isd.dbSession).NewRaw("SELECT COALESCE(MAX(config_revision), 0) FROM issuer").Scan(ctx, &revision)
	return revision, err
}

// StampConfigRevision records revision on every live config row, so a replica on
// an older revision can tell the table was synced from a newer one even when that
// sync changed nothing else. With includeDeleted the deleted rows are stamped too,
// which a sync that starts the revision count over needs so the rows it does not
// touch stop reading as newer.
func (isd IssuerSQLDAO) StampConfigRevision(ctx context.Context, tx *db.Tx, revision int, includeDeleted bool) error {
	q := db.GetIDB(tx, isd.dbSession).NewUpdate().Model((*Issuer)(nil)).WhereAllWithDeleted().
		Set("config_revision = ?", revision).
		Where("source = ? AND config_revision IS DISTINCT FROM ?", IssuerSourceConfig, revision)
	if !includeDeleted {
		q = q.Where("deleted IS NULL")
	}
	_, err := q.Exec(ctx)
	return err
}

// GetByID returns an Issuer by ID. Returns db.ErrDoesNotExist if not found.
func (isd IssuerSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID) (_ *Issuer, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.GetByID")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()
	cotel.SetAttribute(issuerDAOSpan, attribute.String("issuer_id", id.String()))

	i := &Issuer{}
	err := db.GetIDB(tx, isd.dbSession).NewSelect().Model(i).Where("iss.id = ?", id).Scan(ctx)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}
	return i, nil
}

// GetByIssuerURL returns the live Issuer selected by its natural API key.
// Returns db.ErrDoesNotExist if no active row has that URL.
func (isd IssuerSQLDAO) GetByIssuerURL(ctx context.Context, tx *db.Tx, issuerURL string) (_ *Issuer, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.GetByIssuerURL")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()
	cotel.SetAttribute(issuerDAOSpan, attribute.String("issuer_url", issuerURL))

	i := &Issuer{}
	err := db.GetIDB(tx, isd.dbSession).NewSelect().Model(i).Where("iss.issuer_url = ?", issuerURL).Scan(ctx)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}
	return i, nil
}

// GetAll returns all Issuers matching the optional filters. Soft-deleted rows are
// excluded automatically. If no records match, the returned slice is empty and
// error is nil.
func (isd IssuerSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter IssuerFilterInput) ([]Issuer, error) {
	limit := paginator.TotalLimit
	issuers, _, err := isd.GetPage(ctx, tx, filter, paginator.PageInput{Limit: &limit})
	return issuers, err
}

// GetPage returns one deterministic page of Issuers and the total matching row
// count. Equal primary sort values are ordered by ID so page boundaries are stable.
func (isd IssuerSQLDAO) GetPage(ctx context.Context, tx *db.Tx, filter IssuerFilterInput, page paginator.PageInput) (_ []Issuer, _ int, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.GetAll")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()

	issuers := []Issuer{}
	query := db.GetIDB(tx, isd.dbSession).NewSelect().Model(&issuers)

	if filter.IssuerURL != nil {
		query = query.Where("iss.issuer_url = ?", *filter.IssuerURL)
	}
	if filter.Status != nil {
		query = query.Where("iss.status = ?", *filter.Status)
	}

	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(IssuerOrderByDefault)
	}

	pager, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, IssuerOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	if err := pager.Query.Order("iss.id ASC").Limit(pager.Limit).Offset(pager.Offset).Scan(ctx); err != nil {
		return nil, 0, err
	}
	return issuers, pager.Total, nil
}

// Delete soft-deletes an Issuer by ID. Idempotent: no error if the row is absent.
func (isd IssuerSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.Delete")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()

	i := &Issuer{ID: id}
	_, err := db.GetIDB(tx, isd.dbSession).NewDelete().Model(i).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}
	return nil
}

// UpdateJWKSCache stores a freshly fetched key set for an issuer.
//
// The SET clause names the two cache columns only, so a background refresh does
// not touch updated_at or updated_by the way an operator edit does.
//
// The jwks_fetched_at comparison is the stale-write guard for two replicas
// refreshing concurrently: fetchedAt is when the caller issued the fetch, not when
// it completed, so it compares acquisition order. A soft-deleted row is never
// written.
//
// jwksURL is the endpoint the keys were fetched from. A row whose jwks_url has
// moved since the fetch started is left alone: after a PUT changes the endpoint
// and clears the cache, the IS NULL branch would otherwise accept a slow refresh
// of the old endpoint and cache the old provider's keys under the new one.
func (isd IssuerSQLDAO) UpdateJWKSCache(ctx context.Context, tx *db.Tx, id uuid.UUID, jwksURL string, raw json.RawMessage, fetchedAt time.Time) (retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.UpdateJWKSCache")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()
	cotel.SetAttribute(issuerDAOSpan, attribute.String("issuer_id", id.String()))

	if len(raw) == 0 {
		return errors.New("refusing to cache an empty JWKS key set")
	}

	_, err := db.GetIDB(tx, isd.dbSession).NewUpdate().Model((*Issuer)(nil)).
		Set("jwks_fetched_at = ?", fetchedAt).
		Set("jwks_keys = ?::jsonb", string(raw)).
		Where("id = ? AND jwks_url = ? AND deleted IS NULL AND (jwks_fetched_at IS NULL OR jwks_fetched_at < ?)", id, jwksURL, fetchedAt).
		Exec(ctx)
	return err
}

// RecordJWKSAttempt stores the outcome of a key refresh attempt together with the
// status it implies, as one write, and reports whether the status changed so the
// caller records one history entry per transition. The row holds the latest
// accepted attempt across replicas: a report older than the stored attempt, or
// one made against an endpoint the row has since left, is refused. It never
// touches updated_at or the cached keys.
func (isd IssuerSQLDAO) RecordJWKSAttempt(ctx context.Context, tx *db.Tx, id uuid.UUID, jwksURL string, at time.Time, fetchErr *string, status string, message *string) (transitioned bool, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.RecordJWKSAttempt")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()

	var previous string
	err := db.GetIDB(tx, isd.dbSession).NewSelect().Model((*Issuer)(nil)).Column("status").
		Where("id = ? AND deleted IS NULL", id).For("UPDATE").Scan(ctx, &previous)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return false, nil
		}
		return false, err
	}
	res, err := db.GetIDB(tx, isd.dbSession).NewUpdate().Model((*Issuer)(nil)).
		Set("jwks_last_attempt_at = ?", at).
		Set("jwks_last_error = ?", fetchErr).
		Set("status = ?", status).
		Set("status_message = ?", message).
		Where("id = ? AND deleted IS NULL AND jwks_url = ? AND (jwks_last_attempt_at IS NULL OR jwks_last_attempt_at <= ?)", id, jwksURL, at).
		Where("(jwks_last_attempt_at IS DISTINCT FROM ? OR jwks_last_error IS DISTINCT FROM ? OR status IS DISTINCT FROM ? OR status_message IS DISTINCT FROM ?)", at, fetchErr, status, message).
		Exec(ctx)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n > 0 && previous != status, err
}

// UpdateStatus stores status and message when they differ from what the row holds
// and reports whether a row changed, so the caller records one history entry per
// transition rather than per observation. It never touches updated_at.
func (isd IssuerSQLDAO) UpdateStatus(ctx context.Context, tx *db.Tx, id uuid.UUID, status string, message *string) (changed bool, retErr error) {
	ctx, issuerDAOSpan := cotel.StartSpan(ctx, "IssuerDAO.UpdateStatus")
	defer func() { cotel.EndSpan(issuerDAOSpan, retErr) }()

	res, err := db.GetIDB(tx, isd.dbSession).NewUpdate().Model((*Issuer)(nil)).
		Set("status = ?", status).
		Set("status_message = ?", message).
		Where("id = ? AND deleted IS NULL AND (status IS DISTINCT FROM ? OR status_message IS DISTINCT FROM ?)", id, status, message).
		Exec(ctx)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n > 0, err
}

// HasCachedKeys reports whether the row carries a key set some replica fetched.
// Both fields are required: without a fetch time there is no defensible stamp to
// install the keys under, so hydration ignores them.
func (i Issuer) HasCachedKeys() bool {
	return len(i.JWKSKeys) > 0 && i.JWKSFetchedAt != nil
}

// Signature returns a stable SHA-256 content fingerprint covering every field
// that affects the live JwksConfig for this issuer. An unchanged signature means
// the registry entry and its cached JWKS keys can be kept as-is.
//
// JWKSKeys and JWKSFetchedAt are deliberately excluded. They are cached key
// material, not configuration: including them would make every refresh look like
// a config change, so every replica would rebuild its JwksConfig and discard the
// keys it had just fetched.
func (i Issuer) Signature() string {
	payload := struct {
		URL           string         `json:"u"`
		JWKS          string         `json:"j"`
		Origin        string         `json:"o"`
		Timeout       string         `json:"t"`
		SA            bool           `json:"s"`
		Aud           []string       `json:"a"`
		Scp           []string       `json:"c"`
		ClaimMappings []ClaimMapping `json:"m"`
		Source        string         `json:"src"`
	}{
		URL:           i.IssuerURL,
		JWKS:          i.JWKSUrl,
		Origin:        i.Origin,
		Timeout:       i.JWKSTimeout,
		SA:            i.ServiceAccount,
		Aud:           i.Audiences,
		Scp:           i.Scopes,
		ClaimMappings: i.ClaimMappings,
		Source:        i.Source,
	}
	b, err := json.Marshal(payload)
	if err != nil {
		return i.UpdatedAt.String() + ":" + i.ID.String()
	}
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}
