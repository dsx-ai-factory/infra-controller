// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/roles"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	userActivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/user"
	freelru "github.com/elastic/go-freelru"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"golang.org/x/sync/singleflight"
)

const (
	nvapiPrefix     = "nvapi-"
	nvapiPayloadLen = 64

	callerInfoPath     = "/v3/keys/get-caller-info"
	fetchTimeout       = 10 * time.Second
	maxNGCResponseSize = 65536
	resolveTimeout     = 25 * time.Second

	keyTypeService  = "SERVICE_KEY"
	keyTypePersonal = "PERSONAL_KEY"

	ngcOperationCallerInfo = "caller_info"
	ngcResultSuccess       = "success"
	ngcResultRejected      = "rejected"
	ngcResultRateLimited   = "rate_limited"
	ngcResultUpstreamError = "upstream_error"

	apiKeyStalePeriod = 3 * time.Minute
	blockPeriod       = 3 * time.Minute

	allowCapacity uint32 = 8192
	blockCapacity uint32 = 4096
)

var (
	errInvalidKeyFormat = errors.New("credential does not match any accepted NGC API key format")
	errNgcUnauthorized  = errors.New("NGC rejected the API key")
	errNgcUpstream      = errors.New("NGC could not be reached or returned an unusable response")
	errNgcRateLimited   = errors.New("NGC rate limited API key verification")
	errKeyRejected      = errors.New("API key is not valid")
	errOrgNotGranted    = errors.New("API key holds no roles in the requested organization")
	errUnresolvable     = errors.New("API key could not be resolved")
)

// productRoles maps an NGC service-key product to the NICo role it grants
var productRoles = map[string]string{
	"forge-provider": roles.ProviderAdminRole,
	"forge-tenant":   roles.TenantAdminRole,
}

func validateCredential(raw string) error {
	payload, ok := strings.CutPrefix(raw, nvapiPrefix)
	if !ok || len(payload) != nvapiPayloadLen {
		return errInvalidKeyFormat
	}

	return nil
}

// kasRecorder records bounded operational outcomes for KAS API-key resolution.
type kasRecorder interface {
	RecordNGCRequest(operation, result string)
}

type kasMetrics struct {
	ngcRequests *prometheus.CounterVec
}

// newKasMetrics registers metrics for outbound NGC API-key verification.
func newKasMetrics(reg prometheus.Registerer, namespace string) kasRecorder {
	metrics := &kasMetrics{
		ngcRequests: prometheus.NewCounterVec(prometheus.CounterOpts{
			Namespace: namespace,
			Name:      "ngc_api_key_verification_requests_total",
			Help:      "Number of outbound NGC API-key verification operations by operation and result; cache hits are excluded.",
		}, []string{"operation", "result"}),
	}
	reg.MustRegister(metrics.ngcRequests)
	return metrics
}

func (m *kasMetrics) RecordNGCRequest(operation, result string) {
	m.ngcRequests.WithLabelValues(operation, result).Inc()
}

type ngcClient struct {
	http    *http.Client
	baseURL string
	metrics kasRecorder
}

type callerInfo struct {
	KeyType  string                `json:"type"`
	UserID   string                `json:"userId"`
	OrgName  string                `json:"orgName"`
	Products []string              `json:"products"`
	User     *userActivity.NgcUser `json:"user"`
}

func (cl *ngcClient) getCallerInfo(ctx context.Context, key string) (*callerInfo, error) {
	form := url.Values{"credentials": {key}}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		cl.baseURL+callerInfoPath, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, fmt.Errorf("%w: %w", err, errNgcUpstream)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept-Encoding", "identity")

	body, err := cl.do(req)
	if err != nil {
		cl.recordNGCRequest(ngcOperationCallerInfo, ngcResultForError(err))
		return nil, err
	}

	info := &callerInfo{}
	if err := json.Unmarshal(body, info); err != nil {
		cl.recordNGCRequest(ngcOperationCallerInfo, ngcResultUpstreamError)
		return nil, fmt.Errorf("could not decode the get-caller-info response: %w", errNgcUpstream)
	}

	cl.recordNGCRequest(ngcOperationCallerInfo, ngcResultSuccess)
	return info, nil
}

func (cl *ngcClient) recordNGCRequest(operation, result string) {
	if cl.metrics != nil {
		cl.metrics.RecordNGCRequest(operation, result)
	}
}

func ngcResultForError(err error) string {
	if errors.Is(err, errNgcUnauthorized) {
		return ngcResultRejected
	}
	if errors.Is(err, errNgcRateLimited) {
		return ngcResultRateLimited
	}
	return ngcResultUpstreamError
}

// do never includes the credential in the returned error, since these errors are logged
func (cl *ngcClient) do(req *http.Request) ([]byte, error) {
	resp, err := cl.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", err, errNgcUpstream)
	}
	defer resp.Body.Close()

	switch resp.StatusCode {
	case http.StatusOK:
	case http.StatusUnauthorized, http.StatusForbidden:
		return nil, fmt.Errorf("status %d: %w", resp.StatusCode, errNgcUnauthorized)
	case http.StatusTooManyRequests:
		return nil, fmt.Errorf("status %d, retry-after %q: %w",
			resp.StatusCode, resp.Header.Get("Retry-After"), errNgcRateLimited)
	default:
		return nil, fmt.Errorf("status %d: %w", resp.StatusCode, errNgcUpstream)
	}

	body, err := io.ReadAll(io.LimitReader(resp.Body, maxNGCResponseSize+1))
	if err != nil {
		return nil, fmt.Errorf("%w: %w", err, errNgcUpstream)
	}
	if len(body) > maxNGCResponseSize {
		return nil, fmt.Errorf("response exceeded %d bytes: %w", maxNGCResponseSize, errNgcUpstream)
	}

	return body, nil
}

type cachedUser struct {
	userID uuid.UUID
	// orgName is the route org this verification was for. A request naming any other
	// org goes back to NGC rather than reusing a grant the key may not hold there
	orgName    string
	verifiedAt time.Time
}

type apiKeyCache struct {
	allowLRU    *freelru.SyncedLRU[[32]byte, cachedUser]
	blockLRU    *freelru.SyncedLRU[[32]byte, struct{}]
	blockPeriod time.Duration
}

func newAPIKeyCache() (*apiKeyCache, error) {
	allowLRU, err := freelru.NewSynced[[32]byte, cachedUser](allowCapacity, hashDigest)
	if err != nil {
		return nil, err
	}

	blockLRU, err := freelru.NewSynced[[32]byte, struct{}](blockCapacity, hashDigest)
	if err != nil {
		return nil, err
	}

	return &apiKeyCache{
		allowLRU:    allowLRU,
		blockLRU:    blockLRU,
		blockPeriod: blockPeriod,
	}, nil
}

func hashDigest(dg [32]byte) uint32 {
	return binary.LittleEndian.Uint32(dg[:4])
}

func (ca *apiKeyCache) isAllowed(dg [32]byte) (cachedUser, bool) {
	return ca.allowLRU.Get(dg)
}

func (ca *apiKeyCache) setAllowed(dg [32]byte, userID uuid.UUID, orgName string) {
	ca.allowLRU.Add(dg, cachedUser{
		userID:     userID,
		orgName:    orgName,
		verifiedAt: time.Now().UTC(),
	})
}

// unsetAllowedForOrg drops an allow entry recorded for org, and leaves an entry
// recorded for another org in place: a verification speaks only for the org it
// named, so it cannot invalidate a grant it did not check. Peek rather than Get,
// since an entry that stays is not being used here.
func (ca *apiKeyCache) unsetAllowedForOrg(dg [32]byte, org string) {
	entry, found := ca.allowLRU.Peek(dg)
	if found && strings.EqualFold(entry.orgName, org) {
		ca.allowLRU.Remove(dg)
	}
}

func (ca *apiKeyCache) isBlocked(dg [32]byte) bool {
	_, found := ca.blockLRU.Get(dg)
	return found
}

func (ca *apiKeyCache) setBlocked(dg [32]byte) {
	ca.blockLRU.AddWithLifetime(dg, struct{}{}, ca.blockPeriod)
	ca.allowLRU.Remove(dg)
}

type identity struct {
	starfleetID *string
	auxiliaryID *string
	email       string
	firstName   string
	lastName    string
	// orgData holds the single org this resolution verified: the one the route named,
	// matched against what get-caller-info reported for the credential
	orgData cdbm.OrgData
}

// normalizeOrgData restates org names in lower case, both as map keys and on the org
// itself. NGC treats an org name as a case-insensitive slug and the route carries
// it verbatim, so normalizing on ingest keeps one spelling in the cache, in the
// request scope, and in the user record.
func normalizeOrgData(orgData cdbm.OrgData) cdbm.OrgData {
	lowered := make(cdbm.OrgData, len(orgData))
	for name, org := range orgData {
		org.Name = strings.ToLower(org.Name)
		lowered[strings.ToLower(name)] = org
	}

	return lowered
}

type resolver struct {
	dbSession  *cdb.Session
	ngc        *ngcClient
	cache      *apiKeyCache
	fetchGroup singleflight.Group
}

func newResolver(dbSession *cdb.Session, baseURL string, metrics kasRecorder) *resolver {
	cache, err := newAPIKeyCache()
	if err != nil {
		// The cache capacities and the hash function are compile-time constants, so
		// allocation cannot fail on any reachable input
		log.Panic().Err(err).Msg("failed to allocate the kas origin API key caches")
	}

	return &resolver{
		dbSession: dbSession,
		ngc: &ngcClient{
			http:    &http.Client{Timeout: fetchTimeout},
			baseURL: strings.TrimRight(baseURL, "/"),
			metrics: metrics,
		},
		cache: cache,
	}
}

func (r *resolver) getDigest(raw string) [32]byte {
	return sha256.Sum256([]byte(raw))
}

// resolve verifies the credential for one org. urlOrg is expected in lower case.
func (r *resolver) resolve(ctx context.Context, raw, urlOrg string) (*cdbm.User, error) {
	if err := validateCredential(raw); err != nil {
		return nil, errKeyRejected
	}

	dg := r.getDigest(raw)

	if r.cache.isBlocked(dg) {
		return nil, errKeyRejected
	}

	entry, found := r.cache.isAllowed(dg)
	if found && strings.EqualFold(entry.orgName, urlOrg) && time.Since(entry.verifiedAt) <= apiKeyStalePeriod {
		userDAO := cdbm.NewUserDAO(r.dbSession)
		dbUser, err := userDAO.Get(ctx, nil, entry.userID, nil)
		if err == nil {
			return dbUser, nil
		}
	}

	return r.refresh(ctx, dg, raw, urlOrg)
}

func (r *resolver) refresh(ctx context.Context, dg [32]byte, raw, urlOrg string) (*cdbm.User, error) {
	// The leader runs the fetch on its own request goroutine, so it must not be
	// cancelled by that client disconnecting while other callers wait on the result
	fetchCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), resolveTimeout)
	defer cancel()

	// The digest is a fixed 32 bytes, so appending the org cannot collide with
	// another key's digest. Two orgs resolve to different grants, so they must not
	// share a flight
	fetchKey := string(dg[:]) + urlOrg

	resolved, err, _ := r.fetchGroup.Do(fetchKey, func() (interface{}, error) {
		id, err := r.fetchIdentity(fetchCtx, raw, urlOrg)
		if err != nil {
			// A failed verification for this org leaves no entry claiming it. A
			// reload failure on the cache-hit path can reach here with an entry
			// for this org that is still inside the stale period
			r.cache.unsetAllowedForOrg(dg, urlOrg)

			switch {
			case errors.Is(err, errOrgNotGranted):
				// The credential was accepted, so it is not blocked and the user
				// record is left alone
				return nil, err
			case errors.Is(err, errNgcUnauthorized):
				// The credential itself is dead, so setBlocked drops the entry
				// whichever org it was recorded for
				r.cache.setBlocked(dg)
				return nil, fmt.Errorf("%w: %w", err, errKeyRejected)
			}
			return nil, fmt.Errorf("%w: %w", err, errUnresolvable)
		}

		user, err := r.createOrUpdateUser(fetchCtx, id)
		if err != nil {
			return nil, fmt.Errorf("user upsert failed: %w: %w", err, errUnresolvable)
		}

		// Must stay inside the flight so waiters observe the mapping
		r.cache.setAllowed(dg, user.ID, urlOrg)
		return user, nil
	})
	if err != nil {
		return nil, err
	}

	user, ok := resolved.(*cdbm.User)
	if !ok {
		return nil, errUnresolvable
	}

	return user, nil
}

func (r *resolver) fetchIdentity(ctx context.Context, raw, urlOrg string) (*identity, error) {
	caller, err := r.ngc.getCallerInfo(ctx, raw)
	if err != nil {
		return nil, err
	}

	switch caller.KeyType {
	case keyTypePersonal:
		return identityFromNgcUser(caller.User, urlOrg)
	case keyTypeService:
		return identityFromServiceKey(caller, urlOrg)
	default:
		return nil, fmt.Errorf("unrecognized API key type %q: %w", caller.KeyType, errNgcUpstream)
	}
}

// identityFromNgcUser narrows the orgs the key owner holds roles in to the one the
// route named, and reports errOrgNotGranted when it is not among them. A personal key
// is as privileged as its owner everywhere, so the route is what scopes it; the
// caller's own orgName is deliberately ignored, since it names the owner's current NGC
// org rather than the org the request is for.
func identityFromNgcUser(ngcUser *userActivity.NgcUser, urlOrg string) (*identity, error) {
	if ngcUser == nil || ngcUser.StarfleetID == "" {
		return nil, fmt.Errorf("NGC returned a user with no starfleetId: %w", errNgcUpstream)
	}

	routeOrg, err := normalizeOrgData(userActivity.GetOrgData(ngcUser)).GetOrgByName(urlOrg)
	if err != nil {
		return nil, fmt.Errorf("get-caller-info reported no roles in %q: %w", urlOrg, errOrgNotGranted)
	}

	firstName, lastName, _ := strings.Cut(ngcUser.Name, " ")

	return &identity{
		starfleetID: cutil.GetPtr(ngcUser.StarfleetID),
		email:       ngcUser.Email,
		firstName:   firstName,
		lastName:    lastName,
		orgData:     cdbm.OrgData{routeOrg.Name: *routeOrg},
	}, nil
}

// identityFromServiceKey reports the single org the key belongs to, and
// errOrgNotGranted when the route named a different one. A service key is issued
// against one NGC org and carries no roles anywhere else.
func identityFromServiceKey(caller *callerInfo, urlOrg string) (*identity, error) {
	if caller.UserID == "" {
		return nil, fmt.Errorf("get-caller-info returned no userId: %w", errNgcUpstream)
	}
	if caller.OrgName == "" {
		return nil, fmt.Errorf("get-caller-info returned no orgName: %w", errNgcUpstream)
	}
	if !strings.EqualFold(caller.OrgName, urlOrg) {
		return nil, fmt.Errorf("get-caller-info reported the key is issued against another org: %w", errOrgNotGranted)
	}

	orgRoles := []string{}
	for _, product := range caller.Products {
		role, mapped := productRoles[product]
		if mapped && !slices.Contains(orgRoles, role) {
			orgRoles = append(orgRoles, role)
		}
	}

	keyOrg := cdbm.Org{
		Name:  caller.OrgName,
		Roles: orgRoles,
		Teams: []cdbm.Team{},
	}

	return &identity{
		auxiliaryID: cutil.GetPtr(caller.UserID),
		orgData:     normalizeOrgData(cdbm.OrgData{caller.OrgName: keyOrg}),
	}, nil
}

// applyVerifiedOrgs updates verified org roles and timestamps while retaining
// stored metadata and organizations outside the route scope.
func applyVerifiedOrgs(stored, verified cdbm.OrgData, updatedAt time.Time) cdbm.OrgData {
	orgData := make(cdbm.OrgData, len(stored)+len(verified))
	for name, org := range stored {
		orgData[name] = org
	}

	for name, org := range verified {
		existing, found := orgData[name]
		if found {
			existing.Roles = org.Roles
			org = existing
		}

		org.Updated = &updatedAt
		orgData[name] = org
	}

	return orgData
}

func (r *resolver) createOrUpdateUser(ctx context.Context, id *identity) (*cdbm.User, error) {
	userDAO := cdbm.NewUserDAO(r.dbSession)

	dbUser, _, err := userDAO.GetOrCreate(ctx, nil, cdbm.UserGetOrCreateInput{
		StarfleetID: id.starfleetID,
		AuxiliaryID: id.auxiliaryID,
	})
	if err != nil {
		return nil, err
	}

	input := cdbm.UserUpdateInput{
		UserID:          dbUser.ID,
		OrgData:         applyVerifiedOrgs(dbUser.OrgData, id.orgData, time.Now().UTC()),
		PreserveUpdated: true,
	}
	if id.email != "" {
		input.Email = cutil.GetPtr(id.email)
	}
	if id.firstName != "" {
		input.FirstName = cutil.GetPtr(id.firstName)
	}
	if id.lastName != "" {
		input.LastName = cutil.GetPtr(id.lastName)
	}

	return userDAO.Update(ctx, nil, input)
}

// Ensure KasOriginProcessor implements config.TokenProcessor interface
var _ config.TokenProcessor = (*KasOriginProcessor)(nil)

// KasOriginProcessor processes NGC API keys presented as bearer credentials
type KasOriginProcessor struct {
	resolver *resolver
}

// NewKasOriginProcessor creates a new NGC API key processor
func NewKasOriginProcessor(dbSession *cdb.Session, baseURL string, metrics kasRecorder) config.TokenProcessor {
	return &KasOriginProcessor{resolver: newResolver(dbSession, baseURL, metrics)}
}

// ProcessToken resolves an NGC API key to a user record, refreshing it from NGC when stale
func (h *KasOriginProcessor) ProcessToken(c echo.Context, tokenStr string, _ *config.JwksConfig, logger zerolog.Logger) (*cdbm.User, *cutil.APIError) {
	// The route names the org to act on, and it is what the credential is verified
	// against, so a verification for one org is never reused for another
	requestedOrg := strings.ToLower(c.Param("orgName"))

	resolved, err := h.resolver.resolve(c.Request().Context(), tokenStr, requestedOrg)
	if err != nil {
		if errors.Is(err, errOrgNotGranted) {
			logger.Warn().Err(err).
				Str("requested_org", requestedOrg).
				Msg("API key does not grant access to the requested organization")
			return nil, cutil.NewAPIError(http.StatusForbidden, "API key is not authorized for requested organization", nil)
		}

		if errors.Is(err, errKeyRejected) {
			logger.Warn().Err(err).Msg("rejected API key in authorization header")
			return nil, cutil.NewAPIError(http.StatusUnauthorized, "Invalid authorization token in request", nil)
		}

		if errors.Is(err, errNgcRateLimited) {
			logger.Warn().Err(err).Msg("NGC rate limited API key verification; the deployment's egress IP address likely needs an NGC allowlist entry")
			return nil, cutil.NewAPIError(http.StatusServiceUnavailable,
				"NGC rate limited API key verification, try again later", nil)
		}

		logger.Error().Err(err).Msg("failed to resolve API key against NGC")
		return nil, cutil.NewAPIError(http.StatusServiceUnavailable, "Failed to verify authorization token, try again later", nil)
	}

	// Resolution verified the route org against NGC and persisted it, so the record
	// carries it unless another source has since dropped it from the record a cache
	// hit reads back
	routeOrg, err := resolved.OrgData.GetOrgByName(requestedOrg)
	if err != nil {
		logger.Warn().
			Str("requested_org", requestedOrg).
			Msg("API key does not grant access to the requested organization")
		return nil, cutil.NewAPIError(http.StatusForbidden, "API key is not authorized for requested organization", nil)
	}

	scopedUser := *resolved
	scopedUser.OrgData = cdbm.OrgData{routeOrg.Name: *routeOrg}

	// NGC service keys and NICo auth service account enablement are different
	// concepts. NGC service keys are simply API keys for an org that are not tied
	// to a specific user
	config.SetIsServiceAccountInContext(c, false)

	c.Set("user", &scopedUser)
	return &scopedUser, nil
}
