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

// DefaultIssuerReloadInterval is how often every replica rebuilds the DB-sourced
// portion of the live auth registry, and so the convergence floor across replicas.
const DefaultIssuerReloadInterval = 30 * time.Second

// maxConcurrentJWKSRefreshes caps outbound fetches and cache writes started by
// one replica during a refresh pass.
const maxConcurrentJWKSRefreshes = 8

const (
	// DefaultJWKSRefreshInterval is how old a DB-sourced issuer's key set may get
	// before the refresh loop re-fetches it. It must stay well under the shortest
	// key rotation window any configured IdP uses.
	DefaultJWKSRefreshInterval = 15 * time.Minute

	// DefaultJWKSPendingRetryInterval is the refresh loop's tick: how often an
	// issuer that has no key set yet is retried, so registering one whose identity
	// provider is unreachable becomes usable soon after the provider comes back
	// rather than at the next full refresh. It is also the floor: minUpdateInterval
	// in auth/pkg/config throttles refreshes to 10 s, so a shorter tick would fetch
	// nothing. An issuer with no keys has never installed a set, so the throttle
	// does not apply to it at all.
	DefaultJWKSPendingRetryInterval = 10 * time.Second
)

// issuerReloadReadTimeout bounds the one DB read a reload makes. The read runs
// outside state.mu, but a bound still keeps a blocked table from pinning the
// reload goroutine for the whole outage.
const issuerReloadReadTimeout = 10 * time.Second

// maxIssuerStatusMessageBytes bounds what one refresh error or change summary may
// store on the row and in its history.
const maxIssuerStatusMessageBytes = 1024

// IssuerOrgMappingLockKey serializes everything that can change which issuers the
// registry trusts: the create and delete handlers, the ConfigMap sync, and the
// resolver's publication.
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

// StaticIssuers is the ConfigMap issuer set as one replica runs it. Callers take
// it once per operation and pass it down, so every decision in that operation
// sees the same set and the ConfigMap is decoded once. The ConfigMap issuers are
// also rows in the issuer table (see SyncConfigMapIssuers); this snapshot exists
// for the decisions that must not depend on the table, such as who may manage
// issuers at all.
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
// org through a static claim mapping, by role or as a service account. This is
// the anchor for who may manage issuers: the ConfigMap is operator-controlled
// and the API can never change it, whereas an Infrastructure Provider or a
// PROVIDER_ADMIN grant on an API issuer can be created by anyone who already
// holds that role somewhere. Dynamic (attribute-driven) mappings cannot be
// enumerated, so they do not count.
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

// ClaimMappingKey is a comparison key for one claim mapping: its JSON form with
// the org name lowercased and the source removed, so nil and empty lists, org-name
// case and who defined the mapping do not count as differences.
func ClaimMappingKey(cm cdbm.ClaimMapping) string {
	cm.OrgName = strings.ToLower(cm.OrgName)
	cm.Source = ""
	b, err := json.Marshal(cm)
	if err != nil {
		return fmt.Sprintf("%+v", cm)
	}
	return string(b)
}

// IssuerInputFromConfig projects a ConfigMap issuer onto the create-or-update
// input the sync writes: source config on the row and on every mapping, with the
// defaults and normalization the API write path applies, so ToIssuer().Signature()
// compares like for like with a stored row.
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

// APIRuleViolation names the first way row breaks the rules the API enforces on
// what it writes, or "" when it breaks none: a mapping the ConfigMap did not
// define is never attribute-driven, never grants a Provider role, and is never
// a service account. The handler refuses a request that breaks them; load paths
// refuse a row that does, since it was written before the rules existed or
// edited underneath the service; the ConfigMap sync drops such mappings from
// config rows.
func APIRuleViolation(row cdbm.Issuer) string {
	for _, cm := range row.ClaimMappings {
		if cm.Source == cdbm.IssuerSourceConfig {
			continue
		}
		if cm.OrgAttribute != "" || cm.OrgDisplayAttribute != "" || cm.RolesAttribute != "" {
			return "api claim mapping for org " + cm.OrgName + " is attribute-driven (orgAttribute, orgDisplayAttribute or rolesAttribute)"
		}
		if cm.IsServiceAccount {
			return "api claim mapping for org " + cm.OrgName + " is a service account"
		}
		for _, r := range cm.Roles {
			if !cauth.TenantRoles[r] {
				return "api claim mapping for org " + cm.OrgName + " grants non-Tenant role " + r
			}
		}
	}
	return ""
}

// ClaimMappingAudit flattens the orgs and roles a mapping set grants, so every
// trust mutation records what changed, not only which issuer. A service-account
// mapping grants the fixed admin role set rather than listing roles, so those
// are named rather than reported as none.
func ClaimMappingAudit(mappings []cdbm.ClaimMapping) (orgs, roles []string) {
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

// DescribeIssuerChange lists the configuration differences between two states of
// one issuer row, for the status history. Fields are named as the API exposes
// them. Claim mappings compare by ClaimMappingKey, so a mapping whose fields
// changed is reported as removed and added. The result is bounded by
// maxIssuerStatusMessageBytes.
func DescribeIssuerChange(actor string, before, after cdbm.Issuer) string {
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

// jwksErrorSummary reduces a key fetch error to a fixed classification for the
// issuer row and its history, which Provider Admins read. The raw error embeds the
// endpoint URL, resolved addresses, redirect targets and TLS details, so storing it
// would let a registered jwksUrl probe the network the API runs in. The raw error
// is still logged server-side by the caller.
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

// jwksUnreachableSummary covers both a connection failure and an egress-policy
// refusal. Telling them apart in issuer status would let an API caller learn
// whether a hostname resolves to an internal address; the server log keeps the
// precise reason.
const jwksUnreachableSummary = "JWKS endpoint unreachable or not permitted by egress policy"

var jwksStatusCodePattern = regexp.MustCompile(`received status code (\d{3})`)

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

// SyncConfigMapIssuers makes the issuer table agree with the ConfigMap. It runs
// once per replica at startup, under the issuer lock, and every step is
// idempotent so all replicas may run it. A ConfigMap issuer with no row is
// inserted (source config). A config row is updated to the ConfigMap's current
// definition, keeping the api-sourced claim mappings that still fit. An api row
// whose URL the ConfigMap now defines is taken over in place: its source flips to
// config, a Superseded entry records what changed, and its ID, history and cached
// keys survive. A config row whose URL left the ConfigMap is soft-deleted.
//
// Replicas on different ConfigMap revisions coexist during a rollout, and a pod
// on the old image can restart after the new one has synced. Every live config row
// is stamped with the revision of the last sync, deleted rows keep theirs, and a
// replica whose revision is older than the highest it finds leaves the table to
// the newer ConfigMap. A fresh install starts the count over and syncs regardless,
// restamping every row so the count is consistent afterwards; a pod of that
// install restarting during the first upgrade's rollout is the one case this does
// not order. Without a revision (zero) the check is off.
//
// A cross-issuer overlap between the ConfigMap and an api row, an org or a JWKS
// URL both map, must never exist on a live system. The API refuses to create one,
// and this sync refuses to start a replica that would serve one: it returns
// ErrIssuerIdentityConflict naming both issuers, the pass rolls back, and the
// caller fails startup so the pod crash-loops with that message in its log until
// the ConfigMap or the row is fixed.
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

			// Keep an api-sourced mapping when it still fits beside the ConfigMap's:
			// its org is not owned by another ConfigMap issuer, and if this issuer's
			// ConfigMap mappings name the same org, its audiences are non-empty and
			// disjoint from theirs. Anything dropped is named in the history entry.
			for _, cm := range existing.ClaimMappings {
				if cm.Source != cdbm.IssuerSourceAPI || APIRuleViolation(cdbm.Issuer{ClaimMappings: []cdbm.ClaimMapping{cm}}) != "" {
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
				msg := DescribeIssuerChange("superseded by ConfigMap issuer", existing, candidate)
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
			if herr := RecordIssuerHistory(ctx, dbSession, tx, existing.ID, updated.Status, DescribeIssuerChange("ConfigMap sync", existing, candidate)); herr != nil {
				return herr
			}
		}

		// Stamped before the removals so a withdrawn row carries this revision too.
		if revision > 0 {
			if serr := dao.StampConfigRevision(ctx, tx, revision, c.GetConfigRevisionReset()); serr != nil {
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
			orgs, grantedRoles := ClaimMappingAudit(row.ClaimMappings)
			log.Warn().Str("event", "issuer_removed").Str("issuer", row.IssuerURL).Str("id", row.ID.String()).
				Strs("withdrawn_orgs", orgs).Strs("withdrawn_roles", grantedRoles).
				Msg("the ConfigMap no longer defines this issuer; withdrew its row")
		}

		// The table as it now stands is what every replica will load. It must be a
		// valid issuer set, or startup stops here with the reason.
		return c.ValidateCombinedIssuers(ctx, dbSession, tx, nil, nil)
	})
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
// the resolver, the startup sync and reload, and both background loops — so a
// replica can never serve issuer writes it does not load, or maintain a registry
// nothing can write to. Off when Keycloak is on, when a privileged ConfigMap
// origin owns claim extraction, or when the deployment is not disconnected
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

// ValidateCombinedIssuers validates a candidate issuer against every other live
// row, reusing the rules in ValidateIssuersConfig. ConfigMap issuers are rows
// too (see SyncConfigMapIssuers), so the table is the whole set. excludeID, if
// set, drops that existing row so it cannot conflict with itself; candidate may
// be nil to validate the set as it stands.
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
		// A token can name any issuer, so an issuer with no row must cost one
		// lock-free read and nothing else: taking the issuer lock for every unknown
		// name would let unauthenticated traffic serialize behind, and stall, the
		// write path. The locked read below is what the published config is built
		// from; this one only decides whether there is anything to publish.
		if _, err := cdbm.NewIssuerDAO(dbSession).GetByIssuerURL(ctx, nil, issuerURL); err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return nil, nil
			}
			return nil, err
		}
		return c.resolveDBIssuer(ctx, dbSession, reg, issuerURL)
	})
}

// resolveDBIssuer reads the row for issuerURL under the issuer lock, applies the
// acceptance rules reload applies, and installs a config built from that very
// row. Reading once, under the lock, is what makes the published config and the
// recorded signature describe the same row: a PUT that commits between two reads
// would otherwise leave this replica serving the older definition for as long as
// the signature stays unchanged. Publishing under the lock also orders this
// against a concurrent delete, so withdrawn trust is never re-added.
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

		// The same rules as reload and the write path, applied to a row that already
		// exists, so a manually inserted or legacy conflicting row cannot enter
		// through the token path.
		if reason := APIRuleViolation(*live); reason != "" {
			log.Warn().Str("issuer", live.IssuerURL).Str("reason", reason).Msg("issuer row breaks the API's claim mapping rules; refusing")
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

// installRow publishes one committed row to the registry and the managed set,
// under state.mu. Bookkeeping and the registry entry move together: ReloadDBIssuers
// reads both under the same lock, so it must never observe the URL as managed
// while the registry has no config for it. The orgs the row owns are reserved
// before it is live, so a ConfigMap dynamic mapping can never claim them in
// between; the set only grows here, the next reload recomputes it.
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
	reg.AddJwksConfig(carryKeys(reg.GetConfig(row.IssuerURL), built))
	return built
}

// PublishIssuerMutation applies a committed issuer write to this replica's
// registry before the handler responds, so the response never precedes local
// trust. A reload is the normal path and is not cut short by the request ending.
// When it fails, a write is installed the way the resolver installs a row: read
// under the issuer lock, so a delete or replacement that committed since is not
// undone. A deletion, or a write whose locked read fails, withdraws the URL: the
// entry still live is the definition the caller just replaced, and serving it
// until the next reload would be the stale trust the contract rules out.
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
	// A row recreated under the same URL since this one committed is withdrawn
	// too, and comes back at the next reload: cached bookkeeping is not evidence
	// enough to keep an entry live after a delete.
	state.mu.Lock()
	defer state.mu.Unlock()
	reg.RemoveConfig(row.IssuerURL)
	delete(state.dbURLs, row.IssuerURL)
	delete(state.dbSigs, row.IssuerURL)
	delete(state.dbIDs, row.IssuerURL)
}

// StartJWKSRefreshLoop keeps DB-sourced issuers' key sets current and writes each
// success back, so a replica that restarts later starts from a recent snapshot.
// Every tick it refreshes an issuer that has no key set yet, and one whose set is
// older than maxAge, and then records on every issuer's row how its last refresh
// attempt ended, so the status reflects a JWKS endpoint that stopped answering.
// Registration deliberately does not contact the identity provider, so a new
// issuer is live but unusable until some fetch succeeds; the short tick is what
// keeps one created during a brief provider outage from looking broken for
// minutes, while a healthy issuer is re-fetched once per maxAge. The per-endpoint
// throttle in auth/pkg/config bounds the worst case.
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

// refreshJWKS refreshes DB-managed issuers concurrently, so one slow IdP delays
// only its own issuer. Each success is persisted best-effort. An issuer that
// already holds a key set is refreshed only when that set is older than maxAge;
// zero refreshes every issuer. Whether or not an issuer was refreshed this pass,
// its row is brought up to date with the last attempt any caller made, including
// a token-path refresh that failed: Ready when keys are loaded and the last
// attempt succeeded, Degraded when keys are loaded but the last attempt failed,
// Pending when no keys are loaded. A status transition records one history entry.
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
		var msg *string
		if ferr != nil {
			s := jwksErrorSummary(ferr)
			msg = &s
		}
		fetchErr := msg
		// A set past the trust ceiling is refused by every token path, so it counts
		// as no keys: Ready or Degraded would claim the cached set still serves.
		status, detail := cdbm.IssuerStatusReady, "JWKS fetched"
		switch {
		case jwksCfg.KeyCount() > 0 && jwksCfg.GetJWKS() == nil:
			s := "cached key set expired without a successful refresh"
			if msg != nil {
				s += ": " + *msg
			}
			status, detail, msg = cdbm.IssuerStatusPending, s, &s
		case jwksCfg.KeyCount() == 0 && msg != nil:
			status, detail = cdbm.IssuerStatusPending, "no JWKS key set fetched yet: "+*msg
		case jwksCfg.KeyCount() == 0:
			status, detail = cdbm.IssuerStatusPending, "no JWKS key set fetched yet"
		case msg != nil:
			status, detail = cdbm.IssuerStatusDegraded, "JWKS refresh failed, serving the cached key set: "+*msg
		}
		outcome := jwksReport{at: at, status: status}
		if msg != nil {
			outcome.message = *msg
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
			transitioned, rerr := dao.RecordJWKSAttempt(ctx, tx, id, jwksCfg.JWKSURL(), at, fetchErr, status, msg)
			if rerr != nil || !transitioned {
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
		if jwksCfg.KeyCount() > 0 && maxAge > 0 && time.Since(jwksCfg.LastFetchedAt()) < maxAge {
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

			raw, fetchedAt, fetched, err := jwksCfg.RefreshJWKS(ctx)
			if err != nil {
				ev := log.Warn()
				if errors.Is(err, context.Canceled) {
					ev = log.Debug()
				}
				ev.Err(err).Str("issuer", url).Msg("JWKS refresh failed; keeping the currently loaded keys")
			} else if fetched {
				persistJWKS(ctx, dbSession, id, url, jwksCfg.JWKSURL(), raw, fetchedAt)
			}
			if ctx.Err() == nil {
				report(url, id, jwksCfg)
			}
		}(url, id, jwksCfg)
	}
	wg.Wait()
}

// ReloadDBIssuers idempotently rebuilds the live registry from the issuer table
// alone, performing no network I/O. ConfigMap issuers are rows too, so the table
// is the whole trusted set. On the first call every row is built; afterwards only
// changed or new issuers are rebuilt and deleted ones removed, preserving cached
// keys. A row that does not fit the set is left in the table but removed from
// the registry.
func (c *Config) ReloadDBIssuers(ctx context.Context, dbSession *cdb.Session) error {
	if !c.DynamicIssuersEnabled() {
		return nil
	}
	reg := c.TokenOriginConfig
	if reg == nil {
		return nil
	}

	// One pass at a time, read included, so no pass reconciles against a snapshot
	// older than the one a concurrent pass already applied. The DB read itself
	// happens outside state.mu: the resolver takes state.mu while it holds the
	// issuer advisory lock inside an open transaction, so holding state.mu across
	// a read that can queue behind a table lock would order the two locks both
	// ways, and Postgres cannot see the in-process edge.
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

	// Accept rows one at a time, ConfigMap rows first and then table order, so one
	// row that breaks the set (which every writer validates against, so only a
	// direct table edit can produce one) costs only itself and never a ConfigMap
	// issuer. The pass still runs to the removals below: refusing the whole pass
	// would keep trusting an issuer deleted since the last pass for as long as the
	// bad row stayed.
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
		if reason := APIRuleViolation(di); reason != "" {
			log.Warn().Str("issuer", di.IssuerURL).Str("reason", reason).Msg("issuer row breaks the API's claim mapping rules; skipping")
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

		// No IdP contact here: hydrate from the row's cached key set, or carry over
		// the keys the entry being replaced already holds for the same endpoint (the
		// ConfigMap entry installed from memory at startup included), or register
		// keyless and let the first token naming it pay one fetch.
		built := c.buildJwksConfig(di)
		built.SetReservedOrgNames(reserved)
		reg.AddJwksConfig(carryKeys(live, built))
	}

	for url := range prevManaged {
		if newManaged[url] {
			continue
		}
		// Published by the resolver after this reload's read: the row exists but was
		// not in the snapshot. Keep it as it is, and keep the orgs it reserved, and
		// let the next reload judge it.
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

	// configs is every row that made it in plus any the resolver published after the
	// read. A live entry outside the managed set still owns its static orgs, so the
	// set never shrinks below them.
	reserved = computeReservedOrgNames(configs)
	for url, live := range reg.GetAllConfigs() {
		if !newManaged[url] {
			maps.Copy(reserved, computeReservedOrgNames([]IssuerConfig{{ClaimMappings: live.GetClaimMappings()}}))
		}
	}
	setReservedOrgNames(reg, reserved)
	return nil
}

// persistJWKS writes a freshly fetched key set back to the issuer row. It is
// best-effort: the keys are already installed in memory, so a failure only costs
// the next replica a fetch. fetchedAt is the time the fetch was issued, so the
// row's stale-write guard compares acquisition rather than completion order.
// jwksURL is the endpoint the keys came from, so a row that has since moved to
// another endpoint is not written.
func persistJWKS(ctx context.Context, dbSession *cdb.Session, id uuid.UUID, issuerURL, jwksURL string, raw []byte, fetchedAt time.Time) {
	if len(raw) == 0 {
		return
	}
	if err := cdbm.NewIssuerDAO(dbSession).UpdateJWKSCache(ctx, nil, id, jwksURL, raw, fetchedAt); err != nil {
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
// lowercasing static org names to match how ConfigMap issuers are normalized. The
// auth registry has no use for a mapping's source, so it is dropped here.
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

// setReservedOrgNames publishes reserved to every live entry. Static ownership is
// what keeps a dynamic mapping from minting an org another issuer owns, and the
// entries that can carry one come from the table, which may hold issuers this
// replica's own ConfigMap does not name during a rollout. SetReservedOrgNames
// copies the map, so a concurrent reader sees one complete set or the other.
func setReservedOrgNames(reg *cauth.TokenOriginConfig, reserved map[string]bool) {
	for _, live := range reg.GetAllConfigs() {
		live.SetReservedOrgNames(reserved)
	}
}

// buildJwksConfig builds a live JwksConfig for a DB issuer, hydrated from the row's
// cached key set when it has one. An API-defined issuer's endpoints are caller
// input, so its fetches may not reach the pod's network unless the deployment
// allows internal hosts.
func (c *Config) buildJwksConfig(di cdbm.Issuer) *cauth.JwksConfig {
	origin := di.Origin
	if origin == "" {
		origin = cauth.TokenOriginCustom
	}
	cfg := cauth.NewJwksConfig(di.JWKSUrl, di.IssuerURL, origin, di.ServiceAccount, di.Audiences, di.Scopes)
	cfg.RestrictEgress = di.Source == cdbm.IssuerSourceAPI && !c.GetAuthAllowInternalIssuerHosts()
	if d, err := time.ParseDuration(di.JWKSTimeout); err == nil {
		cfg.JWKSTimeout = d
	}
	cfg.ClaimMappings = convertClaimMappings(di.ClaimMappings)
	hydrateFromCache(cfg, di)
	return cfg
}

// carryKeys installs on cfg the key set that old, the entry cfg replaces, holds
// for the same JWKS endpoint, and the fetch outcome that produced it, so a
// rebuild neither costs a fetch nor leaves the row unreported until the next one.
// install's monotonic guard keeps a newer set hydrated from the row. It returns cfg.
func carryKeys(old, cfg *cauth.JwksConfig) *cauth.JwksConfig {
	if old == nil || old == cfg || old.URL != cfg.URL {
		return cfg
	}
	if jwks := old.GetJWKS(); jwks != nil {
		cfg.SetJWKSFromCache(jwks, old.LastFetchedAt())
	}
	cfg.CarryAttempt(old)
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
