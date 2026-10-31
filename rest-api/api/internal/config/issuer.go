// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net"
	"regexp"
	"slices"
	"strings"
	"sync"
	"time"

	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/roles"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/spf13/viper"
)

const (
	// DefaultIssuerReloadInterval is how often every replica rebuilds the DB-sourced
	// portion of the live auth registry, and so the convergence floor across replicas.
	DefaultIssuerReloadInterval = 30 * time.Second

	// maxConcurrentJWKSRefreshes caps outbound fetches and cache writes started by
	// one replica during a refresh pass.
	maxConcurrentJWKSRefreshes = 8

	// DefaultJWKSRefreshInterval is how old a DB-sourced issuer's key set may get
	// before the refresh loop re-fetches it.
	DefaultJWKSRefreshInterval = 15 * time.Minute

	// DefaultJWKSPendingRetryInterval is the refresh loop's tick: how often an issuer
	// with no key set is retried.
	DefaultJWKSPendingRetryInterval = 10 * time.Second

	// issuerReloadReadTimeout bounds the issuer table read made by each reload.
	issuerReloadReadTimeout = 10 * time.Second

	// maxIssuerStatusMessageBytes bounds what one refresh error or change summary may
	// store on the row and in its history.
	maxIssuerStatusMessageBytes = 1024

	// IssuerOrgMappingLockKey is the advisory lock held by every write that changes
	// which issuers are trusted.
	IssuerOrgMappingLockKey = "issuer:organization-mapping-namespace"

	jwksUnreachableSummary = "JWKS endpoint unreachable"

	// jwksReplicaFailureTTL drops a failure set entry its replica has not renewed,
	// such as one left by a pod that was deleted while failing. A failing replica
	// renews its entry every refresh tick.
	jwksReplicaFailureTTL = 5 * time.Minute
)

var (
	// ErrIssuerStoreUnavailable distinguishes datastore failures from semantic
	// issuer validation errors at API boundaries.
	ErrIssuerStoreUnavailable = errors.New("issuer datastore unavailable")

	jwksStatusCodePattern = regexp.MustCompile(`received status code (\d{3})`)
)

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

// StaticIssuers is a snapshot of the ConfigMap issuers, taken once per operation.
type StaticIssuers struct {
	configs []IssuerConfig
	index   map[string]int
}

// StaticIssuers reads the ConfigMap issuers once. See the type for why callers
// hold on to the result rather than calling again.
func (c *Config) StaticIssuers() StaticIssuers {
	configs := c.staticIssuers()
	snapshot := StaticIssuers{configs: configs, index: make(map[string]int, len(configs))}
	for i, issuer := range configs {
		snapshot.index[issuer.Issuer] = i
	}
	return snapshot
}

// Issuer returns the ConfigMap issuer whose issuer URL is issuerURL.
func (s StaticIssuers) Issuer(issuerURL string) (IssuerConfig, bool) {
	i, ok := s.index[issuerURL]
	if !ok {
		return IssuerConfig{}, false
	}
	return s.configs[i], true
}

// GrantsProviderAdmin reports whether a ConfigMap issuer grants PROVIDER_ADMIN to
// org through a static claim mapping.
func (s StaticIssuers) GrantsProviderAdmin(org string) bool {
	org = strings.ToLower(org)
	for _, ic := range s.configs {
		for _, cm := range ic.ClaimMappings {
			if cm.OrgName == "" || strings.ToLower(cm.OrgName) != org {
				continue
			}
			if cm.IsServiceAccount || slices.Contains(cm.Roles, roles.ProviderAdminRole) {
				return true
			}
		}
	}
	return false
}

// ClaimMappingKey returns a comparison key for a claim mapping that ignores org-name
// case, source, and nil versus empty lists.
func ClaimMappingKey(cm cdbm.ClaimMapping) string {
	cm.OrgName = strings.ToLower(cm.OrgName)
	cm.Source = ""
	b, err := json.Marshal(cm)
	if err != nil {
		return fmt.Sprintf("%+v", cm)
	}
	return string(b)
}

// IssuerInputFromConfig converts a ConfigMap issuer to the normalized input the
// ConfigMap sync writes.
func IssuerInputFromConfig(ic IssuerConfig) cdbm.IssuerCreateOrUpdateInput {
	origin, err := ic.GetOrigin()
	if err != nil {
		origin = strings.ToLower(ic.Origin)
	}
	mappings := make([]cdbm.ClaimMapping, len(ic.ClaimMappings))
	for i, cm := range ic.ClaimMappings {
		mappings[i] = cdbm.ClaimMapping{
			OrgAttribute:        cm.OrgAttribute,
			OrgDisplayAttribute: cm.OrgDisplayAttribute,
			OrgName:             strings.ToLower(cm.OrgName),
			OrgDisplayName:      cm.OrgDisplayName,
			RolesAttribute:      cm.RolesAttribute,
			Roles:               cm.Roles,
			Audiences:           cm.Audiences,
			Scopes:              cm.Scopes,
			IsServiceAccount:    cm.IsServiceAccount,
			Source:              cdbm.IssuerSourceConfig,
		}
	}
	return cdbm.IssuerCreateOrUpdateInput{
		Origin:         origin,
		IssuerURL:      ic.Issuer,
		JWKSUrl:        ic.JWKS,
		JWKSTimeout:    ic.JWKSTimeout,
		ServiceAccount: ic.ServiceAccount,
		Audiences:      ic.Audiences,
		Scopes:         ic.Scopes,
		ClaimMappings:  mappings,
		Source:         cdbm.IssuerSourceConfig,
	}
}

// ValidateAPIClaimMapping checks that an api-sourced mapping is static, is not a
// service account, and grants Tenant roles only.
func ValidateAPIClaimMapping(cm cdbm.ClaimMapping) error {
	if cm.Source == cdbm.IssuerSourceConfig {
		return nil
	}
	if cm.OrgAttribute != "" || cm.OrgDisplayAttribute != "" || cm.RolesAttribute != "" {
		return fmt.Errorf("api claim mapping for org %s is attribute-driven (orgAttribute, orgDisplayAttribute or rolesAttribute)", cm.OrgName)
	}
	if cm.IsServiceAccount {
		return fmt.Errorf("api claim mapping for org %s is a service account", cm.OrgName)
	}
	for _, r := range cm.Roles {
		if !cauth.TenantRoles[r] {
			return fmt.Errorf("api claim mapping for org %s grants non-Tenant role %s", cm.OrgName, r)
		}
	}
	return nil
}

// ValidateAPIClaimMappings returns the first ValidateAPIClaimMapping error in mappings.
func ValidateAPIClaimMappings(mappings []cdbm.ClaimMapping) error {
	for _, cm := range mappings {
		err := ValidateAPIClaimMapping(cm)
		if err != nil {
			return err
		}
	}
	return nil
}

// GrantedOrgsAndRoles returns the orgs and roles granted by mappings.
func GrantedOrgsAndRoles(mappings []cdbm.ClaimMapping) (orgs, roles []string) {
	orgs, roles = []string{}, []string{}
	for _, cm := range mappings {
		orgs = append(orgs, cm.OrgName)
		granted := cm.Roles
		if cm.IsServiceAccount {
			granted = cauth.ServiceAccountRoles
		}
		for _, r := range granted {
			if !slices.Contains(roles, r) {
				roles = append(roles, r)
			}
		}
	}
	return orgs, roles
}

// IssuerChangeSummary describes the configuration differences between two versions
// of an issuer for its status history.
func IssuerChangeSummary(actor string, before, after cdbm.Issuer) string {
	var parts []string
	if before.Source != after.Source {
		parts = append(parts, fmt.Sprintf("source %s to %s", before.Source, after.Source))
	}
	if before.JWKSUrl != after.JWKSUrl {
		parts = append(parts, fmt.Sprintf("jwksUrl %q to %q", before.JWKSUrl, after.JWKSUrl))
	}
	if before.JWKSTimeout != after.JWKSTimeout {
		parts = append(parts, fmt.Sprintf("jwksTimeout %q to %q", before.JWKSTimeout, after.JWKSTimeout))
	}
	if before.ServiceAccount != after.ServiceAccount {
		parts = append(parts, fmt.Sprintf("serviceAccount %t to %t", before.ServiceAccount, after.ServiceAccount))
	}
	if !slices.Equal(before.Audiences, after.Audiences) {
		parts = append(parts, fmt.Sprintf("audiences %v to %v", before.Audiences, after.Audiences))
	}
	if !slices.Equal(before.Scopes, after.Scopes) {
		parts = append(parts, fmt.Sprintf("scopes %v to %v", before.Scopes, after.Scopes))
	}
	summarize := func(cm cdbm.ClaimMapping) string {
		s := cm.Source + " claim mapping for org " + cm.OrgName
		if cm.OrgAttribute != "" {
			s = cm.Source + " claim mapping for org attribute " + cm.OrgAttribute
		}
		switch {
		case cm.IsServiceAccount:
			s += " (service account)"
		case cm.RolesAttribute != "":
			s += " roles from " + cm.RolesAttribute
		default:
			s += fmt.Sprintf(" roles %v", cm.Roles)
		}
		if len(cm.Audiences) > 0 {
			s += fmt.Sprintf(" audiences %v", cm.Audiences)
		}
		if len(cm.Scopes) > 0 {
			s += fmt.Sprintf(" scopes %v", cm.Scopes)
		}
		return s
	}
	keys := func(in []cdbm.ClaimMapping) map[string]bool {
		out := make(map[string]bool, len(in))
		for _, cm := range in {
			out[ClaimMappingKey(cm)] = true
		}
		return out
	}
	beforeKeys, afterKeys := keys(before.ClaimMappings), keys(after.ClaimMappings)
	for _, cm := range before.ClaimMappings {
		if !afterKeys[ClaimMappingKey(cm)] {
			parts = append(parts, "removed "+summarize(cm))
		}
	}
	for _, cm := range after.ClaimMappings {
		if !beforeKeys[ClaimMappingKey(cm)] {
			parts = append(parts, "added "+summarize(cm))
		}
	}
	// Keys ignore mapping source and order; the signature does not.
	if len(parts) == 0 && before.Signature() != after.Signature() {
		parts = append(parts, "claim mapping source or order changed")
	}
	if len(parts) == 0 {
		return actor + ": no configuration change"
	}
	return truncateStatusMessage(actor + ": " + strings.Join(parts, "; "))
}

// jwksErrorSummary maps a key fetch error to a fixed message safe to show API callers.
func jwksErrorSummary(err error) string {
	var netErr net.Error
	switch {
	case errors.Is(err, context.DeadlineExceeded), errors.As(err, &netErr) && netErr.Timeout():
		return "JWKS fetch timed out"
	case errors.Is(err, core.ErrJWKSTooLarge):
		return "JWKS response exceeds the maximum allowed size"
	case errors.Is(err, core.ErrEmptyKeySet), errors.Is(err, core.ErrNoValidKeys), errors.Is(err, core.ErrInvalidJWK):
		return "JWKS response is not a usable key set"
	case errors.Is(err, core.ErrJWKSFetch):
		if m := jwksStatusCodePattern.FindStringSubmatch(err.Error()); m != nil {
			return "JWKS endpoint returned HTTP " + m[1]
		}
		return jwksUnreachableSummary
	}
	return "JWKS fetch failed"
}

func truncateStatusMessage(s string) string {
	if len(s) <= maxIssuerStatusMessageBytes {
		return s
	}
	return s[:maxIssuerStatusMessageBytes-3] + "..."
}

// RecordIssuerHistory appends one status_detail entry for an issuer.
func RecordIssuerHistory(ctx context.Context, dbSession *cdb.Session, tx *cdb.Tx, id uuid.UUID, status, message string) error {
	msg := truncateStatusMessage(message)
	_, err := cdbm.NewStatusDetailDAO(dbSession).Create(ctx, tx, cdbm.StatusDetailCreateInput{
		EntityID: id.String(),
		Status:   status,
		Message:  &msg,
	})
	return err
}

// SyncConfigMapIssuers reconciles the issuer table with the ConfigMap: it inserts
// missing issuers, updates config rows, takes over api rows the ConfigMap now
// defines, and soft-deletes config rows the ConfigMap no longer defines. It leaves
// the table alone when a newer ConfigMap revision has synced, and returns
// ErrIssuerIdentityConflict when the ConfigMap overlaps an api issuer.
func (c *Config) SyncConfigMapIssuers(ctx context.Context, dbSession *cdb.Session) error {
	if !c.DynamicIssuersEnabled() {
		return nil
	}
	static := c.staticIssuers()

	return cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		if err := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(IssuerOrgMappingLockKey), true); err != nil {
			return err
		}
		dao := cdbm.NewIssuerDAO(dbSession)
		rows, err := dao.GetAll(ctx, tx, cdbm.IssuerFilterInput{})
		if err != nil {
			return fmt.Errorf("%w: %v", ErrIssuerStoreUnavailable, err)
		}

		revision := c.GetConfigRevision()
		var configRevision *int
		if revision > 0 {
			configRevision = &revision
			tableRevision, rerr := dao.MaxConfigRevision(ctx, tx)
			if rerr != nil {
				return fmt.Errorf("%w: %v", ErrIssuerStoreUnavailable, rerr)
			}
			if tableRevision > revision && !c.GetConfigRevisionReset() {
				log.Warn().Int("revision", revision).Int("table_revision", tableRevision).
					Msg("the issuer table reflects a newer ConfigMap revision than this replica runs; leaving it as it is")
				return nil
			}
		}
		byURL := make(map[string]cdbm.Issuer, len(rows))
		for _, row := range rows {
			byURL[row.IssuerURL] = row
		}
		configURLs := make(map[string]bool, len(static))
		configOrgOwner := map[string]string{} // lowercased org -> ConfigMap issuer URL
		configJWKSOwner := map[string]string{}
		for _, ic := range static {
			configURLs[ic.Issuer] = true
			configJWKSOwner[ic.JWKS] = ic.Issuer
			for _, cm := range ic.ClaimMappings {
				if cm.OrgName != "" {
					configOrgOwner[strings.ToLower(cm.OrgName)] = ic.Issuer
				}
			}
		}

		// Cross-issuer overlap: fail before writing anything.
		for _, row := range rows {
			if row.Source != cdbm.IssuerSourceAPI || configURLs[row.IssuerURL] {
				continue
			}
			for _, cm := range row.ClaimMappings {
				if owner, taken := configOrgOwner[strings.ToLower(cm.OrgName)]; taken {
					return fmt.Errorf("%w: ConfigMap issuer %s maps org %s, which API-managed issuer %s (id %s) already maps; "+
						"an org may belong to exactly one issuer. Remove the org from the ConfigMap issuer, or delete it from the API issuer before restarting",
						ErrIssuerIdentityConflict, owner, cm.OrgName, row.IssuerURL, row.ID)
				}
			}
			if owner, taken := configJWKSOwner[row.JWKSUrl]; taken {
				return fmt.Errorf("%w: ConfigMap issuer %s uses JWKS URL %s, which API-managed issuer %s (id %s) already uses",
					ErrIssuerIdentityConflict, owner, row.JWKSUrl, row.IssuerURL, row.ID)
			}
		}

		for _, ic := range static {
			in := IssuerInputFromConfig(ic)
			in.ConfigRevision = configRevision
			existing, ok := byURL[ic.Issuer]
			if !ok {
				created, cerr := dao.Create(ctx, tx, in)
				if cerr != nil {
					return cerr
				}
				if herr := RecordIssuerHistory(ctx, dbSession, tx, created.ID, created.Status, "ConfigMap sync: created from the ConfigMap"); herr != nil {
					return herr
				}
				continue
			}

			// Keep an api mapping only if it still fits beside the ConfigMap's mappings.
			for _, cm := range existing.ClaimMappings {
				if cm.Source != cdbm.IssuerSourceAPI || ValidateAPIClaimMapping(cm) != nil {
					continue
				}
				org := strings.ToLower(cm.OrgName)
				if owner, taken := configOrgOwner[org]; taken && owner != ic.Issuer {
					continue
				}
				fits := true
				for _, ccm := range in.ClaimMappings {
					if strings.ToLower(ccm.OrgName) != org {
						continue
					}
					fits = fits && len(cm.Audiences) > 0 && len(ccm.Audiences) > 0
					for _, aud := range cm.Audiences {
						fits = fits && !slices.Contains(ccm.Audiences, aud)
					}
				}
				if fits {
					in.ClaimMappings = append(in.ClaimMappings, cm)
				}
			}

			candidate := in.ToIssuer()
			candidate.ID = existing.ID
			if existing.Signature() == candidate.Signature() {
				continue
			}
			jwksChanged := existing.JWKSUrl != candidate.JWKSUrl
			if existing.Source == cdbm.IssuerSourceAPI {
				msg := IssuerChangeSummary("superseded by ConfigMap issuer", existing, candidate)
				if herr := RecordIssuerHistory(ctx, dbSession, tx, existing.ID, cdbm.IssuerStatusSuperseded, msg); herr != nil {
					return herr
				}
			}
			updated, uerr := dao.Update(ctx, tx, existing.ID, in, jwksChanged)
			if uerr != nil {
				return uerr
			}
			if jwksChanged {
				if _, serr := dao.UpdateStatus(ctx, tx, existing.ID, cdbm.IssuerStatusPending, nil); serr != nil {
					return serr
				}
				updated.Status = cdbm.IssuerStatusPending
			}
			if herr := RecordIssuerHistory(ctx, dbSession, tx, existing.ID, updated.Status, IssuerChangeSummary("ConfigMap sync", existing, candidate)); herr != nil {
				return herr
			}
		}

		// Stamped before the removals so a withdrawn row carries this revision too.
		if revision > 0 {
			if serr := dao.SetConfigRevision(ctx, tx, revision, c.GetConfigRevisionReset()); serr != nil {
				return serr
			}
		}

		for _, row := range rows {
			if row.Source != cdbm.IssuerSourceConfig || configURLs[row.IssuerURL] {
				continue
			}
			if herr := RecordIssuerHistory(ctx, dbSession, tx, row.ID, row.Status, "removed from the ConfigMap"); herr != nil {
				return herr
			}
			if derr := dao.Delete(ctx, tx, row.ID); derr != nil {
				return derr
			}
			orgs, grantedRoles := GrantedOrgsAndRoles(row.ClaimMappings)
			log.Warn().Str("event", "issuer_removed").Str("issuer", row.IssuerURL).Str("id", row.ID.String()).
				Strs("withdrawn_orgs", orgs).Strs("withdrawn_roles", grantedRoles).
				Msg("the ConfigMap no longer defines this issuer; withdrew its row")
		}

		// The table as it now stands is what every replica will load. It must be a
		// valid issuer set, or startup stops here with the reason.
		return c.ValidateCombinedIssuers(ctx, dbSession, tx, nil, nil)
	})
}

// HasPrivilegedConfigMapIssuer reports whether any ConfigMap issuer has a keycloak,
// kas-legacy, or kas-ssa origin.
func (c *Config) HasPrivilegedConfigMapIssuer() bool {
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

// DynamicIssuersEnabled reports whether issuers are managed through the issuer table.
func (c *Config) DynamicIssuersEnabled() bool {
	return c.GetEnvDisconnected() && !c.GetKeycloakEnabled() && !c.HasPrivilegedConfigMapIssuer()
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

// ValidateCombinedIssuers validates candidate against every other live issuer row.
// excludeID skips that row; a nil candidate validates the stored set.
func (c *Config) ValidateCombinedIssuers(ctx context.Context, dbSession *cdb.Session, tx *cdb.Tx, candidate *cdbm.Issuer, excludeID *uuid.UUID) error {
	existing, err := cdbm.NewIssuerDAO(dbSession).GetAll(ctx, tx, cdbm.IssuerFilterInput{})
	if err != nil {
		return fmt.Errorf("%w: %v", ErrIssuerStoreUnavailable, err)
	}
	combined := make([]IssuerConfig, 0, len(existing)+1)
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

// NewIssuerLoopContext returns the context for issuer background work, which Close
// cancels.
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

// InstallIssuerResolver lets the auth registry load an issuer it does not have
// from the issuer table.
func (c *Config) InstallIssuerResolver(dbSession *cdb.Session) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return
	}

	reg.SetIssuerResolver(func(ctx context.Context, issuerURL string) (*cauth.JwksConfig, error) {
		// Unlocked existence check first: issuer names are unauthenticated input.
		if _, err := cdbm.NewIssuerDAO(dbSession).GetByIssuerURL(ctx, nil, issuerURL); err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return nil, nil
			}
			return nil, err
		}
		return c.resolveDBIssuer(ctx, dbSession, reg, issuerURL)
	})
}

// resolveDBIssuer reads the row for issuerURL under the issuer lock, validates it
// as reload does, and publishes it before releasing the lock.
func (c *Config) resolveDBIssuer(ctx context.Context, dbSession *cdb.Session, reg *cauth.TokenOriginConfig, issuerURL string) (*cauth.JwksConfig, error) {
	return cdb.WithTxResult(ctx, dbSession, func(tx *cdb.Tx) (*cauth.JwksConfig, error) {
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(IssuerOrgMappingLockKey), true)
		if derr != nil {
			return nil, derr
		}

		live, derr := cdbm.NewIssuerDAO(dbSession).GetByIssuerURL(ctx, tx, issuerURL)
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return nil, nil
			}
			return nil, derr
		}

		merr := ValidateAPIClaimMappings(live.ClaimMappings)
		if merr != nil {
			log.Warn().Err(merr).Str("issuer", live.IssuerURL).Msg("issuer row breaks the API's claim mapping rules; refusing")
			return nil, nil
		}
		verr := c.ValidateCombinedIssuers(ctx, dbSession, tx, live, &live.ID)
		if verr != nil {
			log.Warn().Err(verr).Str("issuer", live.IssuerURL).
				Msg("on-demand issuer failed validation against the combined issuer set; refusing")
			return nil, nil
		}

		// Publishing under the advisory lock also orders this against a concurrent
		// delete, so withdrawn trust is never re-added.
		state := c.DynamicIssuerConfig
		state.mu.Lock()
		defer state.mu.Unlock()
		return c.installRow(reg, *live), nil
	})
}

// installRow publishes a committed row to the registry and the managed set under
// state.mu, reserving its orgs before it goes live.
func (c *Config) installRow(reg *cauth.TokenOriginConfig, row cdbm.Issuer) *cauth.JwksConfig {
	state := c.DynamicIssuerConfig
	state.dbURLs[row.IssuerURL] = true
	state.dbSigs[row.IssuerURL] = row.Signature()
	state.dbIDs[row.IssuerURL] = row.ID

	reserved := computeReservedOrgNames([]IssuerConfig{issuerConfigFromDB(row)})
	for _, entry := range reg.GetAllConfigs() {
		maps.Copy(reserved, entry.GetReservedOrgNames())
	}
	setReservedOrgNames(reg, reserved)
	built := c.buildJwksConfig(row)
	built.SetReservedOrgNames(reserved)
	reg.AddJwksConfig(inheritKeys(reg.GetConfig(row.IssuerURL), built))
	return built
}

// PublishIssuerMutation applies a committed issuer write to this replica's registry
// before the handler responds.
func (c *Config) PublishIssuerMutation(ctx context.Context, dbSession *cdb.Session, row *cdbm.Issuer, deleted bool) {
	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), issuerReloadReadTimeout)
	defer cancel()
	err := c.ReloadDBIssuers(ctx, dbSession)
	if err == nil || !c.DynamicIssuersEnabled() || c.TokenOriginConfig == nil {
		return
	}
	log.Warn().Err(err).Str("issuer", row.IssuerURL).Bool("deleted", deleted).
		Msg("live registry reload failed after an issuer write; applying the committed row directly")

	reg, state := c.TokenOriginConfig, c.DynamicIssuerConfig
	if !deleted {
		installed, rerr := c.resolveDBIssuer(ctx, dbSession, reg, row.IssuerURL)
		if installed != nil {
			return
		}
		log.Warn().Err(rerr).Str("issuer", row.IssuerURL).Msg("issuer write not applied locally; withdrawing the issuer until the next reload")
	}
	// Withdraw even if the URL was recreated since; the next reload restores it.
	state.mu.Lock()
	defer state.mu.Unlock()
	reg.RemoveConfig(row.IssuerURL)
	delete(state.dbURLs, row.IssuerURL)
	delete(state.dbSigs, row.IssuerURL)
	delete(state.dbIDs, row.IssuerURL)
}

// StartJWKSRefreshLoop refreshes DB-sourced issuers' key sets on every tick and
// records each issuer's fetch status.
func (c *Config) StartJWKSRefreshLoop(ctx context.Context, dbSession *cdb.Session, tick, maxAge time.Duration) {
	if !c.DynamicIssuersEnabled() {
		return
	}
	if tick <= 0 {
		tick = DefaultJWKSPendingRetryInterval
	}
	if maxAge <= 0 {
		maxAge = DefaultJWKSRefreshInterval
	}
	state := c.DynamicIssuerConfig
	state.loops.Add(1)
	go func() {
		defer state.loops.Done()
		ticker := time.NewTicker(tick)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				c.refreshJWKS(ctx, dbSession, maxAge)
			}
		}
	}()
}

// refreshJWKS refreshes each issuer that has no key set, one older than maxAge, or
// whose last fetch on this replica failed, then records every issuer's status from
// its last fetch attempt.
func (c *Config) refreshJWKS(ctx context.Context, dbSession *cdb.Session, maxAge time.Duration) {
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

	dao := cdbm.NewIssuerDAO(dbSession)
	report := func(url string, id uuid.UUID, jwksCfg *cauth.JwksConfig) {
		at, ferr := jwksCfg.LastAttempt()
		if at.IsZero() {
			return
		}
		var fetchErr *string
		outcome := jwksRefreshOutcome{at: at}
		if ferr != nil {
			outcome.message = jwksErrorSummary(ferr)
			fetchErr = &outcome.message
		}
		state.mu.Lock()
		unchanged := state.reported[url] == outcome
		state.mu.Unlock()
		if unchanged {
			return
		}

		// The row and its history move in one transaction, so a transition is
		// never committed without its entry.
		err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
			row, rerr := dao.RecordJWKSAttempt(ctx, tx, id, jwksCfg.JWKSURL(), state.replica, at, fetchErr, time.Now().Add(-jwksReplicaFailureTTL))
			if rerr != nil || row == nil {
				return rerr
			}
			// Keys fetched at startup or on the token path reach the row here.
			if fetchedAt := jwksCfg.LastFetchedAt(); row.JWKSFetchedAt == nil || fetchedAt.After(*row.JWKSFetchedAt) {
				if jwks := jwksCfg.GetJWKS(); jwks != nil {
					raw, merr := jwks.Marshal()
					if merr != nil {
						return merr
					}
					if rerr = dao.UpdateJWKSCache(ctx, tx, id, jwksCfg.JWKSURL(), raw, fetchedAt); rerr != nil {
						return rerr
					}
				}
			}
			status, detail, msg := jwksStatus(jwksCfg, fetchErr, row.JWKSReplicaFailures)
			if _, rerr = dao.UpdateStatus(ctx, tx, id, status, msg); rerr != nil || row.Status == status {
				return rerr
			}
			return RecordIssuerHistory(ctx, dbSession, tx, id, status, detail)
		})
		if err != nil {
			log.Warn().Err(err).Str("issuer", url).Msg("failed to record JWKS refresh outcome")
			return
		}
		state.mu.Lock()
		state.reported[url] = outcome
		state.mu.Unlock()
	}

	var wg sync.WaitGroup
	refreshSlots := make(chan struct{}, maxConcurrentJWKSRefreshes)
	for url, id := range targets {
		jwksCfg := reg.GetConfig(url)
		if jwksCfg == nil {
			continue
		}
		_, lastErr := jwksCfg.LastAttempt()
		if lastErr == nil && jwksCfg.KeyCount() > 0 && maxAge > 0 && time.Since(jwksCfg.LastFetchedAt()) < maxAge {
			report(url, id, jwksCfg)
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

			if _, err := jwksCfg.RefreshJWKS(ctx); err != nil {
				ev := log.Warn()
				if errors.Is(err, context.Canceled) {
					ev = log.Debug()
				}
				ev.Err(err).Str("issuer", url).Msg("JWKS refresh failed; keeping the currently loaded keys")
			}
			// Also when this pass was throttled by a failed token-path fetch.
			if _, lastErr := jwksCfg.LastAttempt(); lastErr != nil {
				adoptPersistedKeys(ctx, dao, id, jwksCfg)
			}
			if ctx.Err() == nil {
				report(url, id, jwksCfg)
			}
		}(url, id, jwksCfg)
	}
	wg.Wait()
}

// ReloadDBIssuers reconciles the registry with the issuer table without contacting
// any IdP, rebuilding only new or changed issuers.
func (c *Config) ReloadDBIssuers(ctx context.Context, dbSession *cdb.Session) error {
	if !c.DynamicIssuersEnabled() {
		return nil
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return nil
	}

	// Never hold state.mu across the table read; see DynamicIssuerConfig.mu.
	state := c.DynamicIssuerConfig
	state.reloadMu.Lock()
	defer state.reloadMu.Unlock()

	state.mu.Lock()
	managedBefore := maps.Clone(state.dbURLs)
	state.mu.Unlock()

	rctx, cancel := context.WithTimeout(ctx, issuerReloadReadTimeout)
	defer cancel()
	dbIssuers, err := cdbm.NewIssuerDAO(dbSession).GetAll(rctx, nil, cdbm.IssuerFilterInput{})
	if err != nil {
		return err
	}

	// ConfigMap rows first, so an invalid row only excludes itself.
	slices.SortStableFunc(dbIssuers, func(a, b cdbm.Issuer) int {
		switch {
		case a.Source == b.Source:
			return 0
		case a.Source == cdbm.IssuerSourceConfig:
			return -1
		default:
			return 1
		}
	})
	configs := make([]IssuerConfig, 0, len(dbIssuers))
	accepted := make([]cdbm.Issuer, 0, len(dbIssuers))
	for _, di := range dbIssuers {
		merr := ValidateAPIClaimMappings(di.ClaimMappings)
		if merr != nil {
			log.Warn().Err(merr).Str("issuer", di.IssuerURL).Msg("issuer row breaks the API's claim mapping rules; skipping")
			continue
		}
		trial := append(slices.Clone(configs), issuerConfigFromDB(di))
		if verr := c.ValidateIssuersConfig(trial); verr != nil {
			log.Warn().Err(verr).Str("issuer", di.IssuerURL).
				Msg("DB issuer failed validation against the current issuer set; skipping")
			continue
		}
		configs = trial
		accepted = append(accepted, di)
	}

	state.mu.Lock()
	defer state.mu.Unlock()

	// Every row that follows is installed with the reserved set already applied, so
	// a ConfigMap row with a dynamic mapping is never live with nothing reserved.
	reserved := computeReservedOrgNames(configs)

	prevManaged := state.dbURLs
	newManaged := make(map[string]bool, len(accepted))
	newSigs := make(map[string]string, len(accepted))
	newIDs := make(map[string]uuid.UUID, len(accepted))
	for _, di := range accepted {
		sig := di.Signature()
		newManaged[di.IssuerURL] = true
		newSigs[di.IssuerURL] = sig
		newIDs[di.IssuerURL] = di.ID

		// Unchanged and already live: keep the entry and its in-memory keys.
		live := reg.GetConfig(di.IssuerURL)
		if state.dbSigs[di.IssuerURL] == sig && live != nil {
			continue
		}

		// Hydrate from the row or the replaced entry; never fetch here.
		built := c.buildJwksConfig(di)
		built.SetReservedOrgNames(reserved)
		reg.AddJwksConfig(inheritKeys(live, built))
	}

	for url := range prevManaged {
		if newManaged[url] {
			continue
		}
		// Published by the resolver after this reload's read; the next reload judges it.
		if !managedBefore[url] {
			newManaged[url], newSigs[url], newIDs[url] = true, state.dbSigs[url], state.dbIDs[url]
			if live := reg.GetConfig(url); live != nil {
				configs = append(configs, IssuerConfig{Issuer: url, ClaimMappings: live.GetClaimMappings()})
			}
			continue
		}
		reg.RemoveConfig(url)
		delete(state.reported, url)
	}

	state.dbURLs = newManaged
	state.dbSigs = newSigs
	state.dbIDs = newIDs

	// configs includes entries the resolver published after the read.
	reserved = computeReservedOrgNames(configs)
	for url, live := range reg.GetAllConfigs() {
		if !newManaged[url] {
			maps.Copy(reserved, computeReservedOrgNames([]IssuerConfig{{ClaimMappings: live.GetClaimMappings()}}))
		}
	}
	setReservedOrgNames(reg, reserved)
	return nil
}

// jwksStatus derives an issuer's status from this replica's key set and the
// replicas whose last fetch failed. An issuer is Degraded while any replica fails,
// and Ready once every replica has fetched again.
func jwksStatus(jwksCfg *cauth.JwksConfig, fetchErr *string, failures map[string]cdbm.JWKSReplicaFailure) (status, detail string, message *string) {
	switch {
	case jwksCfg.KeyCount() > 0 && jwksCfg.GetJWKS() == nil:
		s := "cached key set expired without a successful refresh"
		if fetchErr != nil {
			s += ": " + *fetchErr
		}
		return cdbm.IssuerStatusPending, s, &s
	case jwksCfg.KeyCount() == 0 && fetchErr != nil:
		return cdbm.IssuerStatusPending, "no JWKS key set fetched yet: " + *fetchErr, fetchErr
	case jwksCfg.KeyCount() == 0:
		return cdbm.IssuerStatusPending, "no JWKS key set fetched yet", nil
	case len(failures) > 0:
		names := slices.Sorted(maps.Keys(failures))
		latest := failures[names[0]]
		for _, name := range names[1:] {
			if failures[name].At.After(latest.At) {
				latest = failures[name]
			}
		}
		s := truncateStatusMessage(fmt.Sprintf("JWKS refresh failing on %d replica(s) (%s), serving the last fetched key set: %s",
			len(names), strings.Join(names, ", "), latest.Error))
		return cdbm.IssuerStatusDegraded, s, &s
	}
	return cdbm.IssuerStatusReady, "JWKS fetched", nil
}

// adoptPersistedKeys installs the row's key set when another replica fetched it
// after the one this replica holds, so a replica that cannot reach the IdP serves
// the keys a peer fetched.
func adoptPersistedKeys(ctx context.Context, dao cdbm.IssuerDAO, id uuid.UUID, jwksCfg *cauth.JwksConfig) {
	if ctx.Err() != nil {
		return
	}
	row, err := dao.GetByID(ctx, nil, id)
	if err != nil {
		log.Warn().Err(err).Str("issuer", jwksCfg.Issuer).Msg("failed to read persisted JWKS key set")
		return
	}
	if row.JWKSUrl != jwksCfg.JWKSURL() || !row.HasCachedKeys() || !row.JWKSFetchedAt.After(jwksCfg.LastFetchedAt()) {
		return
	}
	loadCachedJWKS(jwksCfg, *row)
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

// convertClaimMappings converts persisted claim mappings to auth claim mappings,
// lowercasing static org names.
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

// setReservedOrgNames sets reserved on every live registry entry.
func setReservedOrgNames(reg *cauth.TokenOriginConfig, reserved map[string]bool) {
	for _, live := range reg.GetAllConfigs() {
		live.SetReservedOrgNames(reserved)
	}
}

// buildJwksConfig builds a JwksConfig for a DB issuer, hydrated from its cached keys.
func (c *Config) buildJwksConfig(di cdbm.Issuer) *cauth.JwksConfig {
	origin := di.Origin
	if origin == "" {
		origin = cauth.TokenOriginCustom
	}
	cfg := cauth.NewJwksConfig(di.JWKSUrl, di.IssuerURL, origin, di.ServiceAccount, di.Audiences, di.Scopes)
	if d, err := time.ParseDuration(di.JWKSTimeout); err == nil {
		cfg.JWKSTimeout = d
	}
	cfg.ClaimMappings = convertClaimMappings(di.ClaimMappings)
	loadCachedJWKS(cfg, di)
	return cfg
}

// inheritKeys copies old's key set and last fetch outcome to cfg when both use the
// same JWKS URL, and returns cfg.
func inheritKeys(old, cfg *cauth.JwksConfig) *cauth.JwksConfig {
	if old == nil || old == cfg || old.URL != cfg.URL {
		return cfg
	}
	if jwks := old.GetJWKS(); jwks != nil {
		cfg.SetJWKSFromCache(jwks, old.LastFetchedAt())
	}
	cfg.InheritLastAttempt(old)
	return cfg
}

// loadCachedJWKS installs the row's cached key set on cfg, if it is usable.
func loadCachedJWKS(cfg *cauth.JwksConfig, di cdbm.Issuer) {
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
