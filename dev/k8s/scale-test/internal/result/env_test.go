// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build envtest

// TestEnvResult proves the result claim, the compare-and-swap status write and result.json against a real API server
// and a mounted directory, from the runner Job (helm test), never a kubeconfig: config and namespace from in-cluster.
package result

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"net/http"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"go.uber.org/goleak"
	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

func TestMain(m *testing.M) {
	code := m.Run()
	http.DefaultTransport.(*http.Transport).CloseIdleConnections()
	if err := goleak.Find(goleak.IgnoreAnyFunction("k8s.io/klog/v2.(*flushDaemon).run.func1")); err != nil && code == 0 {
		fmt.Println(err)
		code = 1
	}
	os.Exit(code)
}

var env struct {
	once sync.Once
	cfg  *rest.Config
	ns   string
}

// upstream is the in-cluster client and the run's own namespace. A fixture fails, never skips, on missing inputs: a
// skipped Job exits 0 and reports a false pass.
func upstream(t *testing.T) (*cluster.Kube, string) {
	t.Helper()
	env.once.Do(func() {
		env.cfg, _ = rest.InClusterConfig()
		env.ns = os.Getenv("SCALE_TEST_NAMESPACE")
	})
	if env.cfg == nil || env.ns == "" {
		t.Fatal("run in the cluster with SCALE_TEST_NAMESPACE set")
	}
	k, err := cluster.NewFor(env.cfg, env.ns)
	if err != nil {
		t.Fatal(err)
	}
	return k, env.ns
}

// uniqueName is a fresh result ConfigMap name. It registers no cleanup: a name is removed by its UID once an object
// owns it, so stale cleanup never deletes a foreign replacement that reused the name.
func uniqueName() string {
	var b [5]byte
	_, _ = rand.Read(b[:])
	return "scale-test-result-" + hex.EncodeToString(b[:])
}

// removeByUID deletes name at the test's end, only while the stored object still has uid; cleanup never fails the test.
func removeByUID(t *testing.T, name, uid string) {
	t.Cleanup(func() { _ = deleteByUID(name, uid) })
}

// deleteByUID deletes name over an HTTP/1.1, no-keep-alive client (no pooled HTTP/2 read loop for goleak), under a UID
// precondition so it never removes a foreign replacement. It keeps the config's auth/TLS; a missing object (404) is clean.
func deleteByUID(name, uid string) error {
	cfg := *env.cfg
	cfg.WrapTransport = func(rt http.RoundTripper) http.RoundTripper {
		t, ok := rt.(*http.Transport)
		if !ok {
			return rt
		}
		t = t.Clone()
		t.DisableKeepAlives = true
		if t.TLSClientConfig != nil {
			t.TLSClientConfig.NextProtos = []string{"http/1.1"}
		}
		return t
	}
	client, err := rest.HTTPClientFor(&cfg)
	if err != nil {
		return err
	}
	base, _, err := rest.DefaultServerUrlFor(&cfg)
	if err != nil {
		return err
	}
	url := fmt.Sprintf("%s/api/v1/namespaces/%s/configmaps/%s", strings.TrimSuffix(base.String(), "/"), env.ns, name)
	body := strings.NewReader(fmt.Sprintf(`{"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":%q}}`, uid))
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, url, body)
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = resp.Body.Close() }()
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNotFound {
		return fmt.Errorf("delete %s: %d", name, resp.StatusCode)
	}
	return nil
}

func TestEnvResult(t *testing.T) {
	k, _ := upstream(t)
	ctx := context.Background()

	t.Run("claim is a compare-and-swap that one owner wins", func(t *testing.T) {
		name := uniqueName()
		made, err := k.CreateConfigMap(ctx, cluster.ConfigMapObject{Name: name})
		if err != nil {
			t.Fatalf("make the empty ConfigMap the chart would: %v", err)
		}
		removeByUID(t, name, made.UID)
		uid, err := Claim(ctx, k, name, "exec/first")
		if err != nil || uid == "" {
			t.Fatalf("first claim: uid=%q err=%v", uid, err)
		}
		if again, err := Claim(ctx, k, name, "exec/first"); err != nil || again != uid {
			t.Fatalf("re-claim by the owner must return the same UID: uid=%q err=%v", again, err)
		}
		switch _, err := Claim(ctx, k, name, "exec/second"); {
		case err == nil:
			t.Fatal("a second owner claimed a ConfigMap the first owns")
		case strings.Contains(err.Error(), "exec/"):
			t.Fatalf("the claim error leaked an owner: %v", err)
		}
	})

	t.Run("claim creates the ConfigMap when it is missing", func(t *testing.T) {
		name := uniqueName()
		uid, err := Claim(ctx, k, name, "exec/only")
		if err != nil || uid == "" {
			t.Fatalf("claim of a missing ConfigMap: uid=%q err=%v", uid, err)
		}
		removeByUID(t, name, uid)
		back, err := k.GetConfigMap(ctx, name)
		if err != nil || back == nil || back.UID != uid {
			t.Fatalf("the created ConfigMap is not the claimed one: %+v err=%v", back, err)
		}
	})

	t.Run("the status write refuses a replaced ConfigMap by its UID alone", func(t *testing.T) {
		name := uniqueName()
		uid, err := Claim(ctx, k, name, "exec/owner")
		if err != nil {
			t.Fatalf("claim: %v", err)
		}
		removeByUID(t, name, uid)
		const status = `{"schemaVersion":1,"outcome":"passed"}`
		if err := WriteResult(ctx, k, name, "exec/owner", uid, status); err != nil {
			t.Fatalf("write the status: %v", err)
		}
		back, err := k.GetConfigMap(ctx, name)
		if err != nil || back == nil || back.Data[ResultKey] != status {
			t.Fatalf("the status was not stored under %s: %+v err=%v", ResultKey, back, err)
		}
		// Another attempt replaces the ConfigMap, keeping the same owner but a new UID, so the stale writer is refused
		// by the UID check alone (the owner still matches), never writing over the new owner's object.
		if err := deleteByUID(name, uid); err != nil {
			t.Fatalf("remove the first object: %v", err)
		}
		remade, err := k.CreateConfigMap(ctx, cluster.ConfigMapObject{Name: name, Annotations: map[string]string{OwnerAnnotation: "exec/owner"}})
		if err != nil {
			t.Fatalf("remake the ConfigMap: %v", err)
		}
		removeByUID(t, name, remade.UID)
		if remade.UID == uid {
			t.Fatal("the remade ConfigMap reused the UID, so the UID check is not isolated")
		}
		if err := WriteResult(ctx, k, name, "exec/owner", uid, status); err == nil {
			t.Fatal("a stale UID wrote over a replaced ConfigMap")
		}
		// A delete with the stale UID must be refused with 409, so the UID precondition — not plain cleanup order —
		// is what protects the replacement. Without it the delete would remove the new owner's object.
		if err := deleteByUID(name, uid); err == nil || !strings.HasSuffix(err.Error(), ": 409") {
			t.Fatalf("a stale-UID delete must be refused with 409, got %v", err)
		}
		back, err = k.GetConfigMap(ctx, name)
		if err != nil || back == nil || back.UID != remade.UID || back.Data[ResultKey] != "" {
			t.Fatalf("the replacement did not survive stale cleanup: %+v err=%v", back, err)
		}
	})

	t.Run("the authoritative result.json round-trips on the mounted directory", func(t *testing.T) {
		dir := os.Getenv("SCALE_TEST_RESULT_DIR")
		if dir == "" {
			t.Fatal("SCALE_TEST_RESULT_DIR names the mounted result directory")
		}
		run := "run-" + strings.TrimPrefix(uniqueName(), "scale-test-result-")
		// Reasons, Stages and Invariants are non-nil: the strict codec refuses a nil required slice, as a real result never has.
		want := RunResult{
			SchemaVersion: SchemaVersion, Test: "fleet-up", RunID: run, Outcome: OutcomeOf(Passed),
			Reasons: []string{}, Stages: []StageResult{}, Invariants: []InvariantResult{},
			Journal: NoneWritten(),
		}
		file := ResultFor(dir, run)
		if err := file.Write(&want); err != nil {
			t.Fatalf("write result.json: %v", err)
		}
		t.Cleanup(func() { _ = os.Remove(file.Path) })
		data, err := os.ReadFile(file.Path)
		if err != nil {
			t.Fatalf("read result.json back: %v", err)
		}
		var got RunResult
		if err := jsonx.Decode(data, &got); err != nil {
			t.Fatalf("decode result.json: %v", err)
		}
		if got.RunID != run || got.Verdict != want.Verdict || got.ExitCode != want.ExitCode {
			t.Fatalf("result.json did not round-trip: got %+v", got)
		}
	})
}
