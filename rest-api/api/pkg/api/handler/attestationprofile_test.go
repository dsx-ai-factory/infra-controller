// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// attestationFixture is a Provider Admin with a Registered Site and a
// Temporal client whose proxied Core reply each case configures.
type attestationFixture struct {
	org          string
	siteID       string
	foreignSite  string
	user         *cdbm.User
	provider     *cdbm.InfrastructureProvider
	site         *cdbm.Site
	dbSession    *cdb.Session
	scp          *sc.ClientPool
	proxied      []grpcproxy.Request
	replyJSON    []byte
	replyErr     error
	foreignMachs []string
}

func newAttestationFixture(t *testing.T) *attestationFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	f := &attestationFixture{org: "test-org", dbSession: dbSession}
	f.user = common.TestBuildUser(t, dbSession, "test-starfleet-id", f.org, []string{authz.ProviderAdminRole})
	f.provider = common.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", f.org, f.user)
	f.site = common.TestBuildSite(t, dbSession, f.provider, "Test Site", f.user)
	_, err := cdbm.NewSiteDAO(dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: f.site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
	require.NoError(t, err)
	f.site.Status = cdbm.SiteStatusRegistered
	f.siteID = f.site.ID.String()

	otherUser := common.TestBuildUser(t, dbSession, "other-starfleet-id", "other-org", []string{authz.ProviderAdminRole})
	otherProvider := common.TestBuildInfrastructureProvider(t, dbSession, "Other Provider", "other-org", otherUser)
	otherSite := common.TestBuildSite(t, dbSession, otherProvider, "Other Site", otherUser)
	f.foreignSite = otherSite.ID.String()
	f.foreignMachs = []string{common.TestBuildMachine(t, dbSession, otherProvider, otherSite, nil, nil, cdbm.MachineStatusReady).ID}

	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		args.Get(1).(*grpcproxy.Response).ResponseJSON = f.replyJSON
	}).Return(func(context.Context, interface{}) error { return f.replyErr })

	tsc := &tmocks.Client{}
	tsc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.MatchedBy(func(req grpcproxy.Request) bool {
		f.proxied = append(f.proxied, req)
		return true
	})).Return(wrun, nil)

	f.scp = sc.NewClientPool(nil)
	f.scp.IDClientMap[f.siteID] = tsc
	return f
}

// reply sets the next Core reply and clears captured requests.
func (f *attestationFixture) reply(t *testing.T, msg proto.Message, err error) {
	t.Helper()
	f.proxied = nil
	f.replyErr = err
	f.replyJSON = nil
	if msg != nil {
		body, merr := protojson.Marshal(msg)
		require.NoError(t, merr)
		f.replyJSON = body
	}
}

// replyRaw sets the next Core reply to raw JSON.
func (f *attestationFixture) replyRaw(raw string) {
	f.proxied = nil
	f.replyErr = nil
	f.replyJSON = []byte(raw)
}

type attestationRequest struct {
	method      string
	target      string
	contentType string
	body        string
	params      map[string]string
}

func (f *attestationFixture) serve(t *testing.T, handle echo.HandlerFunc, r attestationRequest) *httptest.ResponseRecorder {
	t.Helper()
	var body io.Reader
	if r.body != "" {
		body = strings.NewReader(r.body)
	}
	req := httptest.NewRequest(r.method, r.target, body)
	if r.contentType != "" {
		req.Header.Set(echo.HeaderContentType, r.contentType)
	}
	rec := httptest.NewRecorder()
	ec := echo.New().NewContext(req, rec)
	names := []string{"orgName"}
	values := []string{f.org}
	for k, v := range r.params {
		names = append(names, k)
		values = append(values, v)
	}
	ec.SetParamNames(names...)
	ec.SetParamValues(values...)
	ec.Set("user", f.user)
	err := handle(ec)
	if err != nil {
		ec.Echo().HTTPErrorHandler(err, ec)
	}
	return rec
}

// coreError builds a serialized Core gRPC failure as REST receives it.
func coreError(errType, desc string) error {
	return tp.NewNonRetryableApplicationError("rpc error: code = X desc = "+desc, errType, nil)
}

func testProfileProto(class, version string) *corev1.AttestationProfile {
	return &corev1.AttestationProfile{
		HardwareClass: class,
		Version:       version,
		UpdatedAt:     timestamppb.New(time.Date(2026, 11, 20, 10, 0, 0, 0, time.UTC)),
		UpdatedBy:     "elektra-site-agent",
		Selection:     &corev1.AttesterSelection{Mode: corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALL},
	}
}

// attestationErrorBody is the public shape of an error response.
type attestationErrorBody struct {
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data"`
}

func decodeAPIError(t *testing.T, rec *httptest.ResponseRecorder) attestationErrorBody {
	t.Helper()
	var body attestationErrorBody
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &body))
	return body
}

func TestCreateAttestationProfileHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewCreateAttestationProfileHandler(f.dbSession, f.scp)
	validBody := `{"siteId":"` + f.siteID + `","hardwareClass":"nvidia_gb200","selection":{"mode":"Allowlist","componentIds":[{"prefix":"VERA_"}]}}`

	cases := []struct {
		name        string
		req         attestationRequest
		replyMsg    proto.Message
		replyRaw    string
		replyErr    error
		wantStatus  int
		wantMessage string
		wantStarts  int
	}{
		{name: "creates", req: attestationRequest{body: validBody, contentType: "application/json"}, replyMsg: testProfileProto("nvidia_gb200", "V1-T1"), wantStatus: http.StatusCreated, wantStarts: 1},
		{name: "unsupported media type", req: attestationRequest{body: validBody, contentType: "text/plain"}, wantStatus: http.StatusUnsupportedMediaType},
		{name: "unknown member", req: attestationRequest{body: `{"siteId":"` + f.siteID + `","hardwareClass":"nvidia_gb200","selection":{"mode":"All"},"updatedBy":"me"}`, contentType: "application/json"}, wantStatus: http.StatusBadRequest, wantMessage: "unknown member in the top-level object"},
		{name: "decoded NUL in matcher", req: attestationRequest{body: `{"siteId":"` + f.siteID + `","hardwareClass":"nvidia_gb200","selection":{"mode":"Allowlist","componentIds":[{"exact":"A\u0000"}]}}`, contentType: "application/json"}, wantStatus: http.StatusBadRequest},
		{name: "query string rejected", req: attestationRequest{target: "/?siteId=" + f.siteID, body: validBody, contentType: "application/json"}, wantStatus: http.StatusBadRequest, wantMessage: "unknown query parameter"},
		{name: "foreign site", req: attestationRequest{body: strings.Replace(validBody, f.siteID, f.foreignSite, 1), contentType: "application/json"}, wantStatus: http.StatusForbidden},
		{name: "already exists", req: attestationRequest{body: validBody, contentType: "application/json"}, replyErr: coreError(swe.ErrTypeNICoAlreadyExists, "exists"), wantStatus: http.StatusConflict, wantMessage: "already exists", wantStarts: 1},
		{name: "invalid argument carries Core description", req: attestationRequest{body: validBody, contentType: "application/json"}, replyErr: coreError(swe.ErrTypeNICoInvalidArgument, "hardware class nvidia_gb200 is not recorded"), wantStatus: http.StatusBadRequest, wantMessage: errMsgSiteRejected + ": hardware class nvidia_gb200 is not recorded", wantStarts: 1},
		{name: "unavailable may have committed", req: attestationRequest{body: validBody, contentType: "application/json"}, replyErr: coreError(swe.ErrTypeNICoUnavailable, "conn reset"), wantStatus: http.StatusServiceUnavailable, wantMessage: "may have been applied", wantStarts: 1},
		{name: "empty reply cannot confirm", req: attestationRequest{body: validBody, contentType: "application/json"}, replyRaw: `{}`, wantStatus: http.StatusBadGateway, wantMessage: "may have been applied", wantStarts: 1},
		{name: "reply for another class", req: attestationRequest{body: validBody, contentType: "application/json"}, replyMsg: testProfileProto("nvidia_gb300", "V1-T1"), wantStatus: http.StatusBadGateway, wantStarts: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.replyRaw != "" {
				f.replyRaw(tc.replyRaw)
			} else {
				f.reply(t, tc.replyMsg, tc.replyErr)
			}
			tc.req.method = http.MethodPost
			if tc.req.target == "" {
				tc.req.target = "/"
			}
			rec := f.serve(t, handler.Handle, tc.req)
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			assert.Len(t, f.proxied, tc.wantStarts)
			if tc.wantMessage != "" {
				assert.Contains(t, decodeAPIError(t, rec).Message, tc.wantMessage)
			}
			if tc.wantStatus != http.StatusCreated {
				assert.NotContains(t, rec.Body.String(), "rpc error")
				return
			}
			assert.Equal(t, corev1.Forge_CreateAttestationProfile_FullMethodName, f.proxied[0].FullMethod)
			var coreReq corev1.CreateAttestationProfileRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, "nvidia_gb200", coreReq.GetHardwareClass())
			assert.Equal(t, "VERA_", coreReq.GetSelection().GetComponentIds()[0].GetPrefix())
			var resp model.APIAttestationProfile
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			assert.Equal(t, "V1-T1", resp.Version)
		})
	}
}

func TestGetAllAttestationProfileHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAllAttestationProfileHandler(f.dbSession, f.scp)
	listReply := &corev1.ListAttestationProfilesResponse{Profiles: []*corev1.AttestationProfile{
		testProfileProto("nvidia_gb300", "V1"), testProfileProto("any", "V2"), testProfileProto("dell_r760", "V3"),
	}}

	cases := []struct {
		name        string
		query       string
		replyMsg    proto.Message
		wantStatus  int
		wantClasses []string
		wantTotal   int
		wantStarts  int
	}{
		{name: "sorted first page", query: "siteId=" + f.siteID, replyMsg: listReply, wantStatus: http.StatusOK, wantClasses: []string{"any", "dell_r760", "nvidia_gb300"}, wantTotal: 3, wantStarts: 1},
		{name: "second page", query: "siteId=" + f.siteID + "&pageSize=2&pageNumber=2&orderBy=HARDWARE_CLASS_ASC", replyMsg: listReply, wantStatus: http.StatusOK, wantClasses: []string{"nvidia_gb300"}, wantTotal: 3, wantStarts: 1},
		{name: "page past the end", query: "siteId=" + f.siteID + "&pageNumber=5", replyMsg: listReply, wantStatus: http.StatusOK, wantClasses: []string{}, wantTotal: 3, wantStarts: 1},
		{name: "empty list", query: "siteId=" + f.siteID, replyMsg: &corev1.ListAttestationProfilesResponse{}, wantStatus: http.StatusOK, wantClasses: []string{}, wantTotal: 0, wantStarts: 1},
		{name: "descending order unsupported", query: "siteId=" + f.siteID + "&orderBy=HARDWARE_CLASS_DESC", wantStatus: http.StatusBadRequest},
		{name: "repeated site", query: "siteId=" + f.siteID + "&siteId=" + f.siteID, wantStatus: http.StatusBadRequest},
		{name: "missing site", query: "", wantStatus: http.StatusBadRequest},
		{name: "invalid profile in list", query: "siteId=" + f.siteID, replyMsg: &corev1.ListAttestationProfilesResponse{Profiles: []*corev1.AttestationProfile{{HardwareClass: "a_b"}}}, wantStatus: http.StatusBadGateway, wantStarts: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, nil)
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			assert.Len(t, f.proxied, tc.wantStarts)
			if tc.wantStatus != http.StatusOK {
				return
			}
			var profiles []model.APIAttestationProfile
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &profiles))
			classes := []string{}
			for _, p := range profiles {
				classes = append(classes, p.HardwareClass)
			}
			assert.Equal(t, tc.wantClasses, classes)
			var page pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &page))
			assert.Equal(t, tc.wantTotal, page.Total)
			assert.Equal(t, model.AttestationProfileOrderByHardwareClassAsc, *page.OrderBy)
		})
	}
}

func TestGetAttestationProfileHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAttestationProfileHandler(f.dbSession, f.scp)

	cases := []struct {
		name        string
		class       string
		query       string
		replyMsg    proto.Message
		replyErr    error
		wantStatus  int
		wantMessage string
	}{
		{name: "reads", class: "nvidia_gb200", query: "siteId=" + f.siteID, replyMsg: testProfileProto("nvidia_gb200", "V1-T1"), wantStatus: http.StatusOK},
		{name: "not found", class: "nvidia_gb200", query: "siteId=" + f.siteID, replyErr: coreError(swe.ErrTypeNICoObjectNotFound, "missing"), wantStatus: http.StatusNotFound, wantMessage: "No Attestation Profile"},
		{name: "invalid class", class: "Nvidia", query: "siteId=" + f.siteID, wantStatus: http.StatusBadRequest},
		{name: "unknown query", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&x=1", wantStatus: http.StatusBadRequest},
		{name: "newer Site field", class: "nvidia_gb200", query: "siteId=" + f.siteID, replyMsg: nil, wantStatus: http.StatusBadGateway},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, tc.replyErr)
			if tc.name == "newer Site field" {
				f.replyRaw(`{"hardwareClass":"nvidia_gb200","version":"V1","futureField":true}`)
			}
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query, params: map[string]string{"hardwareClass": tc.class}})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantMessage != "" {
				assert.Contains(t, decodeAPIError(t, rec).Message, tc.wantMessage)
			}
			if tc.wantStatus == http.StatusOK {
				var coreReq corev1.GetAttestationProfileRequest
				require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
				assert.Equal(t, tc.class, coreReq.GetHardwareClass())
			}
		})
	}
}

func TestUpdateAttestationProfileHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewUpdateAttestationProfileHandler(f.dbSession, f.scp)
	body := func(version string) string {
		return `{"siteId":"` + f.siteID + `","selection":{"mode":"None"},"ifVersionMatch":` + version + `}`
	}

	cases := []struct {
		name        string
		class       string
		body        string
		replyMsg    proto.Message
		replyErr    error
		wantStatus  int
		wantMessage string
		wantStarts  int
		wantVersion string
	}{
		{name: "forwards version verbatim", class: "nvidia_gb200", body: body(`" V1-T1 "`), replyMsg: testProfileProto("nvidia_gb200", "V2-T2"), wantStatus: http.StatusOK, wantStarts: 1, wantVersion: " V1-T1 "},
		{name: "literal backslash u0000 reaches Core", class: "nvidia_gb200", body: body(`"\\u0000"`), replyErr: coreError(swe.ErrTypeNICoInvalidArgument, "invalid version"), wantStatus: http.StatusBadRequest, wantStarts: 1, wantVersion: `\u0000`},
		{name: "decoded NUL rejected locally", class: "nvidia_gb200", body: body(`"V1\u0000"`), wantStatus: http.StatusBadRequest, wantMessage: "Error validating"},
		{name: "missing version", class: "nvidia_gb200", body: `{"siteId":"` + f.siteID + `","selection":{"mode":"All"}}`, wantStatus: http.StatusBadRequest},
		{name: "any to None needs acknowledgement", class: "any", body: body(`"V1"`), wantStatus: http.StatusBadRequest},
		{name: "any to None acknowledged", class: "any", body: `{"siteId":"` + f.siteID + `","selection":{"mode":"None"},"ifVersionMatch":"V1","acknowledgeFallbackImpact":true}`, replyMsg: testProfileProto("any", "V2"), wantStatus: http.StatusOK, wantStarts: 1, wantVersion: "V1"},
		{name: "stale version", class: "nvidia_gb200", body: body(`"V1"`), replyErr: coreError(swe.ErrTypeNICoFailedPrecondition, "stale"), wantStatus: http.StatusPreconditionFailed, wantMessage: "read the profile again", wantStarts: 1, wantVersion: "V1"},
		{name: "class in body is not a member", class: "nvidia_gb200", body: `{"siteId":"` + f.siteID + `","hardwareClass":"x_y","selection":{"mode":"All"},"ifVersionMatch":"V1"}`, wantStatus: http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, tc.replyErr)
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodPatch, target: "/", contentType: "application/json", body: tc.body, params: map[string]string{"hardwareClass": tc.class}})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			require.Len(t, f.proxied, tc.wantStarts)
			if tc.wantMessage != "" {
				assert.Contains(t, decodeAPIError(t, rec).Message, tc.wantMessage)
			}
			if tc.wantStarts == 0 {
				return
			}
			var coreReq corev1.UpdateAttestationProfileRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, tc.class, coreReq.GetHardwareClass())
			assert.Equal(t, tc.wantVersion, coreReq.GetIfVersionMatch())
		})
	}
}

func TestDeleteAttestationProfileHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewDeleteAttestationProfileHandler(f.dbSession, f.scp)

	cases := []struct {
		name       string
		class      string
		query      string
		body       string
		replyErr   error
		wantStatus int
		wantStarts int
	}{
		{name: "deletes", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&ifVersionMatch=V1", wantStatus: http.StatusNoContent, wantStarts: 1},
		{name: "nonempty body", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&ifVersionMatch=V1", body: "{}", wantStatus: http.StatusBadRequest},
		{name: "invalid UTF-8 version", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&ifVersionMatch=%FF", wantStatus: http.StatusBadRequest},
		{name: "decoded NUL version", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&ifVersionMatch=%00", wantStatus: http.StatusBadRequest},
		{name: "any without acknowledgement", class: "any", query: "siteId=" + f.siteID + "&ifVersionMatch=V1", wantStatus: http.StatusBadRequest},
		{name: "any acknowledged", class: "any", query: "siteId=" + f.siteID + "&ifVersionMatch=V1&acknowledgeFallbackImpact=true", wantStatus: http.StatusNoContent, wantStarts: 1},
		{name: "absent profile", class: "nvidia_gb200", query: "siteId=" + f.siteID + "&ifVersionMatch=V1", replyErr: coreError(swe.ErrTypeNICoObjectNotFound, "gone"), wantStatus: http.StatusNotFound, wantStarts: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, nil, tc.replyErr)
			if tc.replyErr == nil {
				f.replyRaw(`{}`)
			}
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodDelete, target: "/?" + tc.query, body: tc.body, params: map[string]string{"hardwareClass": tc.class}})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			assert.Len(t, f.proxied, tc.wantStarts)
			if tc.wantStatus == http.StatusNoContent {
				assert.Empty(t, rec.Body.String())
				var coreReq corev1.DeleteAttestationProfileRequest
				require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
				assert.Equal(t, "V1", coreReq.GetIfVersionMatch())
			}
		})
	}
}

func TestGetAttestationCoverageHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAttestationCoverageHandler(f.dbSession, f.scp)
	all := corev1.AttesterSelectionMode_ATTESTER_SELECTION_MODE_ALL

	cases := []struct {
		name       string
		query      string
		replyMsg   proto.Message
		wantStatus int
		wantJSON   string
	}{
		{name: "report", query: "siteId=" + f.siteID, replyMsg: &corev1.GetAttestationCoverageResponse{AnyProfileMode: &all, Entries: []*corev1.AttestationCoverageEntry{
			{HardwareClass: "", ExploredEndpoints: 2, Coverage: corev1.AttestationCoverage_ATTESTATION_COVERAGE_ANY_FALLBACK, Mode: &all},
		}}, wantStatus: http.StatusOK, wantJSON: `{"anyProfileMode":"All","entries":[{"hardwareClass":"","exploredEndpoints":2,"coverage":"AnyFallback","mode":"All","attesterSets":[]}]}`},
		{name: "missing site", query: "", wantStatus: http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, nil)
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantJSON != "" {
				assert.JSONEq(t, tc.wantJSON, rec.Body.String())
				assert.Equal(t, corev1.Forge_GetAttestationCoverage_FullMethodName, f.proxied[0].FullMethod)
			}
		})
	}
}
