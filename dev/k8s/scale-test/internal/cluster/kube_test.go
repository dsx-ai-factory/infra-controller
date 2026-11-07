// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cluster

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"go.uber.org/goleak"
	coordv1 "k8s.io/api/coordination/v1"
	corev1 "k8s.io/api/core/v1"
	"k8s.io/client-go/rest"
	clientcmdapi "k8s.io/client-go/tools/clientcmd/api"
)

const secret = "token=s3cret https://user:pw@api.example"

var bg = context.Background()

func TestMain(m *testing.M) {
	code := m.Run()
	http.DefaultTransport.(*http.Transport).CloseIdleConnections()
	if err := goleak.Find(goleak.IgnoreAnyFunction("k8s.io/klog/v2.(*flushDaemon).run.func1")); err != nil && code == 0 {
		fmt.Println(err)
		code = 1
	}
	os.Exit(code)
}

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s: got %v, want %v", what, got, want)
	}
}

// served is a cluster whose API is routes, with a 1 s call and a 4,096 byte
// reply.
func served(t *testing.T, routes map[string]http.HandlerFunc) *Kube {
	mux := http.NewServeMux()
	for pattern, h := range routes {
		mux.HandleFunc(pattern, h)
	}
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	k, err := NewFor(&rest.Config{Host: srv.URL}, "ns")
	if err != nil {
		t.Fatal(err)
	}
	k.callTimeout, k.maxReplySize = time.Second, 4096
	return k
}

func reply(status int, body string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}
}

// endless sends status after wait, then a body that never ends.
func endless(status int, wait time.Duration) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(wait)
		w.WriteHeader(status)
		_, _ = io.WriteString(w, "{")
		w.(http.Flusher).Flush()
		<-r.Context().Done()
	}
}

func object(kind, name, version, uid string) string {
	return fmt.Sprintf(`{"kind":%q,"metadata":{"name":%q,"resourceVersion":%q,"uid":%q}}`, kind, name, version, uid)
}

func podList(next string, names ...string) string {
	items := make([]string, len(names))
	for i, n := range names {
		items[i] = object("Pod", n, "1", "u-"+n)
	}
	return `{"kind":"PodList","metadata":{"resourceVersion":"9","continue":"` + next + `"},"items":[` + strings.Join(items, ",") + `]}`
}

func podNames(pods []PodInfo) (out []string) {
	for _, p := range pods {
		out = append(out, p.Name)
	}
	return out
}

// seen keeps what a handler saw, safe to read from the test.
type seen struct {
	mu    sync.Mutex
	items []string
}

func (s *seen) add(v string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.items = append(s.items, v)
}

func (s *seen) all() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.items...)
}

func testLease(version string) Lease {
	return Lease{Name: "l", ResourceVersion: version, UID: "u1", LeaseDurationSeconds: 60}
}

func api(code int, reason string) error { return Error{Kind: API, Code: code, Text: reason} }

type roundTrip func(*http.Request) (*http.Response, error)

func (f roundTrip) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

// decodeThen is a reply that runs f once it decoded.
type decodeThen func()

func (f decodeThen) UnmarshalJSON([]byte) error { f(); return nil }

// eofAfter runs at the end of a reply body, after its last byte was read.
type eofAfter func()

func (f eofAfter) Read([]byte) (int, error) { f(); return 0, io.EOF }

// errReader is a reply body that truncates: it returns no byte and fails.
type errReader struct{}

func (errReader) Read([]byte) (int, error) { return 0, io.ErrUnexpectedEOF }

// countingBody is a body of left bytes that records how many were read, to
// prove the client stops reading at the cap.
type countingBody struct {
	left int
	read *int
}

func (b *countingBody) Read(p []byte) (int, error) {
	if b.left == 0 {
		return 0, io.EOF
	}
	n := min(len(p), b.left)
	b.left -= n
	*b.read += n
	return n, nil
}

// stuckDeadline has a deadline whose Done never closes, as a caller whose
// context timer has not fired yet.
type stuckDeadline struct {
	context.Context
	at time.Time
}

func (c stuckDeadline) Deadline() (time.Time, bool) { return c.at, true }

const leasePath = "/apis/coordination.k8s.io/v1/namespaces/ns/leases"

func TestKube(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(t *testing.T)
	}{
		{"a list dispatches no page once its deadline has passed", func(t *testing.T) {
			// listUntil is already elapsed at entry, so the pre-page check must
			// stop the list before any page is sent, not hand k.call an expired
			// bound that it would only cancel after dispatch.
			k := served(t, nil)
			k.listTimeout = 0
			var calls atomic.Int32
			k.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
				calls.Add(1)
				return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(podList("c1", "p"))), Header: http.Header{}}, nil
			})
			_, err := listAll[corev1.Pod](bg, k, k.url(core, "ns", "pods", ""), "app=x", 1000)
			is(t, [2]any{hasKind(err, Timeout, 0), calls.Load()}, [2]any{true, int32(0)}, "deadline stops dispatch")
		}},
		{"lists are read in pages and capped", func(t *testing.T) {
			var asked seen
			pages := map[string]string{"": podList("c1", "a", "b"), "c1": podList("c2", "c", "d"), "c2": podList("", "e")}
			k := served(t, map[string]http.HandlerFunc{"GET /api/v1/namespaces/ns/pods": func(w http.ResponseWriter, r *http.Request) {
				q := r.URL.Query()
				asked.add("limit=" + q.Get("limit") + " continue=" + q.Get("continue") + " " + q.Get("labelSelector"))
				_, _ = io.WriteString(w, pages[q.Get("continue")])
			}})
			pods, err := k.ListPods(bg, "ns", "app=x")
			is(t, [2]any{podNames(pods), err}, [2]any{[]string{"a", "b", "c", "d", "e"}, nil}, "pods")
			is(t, asked.all(), []string{"limit=500 continue= app=x", "limit=500 continue=c1 app=x", "limit=500 continue=c2 app=x"}, "pages")
			_, err = listAll[corev1.Pod](bg, k, k.url(core, "ns", "pods", ""), "app=x", 3)
			is(t, err, error(Error{Kind: Invalid, Text: "the list holds over 3 objects"}), "capped")
			// A page that is not a list of the API, and a token that does not move.
			k2 := served(t, map[string]http.HandlerFunc{"GET /api/v1/namespaces/ns/pods": reply(200, `{"items":[]}`)})
			_, err = k2.ListPods(bg, "ns", "app=x")
			is(t, err, error(Error{Kind: Invalid, Text: "the reply is not a list of the API"}), "no list version")
			k3 := served(t, map[string]http.HandlerFunc{"GET /api/v1/namespaces/ns/pods": reply(200, podList("c1", "a"))})
			_, err = k3.ListPods(bg, "ns", "app=x")
			is(t, err, error(Error{Kind: Invalid, Text: "the list continue token did not move on"}), "stuck token")
		}},
		{"each list page has its own deadline inside the list's", func(t *testing.T) {
			// Pages that each answer in 400 ms, every one with a next page.
			k := served(t, map[string]http.HandlerFunc{"GET /api/v1/namespaces/ns/pods": func(w http.ResponseWriter, r *http.Request) {
				time.Sleep(400 * time.Millisecond)
				page, _ := strconv.Atoi(r.URL.Query().Get("continue"))
				_, _ = io.WriteString(w, podList(strconv.Itoa(page+1), "p"+strconv.Itoa(page)))
			}})
			k.listTimeout = 2 * time.Second
			// Four pages take 1.6 s, past the 1 s call timeout, but under the list.
			_, err := listAll[corev1.Pod](bg, k, k.url(core, "ns", "pods", ""), "app=x", 3)
			is(t, err, error(Error{Kind: Invalid, Text: "the list holds over 3 objects"}), "the fourth page is read")
			// A list that never ends stops at the list's own time.
			started := time.Now()
			_, err = listAll[corev1.Pod](bg, k, k.url(core, "ns", "pods", ""), "app=x", 1000)
			is(t, [2]any{err, time.Since(started) < 3*time.Second}, [2]any{error(Error{Kind: Timeout, Secs: 2}), true}, "endless")
			is(t, listTimeout, 200*time.Second, "a second a page")
		}},
		{"a service and a pod map their fields", func(t *testing.T) {
			svc := `{"metadata":{"name":"host-1","labels":{"nvidia-infra-controller/mat-machine-type":"host",` +
				`"nvidia-infra-controller/mat-machine-id":"m1","nvidia-infra-controller/mat-id":"7"},` +
				`"annotations":{"nvidia-infra-controller/mat-bmc-ip":"10.0.0.1","nvidia-infra-controller/mat-hardware-type":"gb200"}},` +
				`"spec":{"externalIPs":["10.0.0.1","bad","2001:db8::1"]}}`
			pod := `{"metadata":{"name":"mat-0","uid":"u-0","creationTimestamp":"2026-01-02T03:04:05Z","labels":{"app":"x"}},` +
				`"status":{"phase":"Running","podIP":"10.1.2.3","conditions":[{"type":"Ready","status":"True"}],` +
				`"containerStatuses":[{"restartCount":2}],"initContainerStatuses":[{"restartCount":3}]}}`
			list := func(kind, item string) http.HandlerFunc {
				return reply(200, `{"kind":"`+kind+`","metadata":{"resourceVersion":"9"},"items":[`+item+`]}`)
			}
			k := served(t, map[string]http.HandlerFunc{
				"GET /api/v1/namespaces/ns/services": list("ServiceList", svc),
				"GET /api/v1/namespaces/ns/pods":     list("PodList", pod),
			})
			svcs, err1 := k.ListMatServices(bg)
			pods, err2 := k.ListPods(bg, "ns", "app=x")
			at, _ := time.Parse(time.RFC3339, "2026-01-02T03:04:05Z")
			is(t, [2]any{svcs[0], err1}, [2]any{MatService{
				Name: "host-1", MachineType: "host", MachineID: "m1", MatID: "7",
				BmcIP: netip.MustParseAddr("10.0.0.1"), HardwareType: "gb200",
				ExternalIPs: []netip.Addr{netip.MustParseAddr("10.0.0.1"), netip.MustParseAddr("2001:db8::1")},
			}, nil}, "service")
			is(t, [2]any{pods[0], err2}, [2]any{PodInfo{
				Name: "mat-0", UID: "u-0", Labels: map[string]string{"app": "x"}, Phase: "Running",
				Ready: true, CreatedAt: at.UTC(), Restarts: 5, IP: "10.1.2.3",
			}, nil}, "pod")
		}},
		{"a lease converts both ways", func(t *testing.T) {
			// The API keeps microseconds, also before the epoch.
			for _, raw := range []string{"1969-12-31T23:59:59.250001Z", "1969-12-31T23:59:59.5Z", "2300-01-01T00:00:00.25Z"} {
				at, _ := time.Parse(time.RFC3339Nano, raw)
				l := Lease{"l", "7", "u1", "test/r1/r1-x", 60, at, at.Add(time.Microsecond), map[string]string{"k": "v"}}
				is(t, leaseFrom(leaseTo(l)), l, raw)
				data, _ := json.Marshal(leaseTo(l))
				var wire coordv1.Lease
				is(t, [2]any{json.Unmarshal(data, &wire), leaseFrom(&wire)}, [2]any{nil, l}, raw+" on the wire")
			}
		}},
		{"only a 2xx reply is a success", func(t *testing.T) {
			// A redirect is no success, even with a whole stored object in it.
			redirect := func(body string) http.HandlerFunc {
				return func(w http.ResponseWriter, _ *http.Request) {
					w.Header().Set("Location", "/elsewhere")
					w.WriteHeader(http.StatusFound)
					_, _ = io.WriteString(w, body)
				}
			}
			k := served(t, map[string]http.HandlerFunc{
				leasePath + "/l": redirect(object("Lease", "l", "7", "u1")),
				"/elsewhere":     reply(200, object("Lease", "l", "7", "u1")),
			})
			want := api(302, "no known reason")
			_, err1 := k.GetLease(bg, "l")
			_, err2 := k.ReplaceLease(bg, testLease("7"))
			is(t, []error{err1, err2}, []error{want, want}, "302")
		}},
		{"a 2xx reply that is not the stored object is an error", func(t *testing.T) {
			// Each reply drops one identity field and keeps the other two, so
			// each guards its own check: name, resource version, UID.
			for _, body := range []string{
				object("Lease", "wrong", "7", "u1"),
				object("Lease", "l", "", "u1"),
				object("Lease", "l", "7", ""),
			} {
				k := served(t, map[string]http.HandlerFunc{leasePath + "/l": reply(200, body)})
				_, err := k.GetLease(bg, "l")
				is(t, err, error(Error{Kind: Invalid, Text: "the reply is not the stored object"}), body)
			}
		}},
		{"a lease replace sends the uid and reports a 201 as created", func(t *testing.T) {
			// The API answers 201 when the replace made the Lease, else 200.
			var sent seen
			k := served(t, map[string]http.HandlerFunc{"PUT " + leasePath + "/l": func(w http.ResponseWriter, r *http.Request) {
				var o coordv1.Lease
				_ = json.NewDecoder(r.Body).Decode(&o)
				sent.add(string(o.UID))
				reply(map[bool]int{true: 200, false: 201}[o.ResourceVersion == "7"], object("Lease", "l", "8", "u2"))(w, r)
			}})
			stored := &Lease{Name: "l", ResourceVersion: "8", UID: "u2"}
			r1, err1 := k.ReplaceLease(bg, testLease("7"))
			r2, err2 := k.ReplaceLease(bg, testLease("6"))
			is(t, [4]any{r1, err1, r2, err2}, [4]any{LeaseReplaced{Lease: stored}, nil, LeaseReplaced{Created: true, Lease: stored}, nil}, "replies")
			is(t, sent.all(), []string{"u1", "u1"}, "the UID is sent")
		}},
		{"a 201 lease replace with a bad body is still created", func(t *testing.T) {
			k := served(t, map[string]http.HandlerFunc{"PUT " + leasePath + "/l": func(w http.ResponseWriter, r *http.Request) {
				var o coordv1.Lease
				_ = json.NewDecoder(r.Body).Decode(&o)
				reply(map[bool]int{true: 200, false: 201}[o.ResourceVersion == "7"], `{"kind":`)(w, r)
			}})
			r1, err1 := k.ReplaceLease(bg, testLease("6"))
			_, err2 := k.ReplaceLease(bg, testLease("7"))
			is(t, [3]any{r1, err1, err2}, [3]any{LeaseReplaced{Created: true}, nil, error(Error{Kind: Invalid, Text: "the reply did not decode"})}, "replies")
		}},
		{"errors carry no server or client text", func(t *testing.T) {
			status := func(code int, reason string) http.HandlerFunc {
				return reply(code, fmt.Sprintf(`{"kind":"Status","code":%d,"reason":%q,"message":%q}`, code, reason, secret))
			}
			k := served(t, map[string]http.HandlerFunc{
				leasePath + "/forbidden": status(403, "Forbidden"),
				leasePath + "/known":     status(500, "InternalError"),
				leasePath + "/unknown":   status(500, "SecretToken123"),
				leasePath + "/text":      reply(502, secret),
				leasePath + "/gone":      status(404, "NotFound"),
			})
			for name, want := range map[string]string{
				"forbidden": "forbidden: Forbidden", "known": "kubernetes API error 500: InternalError",
				"unknown": "kubernetes API error 500: no known reason", "text": "kubernetes API error 502: no known reason",
			} {
				_, err := k.GetLease(bg, name)
				is(t, fmt.Sprint(err), want, name)
			}
			l, err := k.GetLease(bg, "gone")
			is(t, [2]any{l, err}, [2]any{(*Lease)(nil), nil}, "gone")
			gone, _ := NewFor(&rest.Config{Host: "http://user:pw@127.0.0.1:1"}, "ns")
			_, err = gone.GetLease(bg, "l")
			is(t, fmt.Sprint(err), "kubernetes API unreachable: the connection failed", "transport")
			is(t, [2]error{statusError(409, "AlreadyExists"), statusError(409, "Conflict")}, [2]error{Error{Kind: AlreadyExists}, Error{Kind: Conflict}}, "409")
		}},
		{"a reply over its size is an error that keeps its status", func(t *testing.T) {
			big := strings.Repeat("x", 10_000)
			k := served(t, map[string]http.HandlerFunc{
				leasePath + "/l": reply(200, big), leasePath + "/gone": reply(404, big), leasePath + "/denied": reply(403, big),
			})
			_, err1 := k.GetLease(bg, "l")
			l, err2 := k.GetLease(bg, "gone")
			_, err3 := k.GetLease(bg, "denied")
			is(t, [4]any{err1, l, err2, err3}, [4]any{error(Error{Kind: Invalid, Text: "the reply is over 4096 bytes"}), (*Lease)(nil), nil,
				error(Error{Kind: Forbidden, Text: "no known reason"})}, "capped")
		}},
		{"an error whose body never ends keeps its status", func(t *testing.T) {
			k := served(t, map[string]http.HandlerFunc{leasePath + "/l": endless(403, 0)})
			k.callTimeout = 5 * time.Second
			_, err := k.GetLease(bg, "l")
			is(t, err, error(Error{Kind: Forbidden, Text: "no known reason"}), "forbidden")
		}},
		{"an error near the deadline keeps its status", func(t *testing.T) {
			// The 403 headers come at 0.5 s of the 1 s call.
			k := served(t, map[string]http.HandlerFunc{leasePath + "/l": endless(403, 500*time.Millisecond)})
			_, err := k.GetLease(bg, "l")
			is(t, err, error(Error{Kind: Forbidden, Text: "no known reason"}), "forbidden")
		}},
		{"credential plugins are refused", func(t *testing.T) {
			for _, tc := range []struct {
				cfg     rest.Config
				refused bool
			}{
				{rest.Config{}, false},
				{rest.Config{BearerTokenFile: serviceAccountToken}, false},
				{rest.Config{BearerTokenFile: "/tmp/fifo"}, true},
				{rest.Config{ExecProvider: &clientcmdapi.ExecConfig{Command: "helper"}}, true},
				{rest.Config{AuthProvider: &clientcmdapi.AuthProviderConfig{Name: "oidc"}}, true},
			} {
				tc.cfg.Host = "https://127.0.0.1:1"
				_, err := NewFor(&tc.cfg, "ns")
				is(t, err == unsupported, tc.refused, tc.cfg.BearerTokenFile)
			}
		}},
		{"a token read with its newline is sent without it", func(t *testing.T) {
			// As in a pod: the token file's content, and its path.
			var sent seen
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				sent.add(r.Header.Get("Authorization"))
				reply(200, object("Lease", "l", "7", "u1"))(w, r)
			}))
			t.Cleanup(srv.Close)
			var want []string
			for _, file := range []string{"", serviceAccountToken} {
				// Inside a pod the real SA token file would replace "t": skip that
				// accepted-path row only where the file is present.
				if _, err := os.Stat(file); file != "" && err == nil {
					continue
				}
				k, err := NewFor(&rest.Config{Host: srv.URL, BearerToken: "t\n", BearerTokenFile: file}, "ns")
				if err == nil {
					_, err = k.GetLease(bg, "l")
				}
				is(t, err, nil, file)
				want = append(want, "Bearer t")
			}
			is(t, sent.all(), want, "sent")
		}},
		{"an upgraded stream is refused unread", func(t *testing.T) {
			// The server holds the stream open until the client leaves, 5 s at
			// most: no cancel ends a read of it.
			closed := make(chan error, 1)
			k := served(t, map[string]http.HandlerFunc{leasePath + "/l": func(w http.ResponseWriter, _ *http.Request) {
				conn, rw, _ := w.(http.Hijacker).Hijack()
				defer func() { _ = conn.Close() }()
				_, _ = rw.WriteString("HTTP/1.1 101 Switching Protocols\r\nUpgrade: x\r\nConnection: Upgrade\r\n\r\n")
				_ = rw.Flush()
				_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
				_, err := conn.Read(make([]byte, 1))
				closed <- err
			}})
			started := time.Now()
			_, err := k.GetLease(bg, "l")
			is(t, [2]any{err, time.Since(started) < 500*time.Millisecond}, [2]any{api(101, "no known reason"), true}, "101")
			is(t, <-closed, io.EOF, "the client closed the stream") // not the 5 s read deadline
		}},
		{"a 2xx read whole after a cancel or the deadline does not count", func(t *testing.T) {
			// The whole body is read, then the caller cancels or the 1 s call
			// time passes before the read sees its end. A 201 stays created.
			k := served(t, nil)
			reply := func(code int, after func()) {
				k.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
					body := io.MultiReader(strings.NewReader(object("Lease", "l", "7", "u1")), eofAfter(after))
					return &http.Response{StatusCode: code, Body: io.NopCloser(body)}, nil
				})
			}
			ctx, cancel := context.WithCancel(bg)
			late := func() { time.Sleep(1100 * time.Millisecond) }
			reply(200, cancel)
			_, err1 := k.GetLease(ctx, "l")
			reply(200, late)
			_, err2 := k.GetLease(bg, "l")
			reply(201, late)
			r, err3 := k.ReplaceLease(bg, testLease("6"))
			is(t, [4]any{err1, err2, r, err3}, [4]any{context.Canceled, error(Error{Kind: Timeout, Secs: 1}), LeaseReplaced{Created: true}, nil}, "late")
		}},
		{"a cancel or the deadline during the decode does not count", func(t *testing.T) {
			k := served(t, map[string]http.HandlerFunc{leasePath + "/l": reply(201, `{}`)})
			ctx, cancel := context.WithCancel(bg)
			cancelled, late := decodeThen(cancel), decodeThen(func() { time.Sleep(1100 * time.Millisecond) })
			code, err1 := k.call(ctx, http.MethodGet, k.url(leases, k.ns, "leases", "l"), nil, &cancelled, k.deadline())
			_, err2 := k.call(bg, http.MethodGet, k.url(leases, k.ns, "leases", "l"), nil, &late, k.deadline())
			is(t, [3]any{code, err1, err2}, [3]any{201, context.Canceled, error(Error{Kind: Timeout, Secs: 1})}, "decode")
		}},
		{"every call is bounded and a replace needs a version", func(t *testing.T) {
			var asked seen
			hang := func(_ http.ResponseWriter, r *http.Request) {
				asked.add(r.Method)
				// A read body lets the server see the client leave.
				_, _ = io.Copy(io.Discard, r.Body)
				<-r.Context().Done()
			}
			k := served(t, map[string]http.HandlerFunc{leasePath: hang, leasePath + "/l": hang})
			calls := []func() error{
				func() error { _, err := k.GetLease(bg, "l"); return err },
				func() error { _, err := k.CreateLease(bg, testLease("")); return err },
				func() error { _, err := k.ReplaceLease(bg, testLease("7")); return err },
			}
			errs := make([]error, len(calls))
			started := time.Now()
			var wg sync.WaitGroup
			for i, call := range calls {
				wg.Go(func() { errs[i] = call() })
			}
			wg.Wait()
			is(t, time.Since(started) < 5*time.Second, true, "bounded")
			for i, err := range errs {
				is(t, err, error(Error{Kind: Timeout, Secs: 1}), fmt.Sprint("call ", i))
			}
			is(t, len(asked.all()), 3, "each sent once")
			// Without the UID the API would make a missing Lease, and it would
			// replace these versions unconditionally.
			noUID := testLease("7")
			noUID.UID = ""
			_, err := k.ReplaceLease(bg, noUID)
			is(t, err, error(Error{Kind: Invalid, Text: "a Lease replace needs the Lease UID"}), "no UID")
			for _, version := range []string{"", "0"} {
				_, err := k.ReplaceLease(bg, testLease(version))
				is(t, err, error(Error{Kind: Conflict}), "version "+version)
			}
			is(t, len(asked.all()), 3, "no replace was sent")
		}},
		{"a caller past its deadline is a timeout, dispatched or not", func(t *testing.T) {
			// The adapter's own deadline stays far ahead, so only the caller's
			// elapsed deadline ends these, even with its context Done never
			// closed. An already-expired caller dispatches nothing; one that
			// expires mid-call gives the same timeout whatever the reply was.
			whole := func() (*http.Response, error) {
				return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader(object("Lease", "l", "7", "u1")))}, nil
			}
			truncated := func() (*http.Response, error) {
				body := io.MultiReader(strings.NewReader(object("Lease", "l", "7", "u1")[:10]), errReader{})
				return &http.Response{StatusCode: 200, Body: io.NopCloser(body)}, nil
			}
			dropped := func() (*http.Response, error) { return nil, io.ErrUnexpectedEOF }
			for _, tc := range []struct {
				name string
				past bool
				resp func() (*http.Response, error)
				sent int32
			}{
				{"already past", true, whole, 0},
				{"expires during a whole reply", false, whole, 1},
				{"expires during a truncated reply", false, truncated, 1},
				{"expires during a transport error", false, dropped, 1},
			} {
				var sent atomic.Int32
				k := served(t, nil)
				k.callTimeout = 5 * time.Second
				k.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
					sent.Add(1)
					if !tc.past {
						time.Sleep(150 * time.Millisecond) // the caller's deadline passes here
					}
					return tc.resp()
				})
				at := time.Now().Add(50 * time.Millisecond)
				if tc.past {
					at = time.Now().Add(-time.Second)
				}
				_, err := k.GetLease(stuckDeadline{bg, at}, "l")
				is(t, [2]any{err, sent.Load()}, [2]any{error(context.DeadlineExceeded), tc.sent}, tc.name)
			}
		}},
		{"the transport makes one attempt per call", func(t *testing.T) {
			// A clone with keep-alives off and HTTP/1 only, leaving the base it
			// was cloned from untouched.
			base := &http.Transport{ForceAttemptHTTP2: true, TLSClientConfig: &tls.Config{NextProtos: []string{"h2"}}}
			t1 := oneShotTransport(base)
			http1Only := t1.Protocols != nil && t1.Protocols.HTTP1() && !t1.Protocols.HTTP2() && !t1.Protocols.UnencryptedHTTP2()
			is(t, [4]any{t1.DisableKeepAlives, http1Only, t1.TLSClientConfig.NextProtos, base.DisableKeepAlives},
				[4]any{true, true, []string{"http/1.1"}, false}, "hardened clone")
			// Keep-alives off means a new connection per call, so net/http has no
			// reused-but-dead connection to replay a GET on.
			var conns atomic.Int32
			srv := httptest.NewUnstartedServer(reply(200, object("Lease", "l", "7", "u1")))
			srv.Config.ConnState = func(_ net.Conn, s http.ConnState) {
				if s == http.StateNew {
					conns.Add(1)
				}
			}
			srv.Start()
			t.Cleanup(srv.Close)
			k, err := NewFor(&rest.Config{Host: srv.URL}, "ns")
			if err != nil {
				t.Fatal(err)
			}
			for range 2 {
				if _, err := k.GetLease(bg, "l"); err != nil {
					t.Fatal(err)
				}
			}
			is(t, conns.Load(), int32(2), "a new connection per call")
		}},
		{"a bad server URL or header value is refused before dispatch", func(t *testing.T) {
			for name, cfg := range map[string]rest.Config{
				"port out of range": {Host: "https://h:70000"},
				"port zero":         {Host: "https://h:0"},
				"query":             {Host: "https://h/?a=b"},
				"bare query":        {Host: "https://h/?"},
				"fragment":          {Host: "https://h/#f"},
				"transport wrapper": {Host: "https://h:1", WrapTransport: func(rt http.RoundTripper) http.RoundTripper { return rt }},
				"impersonate user":  {Host: "https://h:1", Impersonate: rest.ImpersonationConfig{UserName: "a\nb"}},
				"impersonate extra": {Host: "https://h:1", Impersonate: rest.ImpersonationConfig{Extra: map[string][]string{"k": {"a\nb"}}}},
				"user agent":        {Host: "https://h:1", UserAgent: "a\nb"},
			} {
				_, err := NewFor(&cfg, "ns")
				var e Error
				is(t, errors.As(err, &e) && e.Kind == Invalid && !strings.Contains(err.Error(), "a\nb"), true, name)
			}
		}},
		{"the read stops at the cap", func(t *testing.T) {
			// The body is far over the cap, but the client reads at most cap+1:
			// the size check never sees the rest.
			var read int
			k := served(t, nil)
			k.client.Transport = roundTrip(func(*http.Request) (*http.Response, error) {
				return &http.Response{StatusCode: 200, Body: io.NopCloser(&countingBody{left: 10 * k.maxReplySize, read: &read})}, nil
			})
			_, err := k.GetLease(bg, "l")
			is(t, [2]any{err, read <= k.maxReplySize+1}, [2]any{error(Error{Kind: Invalid, Text: "the reply is over 4096 bytes"}), true}, "capped read")
		}},
		{"a 2xx body that never ends is bounded", func(t *testing.T) {
			// Without the body-read deadline this blocks for ever; the bound
			// turns that into a named failure, not a hang. The explicit cancel
			// frees the read so cleanup does not block on the open connection.
			k := served(t, map[string]http.HandlerFunc{leasePath + "/l": endless(200, 0)})
			ctx, cancel := context.WithCancel(bg)
			defer cancel()
			done := make(chan error, 1)
			go func() { _, err := k.GetLease(ctx, "l"); done <- err }()
			select {
			case err := <-done:
				is(t, err, error(Error{Kind: Timeout, Secs: 1}), "timed out")
			case <-time.After(2 * time.Second):
				cancel()
				<-done
				t.Fatal("the call did not return within 2 s: the body read is not bounded")
			}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			tc.run(t)
		})
	}
}

// TestNew sets the environment, so it runs alone.
func TestNew(t *testing.T) {
	fifo := filepath.Join(t.TempDir(), "token")
	if err := syscall.Mkfifo(fifo, 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("KUBERNETES_SERVICE_HOST", "")
	for user, want := range map[string]error{
		"token: t": nil,
		// A read of the FIFO waits for a writer for ever.
		"tokenFile: " + fifo: unsupported,
		`token: "a\nb"`:      Error{Kind: Invalid, Text: "the bearer token is not a valid header value"},
		// An invalid impersonation value is caught on the New path too.
		`token: t, as: "a\nb"`: Error{Kind: Invalid, Text: "a request header value is not valid"},
	} {
		path := filepath.Join(t.TempDir(), "config")
		config := "{clusters: [{name: c, cluster: {server: 'https://127.0.0.1:1'}}], users: [{name: u, user: {" + user +
			"}}], contexts: [{name: c, context: {cluster: c, user: u}}], current-context: c}"
		if err := os.WriteFile(path, []byte(config), 0o600); err != nil {
			t.Fatal(err)
		}
		t.Setenv("KUBECONFIG", path)
		_, err := New("ns")
		is(t, err, want, user)
	}
}
