// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/viper"
)

// DefaultIssuerReloadInterval is how often every replica rebuilds the DB-sourced
// portion of the live auth registry, and so the convergence floor across replicas.
const DefaultIssuerReloadInterval = 30 * time.Second

// maxConcurrentJWKSRefreshes caps outbound fetches and cache writes started by
// one replica during a refresh pass.
const maxConcurrentJWKSRefreshes = 8

const (
	// DefaultJWKSRefreshInterval is how often each replica re-fetches every
	// DB-sourced issuer's key set. It must stay well under the shortest key
	// rotation window any configured IdP uses.
	DefaultJWKSRefreshInterval = 15 * time.Minute

	// DefaultJWKSPendingRetryInterval is how often an issuer that has no key set yet
	// is retried, so registering one whose identity provider is unreachable becomes
	// usable soon after the provider comes back rather than at the next full refresh.
	// It is also the floor: minUpdateInterval in auth/pkg/config throttles refreshes
	// to 10 s, so a shorter loop would fetch nothing. An issuer with no keys has
	// never installed a set, so the throttle does not apply to it at all.
	DefaultJWKSPendingRetryInterval = 10 * time.Second
)

// IssuerOrgMappingLockKey serializes everything that can change which issuers the
// registry trusts: the create and delete handlers, and the resolver's publication.
const IssuerOrgMappingLockKey = "issuer:organization-mapping-namespace"

// ErrIssuerStoreUnavailable distinguishes datastore failures from semantic
// issuer validation errors at API boundaries.
var ErrIssuerStoreUnavailable = errors.New("issuer datastore unavailable")

// staticIssuers returns the ConfigMap issuers, which were validated at load. A
// decode failure here leaves the registry trusting only what it already holds.
func (c *Config) staticIssuers() []IssuerConfig {
	issuers, err := c.GetIssuersConfig()
	if err != nil {
		log.Error().Err(err).Msg("Failed to read static issuers configuration")
		return nil
	}
	return issuers
}

type staticIssuerSnapshot struct {
	configs    []IssuerConfig
	issuerURLs map[string]bool
	jwksURLs   map[string]bool
}

func (c *Config) snapshotStaticIssuers() staticIssuerSnapshot {
	configs := c.staticIssuers()
	snapshot := staticIssuerSnapshot{
		configs:    configs,
		issuerURLs: make(map[string]bool, len(configs)),
		jwksURLs:   make(map[string]bool, len(configs)),
	}
	for _, issuer := range configs {
		snapshot.issuerURLs[issuer.Issuer] = true
		snapshot.jwksURLs[issuer.JWKS] = true
	}
	return snapshot
}

func (snapshot staticIssuerSnapshot) conflicts(issuerURL, jwksURL string) bool {
	return issuerURL != "" && snapshot.issuerURLs[issuerURL] ||
		jwksURL != "" && snapshot.jwksURLs[jwksURL]
}

// IsStaticIssuer reports whether a static ConfigMap issuer already claims this
// issuer URL (the token "iss") or JWKS URL. Static issuers always win: a DB issuer
// colliding on either is rejected at write time and ignored at load time. Empty
// arguments are skipped.
func (c *Config) IsStaticIssuer(issuerURL, jwksURL string) bool {
	return c.snapshotStaticIssuers().conflicts(issuerURL, jwksURL)
}

// HasPrivilegedStaticIssuerOrigins reports whether any ConfigMap issuer uses a
// privileged origin (keycloak, kas-legacy, or kas-ssa). Those IdPs own claim
// extraction, so runtime custom issuers may only be added when the static set is
// custom-only or empty.
func (c *Config) HasPrivilegedStaticIssuerOrigins() bool {
	for _, ic := range c.staticIssuers() {
		origin, err := ic.GetOrigin()
		if err != nil {
			log.Error().Err(err).Str("issuer", ic.Issuer).
				Msg("static issuer origin is unparseable; treating it as privileged and disabling dynamic issuers")
			return true
		}
		switch origin {
		case cauth.TokenOriginKeycloak, cauth.TokenOriginKasLegacy, cauth.TokenOriginKasSsa:
			return true
		}
	}
	return false
}

// DynamicIssuersEnabled reports whether this deployment manages issuers through the
// issuer table. It is the single condition behind the whole feature — the routes,
// the resolver, the startup reload, and both background loops — so a replica can
// never serve issuer writes it does not load, or maintain a registry nothing can
// write to. Off when Keycloak is on, when a privileged ConfigMap origin owns
// claim extraction, or when the deployment is not disconnected
func (c *Config) DynamicIssuersEnabled() bool {
	return c.GetEnvDisconnected() && !c.GetKeycloakEnabled() && !c.HasPrivilegedStaticIssuerOrigins()
}

// NewConfigFromYAML builds a Config from an in-memory YAML document, for tests that
// need a controlled issuers/keycloak block without an on-disk config.yaml.
func NewConfigFromYAML(yamlDoc string) (*Config, error) {
	v := viper.New()
	v.SetConfigType("yaml")
	v.SetDefault(ConfigKeycloakEnabled, false)
	if err := v.ReadConfig(strings.NewReader(yamlDoc)); err != nil {
		return nil, err
	}
	return &Config{
		v:                   v,
		DynamicIssuerConfig: newDynamicIssuerConfig(),
	}, nil
}

// ValidateCombinedIssuers validates a candidate DB issuer against the static
// ConfigMap issuers plus every other DB issuer, reusing the rules in
// ValidateIssuersConfig. excludeID, if set, drops that existing row so it cannot
// conflict with itself; candidate may be nil to validate the set as it stands.
func (c *Config) ValidateCombinedIssuers(ctx context.Context, dbSession *cdb.Session, tx *cdb.Tx, candidate *cdbm.Issuer, excludeID *uuid.UUID) error {
	return c.validateCombinedIssuers(ctx, dbSession, tx, candidate, excludeID, c.snapshotStaticIssuers().configs)
}

func (c *Config) validateCombinedIssuers(
	ctx context.Context,
	dbSession *cdb.Session,
	tx *cdb.Tx,
	candidate *cdbm.Issuer,
	excludeID *uuid.UUID,
	staticConfigs []IssuerConfig,
) error {
	dao := cdbm.NewIssuerDAO(dbSession)
	existing, err := dao.GetAll(ctx, tx, cdbm.IssuerFilterInput{})
	if err != nil {
		return fmt.Errorf("%w: %v", ErrIssuerStoreUnavailable, err)
	}

	combined := append([]IssuerConfig{}, staticConfigs...)
	for _, di := range existing {
		if excludeID != nil && di.ID == *excludeID {
			continue
		}
		combined = append(combined, issuerConfigFromDB(di))
	}
	if candidate != nil {
		combined = append(combined, issuerConfigFromDB(*candidate))
	}

	return c.ValidateIssuersConfig(combined)
}

// NewIssuerLoopContext returns the context the issuer background work runs on and
// arms Close to cancel it. The loops outlive every request, so they cannot borrow a
// request context.
func (c *Config) NewIssuerLoopContext(parent context.Context) context.Context {
	state := c.DynamicIssuerConfig
	state.mu.Lock()
	defer state.mu.Unlock()

	if state.stopLoops != nil {
		state.stopLoops()
	}

	ctx, cancel := context.WithCancel(parent)
	state.stopLoops = cancel

	return ctx
}

// StartIssuerReloadLoop periodically calls ReloadDBIssuers, so a write on one
// replica converges everywhere within one interval.
func (c *Config) StartIssuerReloadLoop(ctx context.Context, dbSession *cdb.Session, interval time.Duration) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	if interval <= 0 {
		interval = DefaultIssuerReloadInterval
	}
	state := c.DynamicIssuerConfig
	state.loops.Add(1)
	go func() {
		defer state.loops.Done()
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if err := c.ReloadDBIssuers(ctx, dbSession); err != nil {
					log.Warn().Err(err).Msg("periodic DB issuer reload failed")
				}
			}
		}
	}()
}

// InstallIssuerResolver teaches the auth registry to look an unknown issuer up in
// the issuer table, so a token minted against an issuer created seconds ago is not
// rejected for up to one reload interval.
//
// Resolution reads the database and nothing else: the issuer joins the registry
// with the key set its row already carries, and a row with no cached keys is
// published keyless just as the create path leaves it. It applies the same
// acceptance rules as ReloadDBIssuers, and publishes under the issuer lock so a
// resolution racing a deletion cannot re-add withdrawn trust.
func (c *Config) InstallIssuerResolver(dbSession *cdb.Session) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return
	}

	reg.SetIssuerResolver(func(ctx context.Context, issuerURL string) (*cauth.JwksConfig, error) {
		di, err := c.findAcceptableDBIssuer(ctx, dbSession, issuerURL)
		if di == nil || err != nil {
			return nil, err
		}

		jwksCfg := buildJwksConfig(*di)

		published, perr := c.publishResolvedIssuer(ctx, dbSession, reg, di.ID, jwksCfg)
		if perr != nil || !published {
			return nil, perr
		}
		return jwksCfg, nil
	})
}

// publishResolvedIssuer re-reads the candidate row and installs it in the registry
// while holding the issuer lock. The candidate read happens without the lock, so
// this re-read is where a deletion that committed in the meantime is noticed, and
// publishing under the lock is what orders this against the delete handler.
func (c *Config) publishResolvedIssuer(
	ctx context.Context,
	dbSession *cdb.Session,
	reg *cauth.TokenOriginConfig,
	id uuid.UUID,
	jwksCfg *cauth.JwksConfig,
) (bool, error) {
	return cdb.WithTxResult(ctx, dbSession, func(tx *cdb.Tx) (bool, error) {
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(IssuerOrgMappingLockKey), true)
		if derr != nil {
			return false, derr
		}

		live, derr := cdbm.NewIssuerDAO(dbSession).GetByID(ctx, tx, id)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				log.Warn().Str("issuer", jwksCfg.Issuer).
					Msg("on-demand issuer was deleted while it was being resolved; not publishing it")
				return false, nil
			}
			return false, derr
		}

		// The write path's rule applied to a row that already exists, so a manually
		// inserted or legacy conflicting row cannot enter through the token path.
		staticIssuers := c.snapshotStaticIssuers()
		verr := c.validateCombinedIssuers(ctx, dbSession, tx, live, &live.ID, staticIssuers.configs)
		if verr != nil {
			log.Warn().Err(verr).Str("issuer", live.IssuerURL).
				Msg("on-demand issuer failed validation against the combined issuer set; refusing")
			return false, nil
		}

		// Bookkeeping and the registry entry move together under mu: ReloadDBIssuers
		// reads both under the same lock, so it must never observe the URL as managed
		// while the registry has no config for it, or it installs a second, keyless
		// config that this publication then overwrites.
		state := c.DynamicIssuerConfig
		state.mu.Lock()
		defer state.mu.Unlock()
		state.dbURLs[live.IssuerURL] = true
		state.dbSigs[live.IssuerURL] = live.Signature()
		state.dbIDs[live.IssuerURL] = live.ID

		// Reserve before publishing, so the org this row owns is never claimable by a
		// ConfigMap dynamic mapping while the row is already live.
		c.reserveOrgNames(reg, computeReservedOrgNames([]IssuerConfig{issuerConfigFromDB(*live)}), staticIssuers.configs)
		reg.AddJwksConfig(jwksCfg)
		return true, nil
	})
}

// findAcceptableDBIssuer returns the issuer row for issuerURL if it passes the
// cheap checks that need no lock, or nil when there is no usable row. The
// combined-set validation happens later, under the lock, in publishResolvedIssuer.
func (c *Config) findAcceptableDBIssuer(ctx context.Context, dbSession *cdb.Session, issuerURL string) (*cdbm.Issuer, error) {
	staticIssuers := c.snapshotStaticIssuers()
	if staticIssuers.conflicts(issuerURL, "") {
		return nil, nil
	}

	rows, err := cdbm.NewIssuerDAO(dbSession).GetAll(ctx, nil, cdbm.IssuerFilterInput{IssuerURL: &issuerURL})
	if err != nil {
		return nil, err
	}
	if len(rows) == 0 {
		return nil, nil
	}

	di := rows[0]

	// The "iss" is free, but the ConfigMap may have grown an issuer claiming this
	// row's JWKS URL since it was written.
	if staticIssuers.conflicts("", di.JWKSUrl) {
		log.Warn().Str("issuer", di.IssuerURL).Str("jwks", di.JWKSUrl).
			Msg("on-demand issuer conflicts with a static issuer's JWKS URL; refusing (static wins)")
		return nil, nil
	}

	if di.HasDynamicMapping() {
		log.Warn().Str("issuer", di.IssuerURL).
			Msg("on-demand issuer has a dynamic org mapping (orgAttribute); refusing — dynamic issuers must be defined in the ConfigMap")
		return nil, nil
	}

	return &di, nil
}

// StartJWKSRefreshLoop periodically re-fetches every DB-sourced issuer's key set
// and writes each success back, so a replica that restarts later starts from a
// recent snapshot. The first pass waits one interval; a restarting replica
// hydrates from the row, and an unknown kid on the request path fetches immediately.
func (c *Config) StartJWKSRefreshLoop(ctx context.Context, dbSession *cdb.Session, interval time.Duration) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	if interval <= 0 {
		interval = DefaultJWKSRefreshInterval
	}
	state := c.DynamicIssuerConfig
	state.loops.Add(1)
	go func() {
		defer state.loops.Done()
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				c.refreshJWKS(ctx, dbSession, false)
			}
		}
	}()
}

// StartJWKSPendingRetryLoop periodically retries only the DB-sourced issuers that
// have no key set yet. Registration deliberately does not contact the identity
// provider, so a new issuer is live but unusable until some fetch succeeds; waiting
// a full refresh interval for that would make an issuer created during a brief
// provider outage look broken for minutes.
func (c *Config) StartJWKSPendingRetryLoop(ctx context.Context, dbSession *cdb.Session, interval time.Duration) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	if interval <= 0 {
		interval = DefaultJWKSPendingRetryInterval
	}
	state := c.DynamicIssuerConfig
	state.loops.Add(1)
	go func() {
		defer state.loops.Done()
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				c.refreshJWKS(ctx, dbSession, true)
			}
		}
	}()
}

// refreshJWKS refreshes DB-managed issuers concurrently, so one slow IdP delays
// only its own issuer. Each success is persisted best-effort. With pendingOnly,
// issuers that already hold a key set are left to the ordinary refresh interval.
func (c *Config) refreshJWKS(ctx context.Context, dbSession *cdb.Session, pendingOnly bool) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return
	}

	state := c.DynamicIssuerConfig
	state.mu.Lock()
	targets := make(map[string]uuid.UUID, len(state.dbIDs))
	for url, id := range state.dbIDs {
		targets[url] = id
	}
	state.mu.Unlock()

	var wg sync.WaitGroup
	refreshSlots := make(chan struct{}, maxConcurrentJWKSRefreshes)
	for url, id := range targets {
		jwksCfg := reg.GetConfig(url)
		if jwksCfg == nil {
			continue
		}
		if pendingOnly && jwksCfg.KeyCount() > 0 {
			continue
		}

		select {
		case refreshSlots <- struct{}{}:
		case <-ctx.Done():
			wg.Wait()
			return
		}

		wg.Add(1)
		go func(url string, id uuid.UUID, jwksCfg *cauth.JwksConfig) {
			defer wg.Done()
			defer func() { <-refreshSlots }()

			raw, fetchedAt, fetched, err := jwksCfg.RefreshJWKS(ctx)
			if err != nil {
				ev := log.Warn()
				if errors.Is(err, context.Canceled) {
					ev = log.Debug()
				}
				ev.Err(err).Str("issuer", url).Msg("JWKS refresh failed; keeping the currently loaded keys")
				return
			}
			if !fetched {
				return
			}
			persistJWKS(ctx, dbSession, id, url, raw, fetchedAt)
		}(url, id, jwksCfg)
	}
	wg.Wait()
}

// ReloadDBIssuers idempotently rebuilds the DB-sourced portion of the live registry
// from the issuer table alone, performing no network I/O. On the first call every
// row is built; afterwards only changed or new issuers are rebuilt and deleted ones
// removed, preserving cached keys. Static ConfigMap issuers are never touched.
func (c *Config) ReloadDBIssuers(ctx context.Context, dbSession *cdb.Session) error {
	if !c.DynamicIssuersEnabled() {
		return nil
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return nil
	}

	state := c.DynamicIssuerConfig
	state.mu.Lock()
	defer state.mu.Unlock()

	dbIssuers, err := cdbm.NewIssuerDAO(dbSession).GetAll(ctx, nil, cdbm.IssuerFilterInput{})
	if err != nil {
		return err
	}

	prevManaged := state.dbURLs
	staticIssuers := c.snapshotStaticIssuers()

	// Accept rows that collide with no static issuer and keep the combined set
	// valid; one bad row never blocks the others. A rejected row is ignored, never
	// deleted — staying out of the registry also keeps it out of the refresh loop.
	// Dynamic org mappings (orgAttribute) are a cross-tenant escalation risk and
	// must live in the ConfigMap.
	acceptedConfigs := append([]IssuerConfig{}, staticIssuers.configs...)
	acceptedDB := make([]cdbm.Issuer, 0, len(dbIssuers))
	for _, di := range dbIssuers {
		if staticIssuers.conflicts(di.IssuerURL, di.JWKSUrl) ||
			(c.TokenOriginConfig.GetConfig(di.IssuerURL) != nil && !prevManaged[di.IssuerURL]) {
			log.Warn().Str("issuer", di.IssuerURL).Str("jwks", di.JWKSUrl).
				Msg("DB issuer conflicts with a static/built-in issuer; ignoring the row (static wins)")
			continue
		}
		if di.HasDynamicMapping() {
			log.Warn().Str("issuer", di.IssuerURL).
				Msg("DB issuer has a dynamic org mapping (orgAttribute); skipping — dynamic issuers must be defined in the ConfigMap")
			continue
		}
		trial := append(append([]IssuerConfig{}, acceptedConfigs...), issuerConfigFromDB(di))
		if verr := c.ValidateIssuersConfig(trial); verr != nil {
			log.Warn().Err(verr).Str("issuer", di.IssuerURL).
				Msg("DB issuer failed validation against the current issuer set; skipping")
			continue
		}
		acceptedConfigs = trial
		acceptedDB = append(acceptedDB, di)
	}

	newManaged := make(map[string]bool, len(acceptedDB))
	newSigs := make(map[string]string, len(acceptedDB))
	newIDs := make(map[string]uuid.UUID, len(acceptedDB))
	for _, di := range acceptedDB {
		sig := di.Signature()
		newManaged[di.IssuerURL] = true
		newSigs[di.IssuerURL] = sig
		newIDs[di.IssuerURL] = di.ID

		// Unchanged and already live: keep the in-memory keys, but adopt the persisted
		// set when another replica has fetched a newer one. Without this a replica
		// whose signature never changes would reject tokens signed by current keys
		// sitting in the DB whenever the IdP is unreachable.
		live := reg.GetConfig(di.IssuerURL)
		if state.dbSigs[di.IssuerURL] == sig && live != nil {
			if di.JWKSFetchedAt != nil && di.JWKSFetchedAt.After(live.LastFetchedAt()) {
				hydrateFromCache(live, di)
			}
			continue
		}

		// No IdP contact here: hydrate from the row's cached key set, or register
		// keyless when it has none and let the first token naming it pay one fetch.
		reg.AddJwksConfig(buildJwksConfig(di))
	}

	// Static and Keycloak entries are never in prevManaged, so they are never removed.
	for url := range prevManaged {
		if !newManaged[url] {
			reg.RemoveConfig(url)
		}
	}

	state.dbURLs = newManaged
	state.dbSigs = newSigs
	state.dbIDs = newIDs

	// acceptedConfigs is the ConfigMap set plus every row that made it in, which is
	// exactly the set that owns org names.
	c.publishReservedOrgNames(reg, acceptedConfigs, staticIssuers.configs)
	return nil
}

// persistJWKS writes a freshly fetched key set back to the issuer row. It is
// best-effort: the keys are already installed in memory, so a failure only costs
// the next replica a fetch. fetchedAt is the time the fetch was issued, so the
// row's stale-write guard compares acquisition rather than completion order.
func persistJWKS(ctx context.Context, dbSession *cdb.Session, id uuid.UUID, issuerURL string, raw []byte, fetchedAt time.Time) {
	if len(raw) == 0 {
		return
	}
	err := cdbm.NewIssuerDAO(dbSession).UpdateJWKSCache(ctx, nil, id, raw, fetchedAt)
	if err != nil {
		log.Warn().Err(err).Str("issuer", issuerURL).
			Msg("failed to persist fetched JWKS key set; other replicas will fetch it themselves")
	}
}

// issuerConfigFromDB projects a DB Issuer onto the ConfigMap IssuerConfig shape, so
// the existing validator and registry-building logic are reused unchanged.
func issuerConfigFromDB(di cdbm.Issuer) IssuerConfig {
	return IssuerConfig{
		Origin:         di.Origin,
		JWKS:           di.JWKSUrl,
		Issuer:         di.IssuerURL,
		ServiceAccount: di.ServiceAccount,
		Audiences:      di.Audiences,
		Scopes:         di.Scopes,
		JWKSTimeout:    di.JWKSTimeout,
		ClaimMappings:  convertClaimMappings(di.ClaimMappings),
	}
}

// convertClaimMappings maps persisted claim mappings onto the in-memory ones,
// lowercasing static org names to match how ConfigMap issuers are normalized.
func convertClaimMappings(in []cdbm.ClaimMapping) []cauth.ClaimMapping {
	out := make([]cauth.ClaimMapping, len(in))
	for i, cm := range in {
		out[i] = cauth.ClaimMapping{
			OrgAttribute:        cm.OrgAttribute,
			OrgDisplayAttribute: cm.OrgDisplayAttribute,
			OrgName:             cm.OrgName,
			OrgDisplayName:      cm.OrgDisplayName,
			RolesAttribute:      cm.RolesAttribute,
			Roles:               cm.Roles,
			Audiences:           cm.Audiences,
			Scopes:              cm.Scopes,
			IsServiceAccount:    cm.IsServiceAccount,
		}
		if out[i].OrgName != "" {
			out[i].OrgName = strings.ToLower(out[i].OrgName)
		}
	}
	return out
}

// computeReservedOrgNames collects the lowercased static org names across the
// combined issuer set.
func computeReservedOrgNames(configs []IssuerConfig) map[string]bool {
	reserved := map[string]bool{}
	for _, ic := range configs {
		for _, cm := range ic.ClaimMappings {
			if cm.OrgName != "" {
				reserved[strings.ToLower(cm.OrgName)] = true
			}
		}
	}
	return reserved
}

// publishReservedOrgNames recomputes the statically owned org names across combined
// and republishes them to the ConfigMap issuers. Static ownership is what keeps a
// ConfigMap dynamic mapping from minting an org another issuer already owns, so a DB
// row's static org has to reserve its name as soon as the row is accepted. Only
// ConfigMap issuers can carry a dynamic mapping, so only they need the set.
func (c *Config) publishReservedOrgNames(reg *cauth.TokenOriginConfig, combined, staticConfigs []IssuerConfig) {
	reserved := computeReservedOrgNames(combined)
	for _, ic := range staticConfigs {
		if live := reg.GetConfig(ic.Issuer); live != nil {
			live.SetReservedOrgNames(reserved)
		}
	}
}

// reserveOrgNames adds orgs to what the ConfigMap issuers already reserve. The map is
// replaced rather than mutated, so a concurrent reader sees one complete set or the
// other. The next ReloadDBIssuers recomputes the set from the whole issuer set.
func (c *Config) reserveOrgNames(reg *cauth.TokenOriginConfig, orgs map[string]bool, staticConfigs []IssuerConfig) {
	if len(orgs) == 0 {
		return
	}
	for _, ic := range staticConfigs {
		live := reg.GetConfig(ic.Issuer)
		if live == nil {
			continue
		}
		current := live.GetReservedOrgNames()
		merged := make(map[string]bool, len(current)+len(orgs))
		for org := range current {
			merged[org] = true
		}
		for org := range orgs {
			merged[org] = true
		}
		live.SetReservedOrgNames(merged)
	}
}

// buildJwksConfig builds a live JwksConfig for a DB issuer, hydrated from the row's
// cached key set when it has one. DB issuers are always static-org (orgAttribute
// rows are filtered before this is called), so no reserved-org-names set is needed.
func buildJwksConfig(di cdbm.Issuer) *cauth.JwksConfig {
	origin := di.Origin
	if origin == "" {
		origin = cauth.TokenOriginCustom
	}
	cfg := cauth.NewJwksConfig(di.JWKSUrl, di.IssuerURL, origin, di.ServiceAccount, di.Audiences, di.Scopes)
	if d, err := time.ParseDuration(di.JWKSTimeout); err == nil {
		cfg.JWKSTimeout = d
	}
	cfg.ClaimMappings = convertClaimMappings(di.ClaimMappings)
	hydrateFromCache(cfg, di)
	return cfg
}

// hydrateFromCache installs the persisted key set on cfg when the row has one. An
// unusable blob is left in place rather than cleared, so an operator can still
// inspect it; the next successful fetch overwrites it.
func hydrateFromCache(cfg *cauth.JwksConfig, di cdbm.Issuer) {
	if !di.HasCachedKeys() {
		return
	}

	jwks, err := core.NewJWKSFromBytes(di.JWKSKeys)
	if err != nil {
		log.Warn().Err(err).Str("issuer", di.IssuerURL).
			Msg("cached JWKS key set is unusable; falling back to a network fetch")
		return
	}

	cfg.SetJWKSFromCache(jwks, *di.JWKSFetchedAt)
}
