// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cluster

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"

	"golang.org/x/net/http/httpguts"
	coordv1 "k8s.io/api/coordination/v1"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"
)

const (
	managedByMat        = "app.kubernetes.io/managed-by=mat-k8s-controller"
	labelMachineID      = "nvidia-infra-controller/mat-machine-id"
	labelMachineType    = "nvidia-infra-controller/mat-machine-type"
	labelMatID          = "nvidia-infra-controller/mat-id"
	annotationBmcIP     = "nvidia-infra-controller/mat-bmc-ip"
	annotationHwType    = "nvidia-infra-controller/mat-hardware-type"
	leases              = "/apis/coordination.k8s.io/v1"
	core                = "/api/v1"
	listPage            = 500
	maxListItems        = 100_000
	listTimeout         = maxListItems / listPage * time.Second // a second a page at the cap
	maxReplyBytes       = 16 << 20
	errorBodyWait       = time.Second // how long a non-2xx body is read for its reason
	serviceAccountToken = "/var/run/secrets/kubernetes.io/serviceaccount/token"
)

// Kube is Cluster on the Kubernetes API, on raw HTTP so that only a 2xx is a
// success, a 201 stays visible, no redirect is followed, no server text
// reaches an error and every reply is capped. Every call is bounded by
// callTimeout whoever calls it, a list by listTimeout over its pages. No
// reply body is ever logged.
type Kube struct {
	client       *http.Client
	base, ns     string
	callTimeout  time.Duration
	listTimeout  time.Duration
	maxReplySize int
}

// New uses the in-cluster config, else the local kubeconfig.
func New(namespace string) (*Kube, error) {
	cfg, err := rest.InClusterConfig()
	if err != nil {
		loader := clientcmd.NewNonInteractiveDeferredLoadingClientConfig(clientcmd.NewDefaultClientConfigLoadingRules(), nil)
		// ClientConfig reads a token file with no deadline, a FIFO for ever:
		// the raw config is checked first.
		raw, _ := loader.RawConfig()
		if c := raw.Contexts[raw.CurrentContext]; c != nil {
			if a := raw.AuthInfos[c.AuthInfo]; a != nil && a.TokenFile != "" && a.TokenFile != serviceAccountToken {
				return nil, unsupported
			}
		}
		cfg, err = loader.ClientConfig()
	}
	if err != nil {
		return nil, Error{Kind: Invalid, Text: "no Kubernetes client config"}
	}
	return NewFor(cfg, namespace)
}

var unsupported = Error{Kind: Invalid, Text: "credential plugins and token files are not supported"}

// NewFor refuses a config that runs a credential plugin or reads a token
// file other than the pod's own: the client would run either with no
// deadline. It refuses a bearer token no header can carry too: net/http
// would refuse each call before sending it, as if the connection failed.
// The token is trimmed first, as client-go trims a token file: an in-cluster
// token read with its newline still works.
func NewFor(cfg *rest.Config, namespace string) (*Kube, error) {
	cfg = rest.CopyConfig(cfg)
	cfg.BearerToken = strings.TrimSpace(cfg.BearerToken)
	switch {
	case cfg.ExecProvider != nil || cfg.AuthProvider != nil ||
		cfg.BearerTokenFile != "" && cfg.BearerTokenFile != serviceAccountToken:
		return nil, unsupported
	case strings.ContainsFunc(cfg.BearerToken, func(r rune) bool { return r < ' ' && r != '\t' || r == 0x7f }):
		return nil, Error{Kind: Invalid, Text: "the bearer token is not a valid header value"}
	}
	// Every header value client-go would add must be one net/http can send, or
	// it refuses each call before sending it, with no deadline. client-go
	// escapes the impersonation Extra keys itself, so only the values are here.
	headers := append([]string{cfg.Impersonate.UserName, cfg.Impersonate.UID, cfg.UserAgent}, cfg.Impersonate.Groups...)
	for _, vs := range cfg.Impersonate.Extra {
		headers = append(headers, vs...)
	}
	for _, v := range headers {
		if !httpguts.ValidHeaderFieldValue(v) {
			return nil, Error{Kind: Invalid, Text: "a request header value is not valid"}
		}
	}
	base, _, err := rest.DefaultServerUrlFor(cfg)
	if err != nil {
		return nil, Error{Kind: Invalid, Text: "no Kubernetes client config"}
	}
	if err := validServerURL(base); err != nil {
		return nil, err
	}
	// A supplied wrapper would be dropped by the hardened one below, silently
	// losing any auth or header injection it carried: refuse it for v1.
	if cfg.WrapTransport != nil {
		return nil, Error{Kind: Invalid, Text: "a Kubernetes transport wrapper is not supported"}
	}
	// Own the transport so net/http makes one attempt per call: client-go's can
	// replay an idempotent HTTP/1 GET on a reused connection and an HTTP/2
	// REFUSED_STREAM. WrapTransport sees the base before the auth wrappers; a
	// clone with keep-alives off and HTTP/1 only keeps its TLS, proxy and dial
	// and never mutates the base client-go may share.
	var wrapErr error
	cfg.WrapTransport = func(rt http.RoundTripper) http.RoundTripper {
		base, ok := rt.(*http.Transport)
		if !ok {
			wrapErr = Error{Kind: Invalid, Text: "the Kubernetes transport is not supported"}
			return rt
		}
		return oneShotTransport(base)
	}
	shared, err := rest.HTTPClientFor(cfg)
	switch {
	case wrapErr != nil:
		return nil, wrapErr
	case err != nil:
		return nil, Error{Kind: Invalid, Text: "TLS failed"}
	}
	// A copy: the client may be one client-go shares, http.DefaultClient too.
	client := *shared
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	return &Kube{&client, strings.TrimSuffix(base.String(), "/"), namespace, CallTimeout, listTimeout, maxReplyBytes}, nil
}

// validServerURL rejects a resolved server URL net/http cannot safely use: a
// non-HTTP(S) scheme, no host, a port out of range or zero, or a query or
// fragment that would displace the request path. A bare "?" delimiter counts
// as a query even with no pairs. A valid API path prefix is kept, and no
// offending value reaches the error.
func validServerURL(u *url.URL) error {
	bad := Error{Kind: Invalid, Text: "the Kubernetes server URL is not valid"}
	if u.Scheme != "http" && u.Scheme != "https" || u.Hostname() == "" || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" {
		return bad
	}
	if p := u.Port(); p != "" {
		if n, err := strconv.ParseUint(p, 10, 16); err != nil || n == 0 {
			return bad
		}
	}
	return nil
}

// oneShotTransport clones base so net/http retries nothing: keep-alives off
// leaves no reused connection to replay a GET on, and HTTP/1 only drops the
// HTTP/2 REFUSED_STREAM retry. The clone keeps base's TLS, proxy and dial and
// never mutates base, which client-go may share.
func oneShotTransport(base *http.Transport) *http.Transport {
	t := base.Clone()
	t.DisableKeepAlives = true
	t.ForceAttemptHTTP2 = false
	t.TLSNextProto = map[string]func(string, *tls.Conn) http.RoundTripper{}
	only1 := new(http.Protocols)
	only1.SetHTTP1(true)
	t.Protocols = only1
	if t.TLSClientConfig != nil {
		t.TLSClientConfig.NextProtos = []string{"http/1.1"}
	}
	return t
}

func (k *Kube) url(api, ns, kind, name string) string {
	u := k.base + api + "/namespaces/" + url.PathEscape(ns) + "/" + kind
	if name != "" {
		u += "/" + url.PathEscape(name)
	}
	return u
}

func (k *Kube) deadline() time.Time { return time.Now().Add(k.callTimeout) }

func (k *Kube) timedOut() Error {
	return Error{Kind: Timeout, Secs: int64(k.callTimeout / time.Second)}
}

var errTimedOut = errors.New("timed out")

// call sends one request by until and decodes a 2xx body into out. The
// headers must arrive by until less the error body wait, so a refusal keeps
// its status inside the call's own time. Any other status is an error with
// its code and a known reason, also when its body does not arrive. The
// status is returned only for a 2xx, also with a body error: a 201 keeps its
// meaning whatever its body holds, but a 2xx read whole after until or a
// cancel is the timeout or the cancel. A cancel of ctx wins over every other
// outcome but a non-2xx status.
func (k *Kube) call(ctx context.Context, method, url string, in, out any, until time.Time) (int, error) {
	var body io.Reader
	if in != nil {
		data, err := json.Marshal(in)
		if err != nil {
			return 0, Error{Kind: Invalid, Text: "the request did not build"}
		}
		body = bytes.NewReader(data)
	}
	callCtx, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	req, err := http.NewRequestWithContext(callCtx, method, url, body)
	if err != nil {
		return 0, Error{Kind: Invalid, Text: "the request did not build"}
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("Content-Type", "application/json")
	headersBy := until.Add(-errorBodyWait)
	if !headersBy.After(time.Now()) {
		headersBy = until
	}
	timer := time.AfterFunc(time.Until(headersBy), func() { cancel(errTimedOut) })
	defer timer.Stop()
	failed := func(otherwise Error) error {
		if s := callerStopped(ctx); s != nil {
			return s
		}
		if context.Cause(callCtx) == errTimedOut || !time.Now().Before(until) {
			return k.timedOut()
		}
		return otherwise
	}
	// The caller's own cancel or elapsed deadline ends the call before it is
	// sent, so an already-expired caller dispatches nothing.
	if s := callerStopped(ctx); s != nil {
		return 0, s
	}
	resp, err := k.client.Do(req)
	if err != nil {
		var tlsErr *tls.CertificateVerificationError
		var header tls.RecordHeaderError
		if errors.As(err, &tlsErr) || errors.As(err, &header) {
			return 0, failed(Error{Kind: Invalid, Text: "TLS failed"})
		}
		return 0, failed(Error{Kind: Transport, Text: "the connection failed"})
	}
	defer func() { _ = resp.Body.Close() }()
	// An upgraded stream: no cancel ends a read of it.
	if resp.StatusCode == http.StatusSwitchingProtocols {
		return 0, statusError(resp.StatusCode, "")
	}
	ok := resp.StatusCode >= 200 && resp.StatusCode < 300
	if bodyBy := time.Now().Add(errorBodyWait); ok || until.Before(bodyBy) {
		timer.Reset(time.Until(until))
	} else {
		timer.Reset(time.Until(bodyBy))
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, int64(k.maxReplySize)+1))
	if !ok {
		var status metav1.Status
		if err != nil || len(data) > k.maxReplySize || json.Unmarshal(data, &status) != nil {
			status.Reason = ""
		}
		return 0, statusError(resp.StatusCode, string(status.Reason))
	}
	switch {
	case err != nil:
		err = Error{Kind: Transport, Text: "the reply body did not arrive whole"}
	case len(data) > k.maxReplySize:
		err = Error{Kind: Invalid, Text: fmt.Sprintf("the reply is over %d bytes", k.maxReplySize)}
	case json.Unmarshal(data, out) != nil:
		err = Error{Kind: Invalid, Text: "the reply did not decode"}
	}
	// Checked last, so a cancel or the deadline during the read or the decode
	// wins over any of them, even a caller deadline whose timer has not fired.
	if callerStopped(ctx) != nil || callCtx.Err() != nil || !time.Now().Before(until) {
		err = failed(k.timedOut())
	}
	return resp.StatusCode, err
}

// callerStopped is the caller's own cancel or elapsed deadline, which outranks
// the adapter's own deadline, or nil. The deadline is read directly so an
// expiry whose context timer has not fired yet still counts.
func callerStopped(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if d, ok := ctx.Deadline(); ok && !time.Now().Before(d) {
		return context.DeadlineExceeded
	}
	return nil
}

// object sends one request and checks that a 2xx reply is the stored object
// name: its own name, a resource version and a UID.
func (k *Kube) object(ctx context.Context, method, url string, in any, out metav1.Object, name string, until time.Time) (int, error) {
	status, err := k.call(ctx, method, url, in, out, until)
	if err == nil && (out.GetName() != name || out.GetResourceVersion() == "" || out.GetUID() == "") {
		err = Error{Kind: Invalid, Text: "the reply is not the stored object"}
	}
	return status, err
}

// get reads the object name, or reports false on a 404.
func (k *Kube) get(ctx context.Context, url string, out metav1.Object, name string) (bool, error) {
	_, err := k.object(ctx, http.MethodGet, url, nil, out, name, k.deadline())
	if hasKind(err, API, 404) {
		return false, nil
	}
	return err == nil, err
}

func hasKind(err error, kind Kind, code int) bool {
	var e Error
	return errors.As(err, &e) && e.Kind == kind && e.Code == code
}

// statusError keeps the code and a known reason, never the server's message.
func statusError(code int, reason string) error {
	switch {
	case code == http.StatusConflict && reason == "AlreadyExists":
		return Error{Kind: AlreadyExists}
	case code == http.StatusConflict:
		return Error{Kind: Conflict}
	case code == http.StatusForbidden:
		return Error{Kind: Forbidden, Text: knownReason(reason)}
	}
	return Error{Kind: API, Code: code, Text: knownReason(reason)}
}

var knownReasons = []string{
	"BadRequest", "Unauthorized", "Forbidden", "NotFound", "AlreadyExists", "Conflict", "Gone",
	"Invalid", "ServerTimeout", "Timeout", "TooManyRequests", "MethodNotAllowed", "NotAcceptable",
	"RequestEntityTooLarge", "UnsupportedMediaType", "InternalError", "Expired", "ServiceUnavailable",
}

func knownReason(reason string) string {
	if slices.Contains(knownReasons, reason) {
		return reason
	}
	return "no known reason"
}

// swappable refuses a version the API would replace on unconditionally.
func swappable(version string) error {
	if version == "" || version == "0" {
		return Error{Kind: Conflict}
	}
	return nil
}

// listAll reads every object with selector in pages, up to max in all. Each
// page has the call timeout, the whole list listTimeout. A page without a
// list resource version, a continue token that does not move on, or more
// than max objects is an error.
func listAll[T any](ctx context.Context, k *Kube, url, selector string, max int) ([]T, error) {
	listUntil := time.Now().Add(k.listTimeout)
	var items []T
	token := ""
	for {
		query := urlValues(selector, token)
		// The whole-list bound is checked here, before a page is dispatched: an
		// elapsed listUntil stops the list rather than starting another page
		// that k.call would only cancel once its timer runs. A caller cancel or
		// deadline outranks it, so when the caller has stopped this defers to
		// k.call, which returns the caller's own error.
		if callerStopped(ctx) == nil && !time.Now().Before(listUntil) {
			return nil, Error{Kind: Timeout, Secs: int64(k.listTimeout / time.Second)}
		}
		until := k.deadline()
		if listUntil.Before(until) {
			until = listUntil
		}
		var page struct {
			Metadata metav1.ListMeta `json:"metadata"`
			Items    []T             `json:"items"`
		}
		_, err := k.call(ctx, http.MethodGet, url+"?"+query, nil, &page, until)
		if hasKind(err, Timeout, 0) && until.Equal(listUntil) {
			return nil, Error{Kind: Timeout, Secs: int64(k.listTimeout / time.Second)}
		}
		switch {
		case err != nil:
			return nil, err
		case page.Metadata.ResourceVersion == "":
			return nil, Error{Kind: Invalid, Text: "the reply is not a list of the API"}
		case len(items)+len(page.Items) > max:
			return nil, Error{Kind: Invalid, Text: fmt.Sprintf("the list holds over %d objects", max)}
		}
		items = append(items, page.Items...)
		next := page.Metadata.Continue
		if next == "" {
			return items, nil
		}
		if next == token {
			return nil, Error{Kind: Invalid, Text: "the list continue token did not move on"}
		}
		token = next
	}
}

func urlValues(selector, token string) string {
	q := url.Values{"labelSelector": {selector}, "limit": {fmt.Sprint(listPage)}}
	if token != "" {
		q.Set("continue", token)
	}
	return q.Encode()
}

func matServiceFrom(s corev1.Service) MatService {
	ip := func(raw string) netip.Addr { a, _ := netip.ParseAddr(raw); return a }
	m := MatService{
		Name: s.Name, MachineType: s.Labels[labelMachineType], MachineID: s.Labels[labelMachineID],
		MatID: s.Labels[labelMatID], BmcIP: ip(s.Annotations[annotationBmcIP]), HardwareType: s.Annotations[annotationHwType],
	}
	for _, raw := range s.Spec.ExternalIPs {
		if a := ip(raw); a.IsValid() {
			m.ExternalIPs = append(m.ExternalIPs, a)
		}
	}
	return m
}

func podFrom(p corev1.Pod) PodInfo {
	info := PodInfo{
		Name: p.Name, UID: string(p.UID), Labels: p.Labels, Annotations: p.Annotations, Phase: string(p.Status.Phase),
		CreatedAt: p.CreationTimestamp.UTC(), IP: p.Status.PodIP,
	}
	if p.CreationTimestamp.IsZero() {
		info.CreatedAt = time.Unix(0, 0).UTC()
	}
	for _, c := range p.Status.Conditions {
		info.Ready = info.Ready || c.Type == corev1.PodReady && c.Status == corev1.ConditionTrue
	}
	// A restartable init container (a native sidecar) restarts on its own,
	// so its count goes in with the main containers.
	for _, c := range slices.Concat(p.Status.ContainerStatuses, p.Status.InitContainerStatuses) {
		info.Restarts = uint32(min(uint64(info.Restarts)+uint64(max(c.RestartCount, 0)), math.MaxUint32))
	}
	return info
}

func (k *Kube) ListMatServices(ctx context.Context) ([]MatService, error) {
	list, err := listAll[corev1.Service](ctx, k, k.url(core, k.ns, "services", ""), managedByMat, maxListItems)
	out := make([]MatService, 0, len(list))
	for _, s := range list {
		out = append(out, matServiceFrom(s))
	}
	return out, err
}

func (k *Kube) ListPods(ctx context.Context, namespace, selector string) ([]PodInfo, error) {
	list, err := listAll[corev1.Pod](ctx, k, k.url(core, namespace, "pods", ""), selector, maxListItems)
	out := make([]PodInfo, 0, len(list))
	for _, p := range list {
		out = append(out, podFrom(p))
	}
	return out, err
}

// The resource version and UID go along: the API refuses a replace when
// either changed, and a replace with the UID never makes a missing Lease.
func leaseTo(l Lease) *coordv1.Lease {
	o := &coordv1.Lease{
		TypeMeta:   metav1.TypeMeta{APIVersion: "coordination.k8s.io/v1", Kind: "Lease"},
		ObjectMeta: metav1.ObjectMeta{Name: l.Name, ResourceVersion: l.ResourceVersion, UID: types.UID(l.UID), Annotations: l.Annotations},
		Spec:       coordv1.LeaseSpec{LeaseDurationSeconds: &l.LeaseDurationSeconds},
	}
	if l.HolderIdentity != "" {
		o.Spec.HolderIdentity = &l.HolderIdentity
	}
	if !l.AcquireTime.IsZero() {
		o.Spec.AcquireTime = &metav1.MicroTime{Time: l.AcquireTime}
	}
	if !l.RenewTime.IsZero() {
		o.Spec.RenewTime = &metav1.MicroTime{Time: l.RenewTime}
	}
	return o
}

func leaseFrom(o *coordv1.Lease) Lease {
	l := Lease{Name: o.Name, ResourceVersion: o.ResourceVersion, UID: string(o.UID), Annotations: o.Annotations}
	if s := o.Spec; s.HolderIdentity != nil {
		l.HolderIdentity = *s.HolderIdentity
	}
	if o.Spec.LeaseDurationSeconds != nil {
		l.LeaseDurationSeconds = *o.Spec.LeaseDurationSeconds
	}
	if o.Spec.AcquireTime != nil {
		l.AcquireTime = o.Spec.AcquireTime.UTC()
	}
	if o.Spec.RenewTime != nil {
		l.RenewTime = o.Spec.RenewTime.UTC()
	}
	return l
}

func (k *Kube) GetLease(ctx context.Context, name string) (*Lease, error) {
	var o coordv1.Lease
	if found, err := k.get(ctx, k.url(leases, k.ns, "leases", name), &o, name); !found {
		return nil, err
	}
	l := leaseFrom(&o)
	return &l, nil
}

func (k *Kube) CreateLease(ctx context.Context, l Lease) (Lease, error) {
	l.ResourceVersion, l.UID = "", ""
	var o coordv1.Lease
	_, err := k.object(ctx, http.MethodPost, k.url(leases, k.ns, "leases", ""), leaseTo(l), &o, l.Name, k.deadline())
	return leaseFrom(&o), err
}

func (k *Kube) ReplaceLease(ctx context.Context, l Lease) (LeaseReplaced, error) {
	if err := swappable(l.ResourceVersion); err != nil {
		return LeaseReplaced{}, err
	}
	// Without the UID the API makes the Lease when it is missing.
	if l.UID == "" {
		return LeaseReplaced{}, Error{Kind: Invalid, Text: "a Lease replace needs the Lease UID"}
	}
	var o coordv1.Lease
	status, err := k.object(ctx, http.MethodPut, k.url(leases, k.ns, "leases", l.Name), leaseTo(l), &o, l.Name, k.deadline())
	r := LeaseReplaced{Created: status == http.StatusCreated}
	if err == nil {
		stored := leaseFrom(&o)
		r.Lease = &stored
	} else if !r.Created {
		return LeaseReplaced{}, err
	}
	return r, nil
}
