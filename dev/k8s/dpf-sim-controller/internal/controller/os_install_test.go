// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
)

const (
	testBFBName = "bf3-bfb"
	testBFBURL  = "http://bfb.example/bf-bundle.bfb"
	testTaskURI = "/redfish/v1/TaskService/Tasks/7"
	testBMCPass = "s3cret"
)

// fakeBMC is a DPU BMC serving the two Redfish calls the simulator makes.
type fakeBMC struct {
	srv *httptest.Server

	mu                 sync.Mutex
	dropUpdateResponse bool
	updateCode         int // SimpleUpdate response status
	updateBody         string
	taskLocation       string
	taskCode           int    // task GET response status
	taskState          string // task GET TaskState
	taskMessage        string
	taskBody           string
	posts              []map[string]any
	gets               int
	user, pass         string // credentials seen on the last request
}

func newFakeBMC(t *testing.T) *fakeBMC {
	t.Helper()
	b := &fakeBMC{updateCode: http.StatusAccepted, updateBody: `{"Id":"7"}`, taskLocation: testTaskURI, taskCode: http.StatusOK, taskState: "Running"}
	b.srv = httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		b.mu.Lock()
		defer b.mu.Unlock()
		b.user, b.pass, _ = req.BasicAuth()
		switch {
		case req.Method == http.MethodPost && req.URL.Path == "/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate":
			var body map[string]any
			data, _ := io.ReadAll(req.Body)
			_ = json.Unmarshal(data, &body)
			b.posts = append(b.posts, body)
			if b.dropUpdateResponse {
				conn, _, err := w.(http.Hijacker).Hijack()
				if err == nil {
					_ = conn.Close()
				}
				return
			}
			if b.updateCode == http.StatusAccepted && b.taskLocation != "" {
				w.Header().Set("Location", b.taskLocation)
			}
			w.WriteHeader(b.updateCode)
			_, _ = w.Write([]byte(b.updateBody))
		case req.Method == http.MethodGet && req.URL.Path == testTaskURI:
			b.gets++
			w.WriteHeader(b.taskCode)
			if b.taskBody != "" {
				_, _ = w.Write([]byte(b.taskBody))
			} else if b.taskCode == http.StatusNotFound {
				_, _ = w.Write([]byte(`{"error":{"code":"Base.ResourceMissingAtURI","message":"task not found"}}`))
			} else if b.taskCode == http.StatusOK {
				_ = json.NewEncoder(w).Encode(map[string]any{
					"TaskState": b.taskState,
					"Messages":  []map[string]any{{"Message": b.taskMessage}},
				})
			}
		default:
			http.NotFound(w, req)
		}
	}))
	t.Cleanup(b.srv.Close)
	return b
}

func (b *fakeBMC) set(f func(*fakeBMC)) {
	b.mu.Lock()
	defer b.mu.Unlock()
	f(b)
}

// address returns the IP and port the DPUDevice should carry for this BMC.
func (b *fakeBMC) address(t *testing.T) (string, uint32) {
	t.Helper()
	host, port, err := net.SplitHostPort(b.srv.Listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	p, err := strconv.ParseUint(port, 10, 32)
	if err != nil {
		t.Fatal(err)
	}
	return host, uint32(p)
}

// installFixture is a DPU in OS Installing whose DPUDevice points at bmc.
type installFixture struct {
	r      *DPUDeviceReconciler
	bmc    *fakeBMC
	device *provisioningv1.DPUDevice
	dpuKey types.NamespacedName
}

// newInstallFixture builds a reconciler with the Redfish install enabled. The
// DPU entered OS Installing `age` ago.
func newInstallFixture(t *testing.T, age time.Duration, timeout time.Duration) *installFixture {
	t.Helper()
	bmc := newFakeBMC(t)
	ip, port := bmc.address(t)

	device := newDevice()
	device.Spec.BMCIP, device.Spec.BMCPort = &ip, &port
	nodeID := "n1"
	node := newNode(carbide.DPUNodeName(nodeID), nil)
	node.Spec.DPUs = []provisioningv1.DPURef{{Name: device.Name}}
	dpu := &provisioningv1.DPU{
		ObjectMeta: metav1.ObjectMeta{
			Namespace: testNamespace,
			Name:      carbide.DPUName(nodeID, carbide.DeviceIDFromDeviceCRName(device.Name)),
			Annotations: map[string]string{
				carbide.AnnSimPhaseEnteredAt: time.Now().Add(-age).UTC().Format(time.RFC3339),
			},
		},
		Spec: provisioningv1.DPUSpec{
			DPUNodeName: node.Name, DPUDeviceName: device.Name, DPUFlavor: "f", BFB: testBFBName,
		},
		Status: provisioningv1.DPUStatus{Phase: provisioningv1.DPUOSInstalling},
	}
	bfb := &provisioningv1.BFB{
		ObjectMeta: metav1.ObjectMeta{Namespace: testNamespace, Name: testBFBName},
		Spec:       provisioningv1.BFBSpec{URL: testBFBURL},
	}
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{Namespace: testNamespace, Name: DefaultBMCSecretName},
		Data:       map[string][]byte{"password": []byte(testBMCPass)},
	}
	r := newReconciler(t, device, node, dpu, bfb, secret)
	r.SecretReader = r.Client
	r.osInstaller = newOSInstaller(OSInstallConfig{
		Timeout: timeout, PollInterval: time.Second, RequestTimeout: 5 * time.Second,
	}, r.SecretReader, testNamespace)
	return &installFixture{
		r: r, bmc: bmc, device: device,
		dpuKey: types.NamespacedName{Namespace: testNamespace, Name: dpu.Name},
	}
}

func (f *installFixture) dpu(t *testing.T) *provisioningv1.DPU {
	t.Helper()
	var dpu provisioningv1.DPU
	if err := f.r.Get(context.Background(), f.dpuKey, &dpu); err != nil {
		t.Fatal(err)
	}
	return &dpu
}

// step runs one osInstallStep against the stored DPU.
func (f *installFixture) step(t *testing.T) (bool, ctrl.Result) {
	t.Helper()
	advance, res, err := f.r.osInstallStep(context.Background(), f.device, f.dpu(t))
	if err != nil {
		t.Fatalf("osInstallStep: %v", err)
	}
	return advance, res
}

func (f *installFixture) requireError(t *testing.T, wantMessage string) {
	t.Helper()
	dpu := f.dpu(t)
	if dpu.Status.Phase != provisioningv1.DPUError {
		t.Fatalf("phase = %q, want %q", dpu.Status.Phase, provisioningv1.DPUError)
	}
	cond := meta.FindStatusCondition(dpu.Status.Conditions, string(provisioningv1.DPUCondOSInstalled))
	if cond == nil || cond.Status != metav1.ConditionFalse {
		t.Fatalf("OSInstalled condition = %+v, want False", cond)
	}
	if wantMessage != "" && !strings.Contains(cond.Message, wantMessage) {
		t.Fatalf("condition message %q does not contain %q", cond.Message, wantMessage)
	}
}

func TestOSInstallSuccess(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)

	// First step submits the install and waits for it.
	advance, res := f.step(t)
	if advance || res.RequeueAfter == 0 {
		t.Fatalf("after submit: advance=%v requeue=%v, want wait", advance, res.RequeueAfter)
	}
	if len(f.bmc.posts) != 1 {
		t.Fatalf("SimpleUpdate posts = %d, want 1", len(f.bmc.posts))
	}
	post := f.bmc.posts[0]
	if post["ImageURI"] != testBFBURL || post["TransferProtocol"] != "HTTP" {
		t.Errorf("SimpleUpdate body = %v, want ImageURI %q over HTTP", post, testBFBURL)
	}
	targets, _ := post["Targets"].([]any)
	if len(targets) != 1 || targets[0] != "redfish/v1/UpdateService/FirmwareInventory/DPU_OS" {
		t.Errorf("Targets = %v, want the DPU_OS inventory entry", targets)
	}
	if f.bmc.user != DefaultBMCUsername || f.bmc.pass != testBMCPass {
		t.Errorf("credentials = %q/%q, want %q/%q", f.bmc.user, f.bmc.pass, DefaultBMCUsername, testBMCPass)
	}
	if got := f.dpu(t).Annotations[carbide.AnnSimBFBInstallTask]; got != testTaskURI {
		t.Fatalf("task annotation = %q, want %q", got, testTaskURI)
	}

	// While the task runs the phase holds and no second install is submitted.
	advance, res = f.step(t)
	if advance || res.RequeueAfter == 0 {
		t.Fatalf("task running: advance=%v requeue=%v, want wait", advance, res.RequeueAfter)
	}
	if len(f.bmc.posts) != 1 {
		t.Fatalf("SimpleUpdate posts = %d while task runs, want 1", len(f.bmc.posts))
	}

	f.bmc.set(func(b *fakeBMC) { b.taskState = "Completed" })
	if advance, _ = f.step(t); !advance {
		t.Fatal("task Completed: want advance")
	}
	if got := f.dpu(t).Status.Phase; got != provisioningv1.DPUOSInstalling {
		t.Fatalf("step itself changed phase to %q", got)
	}
}

func TestOSInstallTaskException(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.step(t)
	f.bmc.set(func(b *fakeBMC) { b.taskState, b.taskMessage = "Exception", "BFB write failed" })

	if advance, _ := f.step(t); advance {
		t.Fatal("task Exception: must not advance")
	}
	f.requireError(t, "BFB write failed")
}

func TestOSInstallTimeout(t *testing.T) {
	f := newInstallFixture(t, 2*time.Hour, time.Hour)

	if advance, _ := f.step(t); advance {
		t.Fatal("timed out: must not advance")
	}
	f.requireError(t, "timeout")
	if len(f.bmc.posts) != 0 {
		t.Fatalf("SimpleUpdate posts = %d after timeout, want none", len(f.bmc.posts))
	}
}

func TestOSInstallTimeoutWhileTaskRunning(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.step(t)
	// The same DPU, an hour later, with the task still Running.
	dpu := f.dpu(t)
	dpu.Annotations[carbide.AnnSimPhaseEnteredAt] = time.Now().Add(-2 * time.Hour).UTC().Format(time.RFC3339)
	if err := f.r.Update(context.Background(), dpu); err != nil {
		t.Fatal(err)
	}

	f.step(t)
	f.requireError(t, "timeout")
}

func TestReconcileOSInstallTaskNotFoundFailsFast(t *testing.T) {
	f := newInstallFixture(t, time.Second, 45*time.Minute)
	f.reconcileOnce(t)
	f.bmc.set(func(b *fakeBMC) { b.taskCode = http.StatusNotFound })

	if res := f.reconcileOnce(t); res != (ctrl.Result{}) {
		t.Fatalf("404 task: result=%+v, want no retry", res)
	}
	f.requireError(t, "lost")
	dpu := f.dpu(t)
	cond := meta.FindStatusCondition(dpu.Status.Conditions, string(provisioningv1.DPUCondOSInstalled))
	if cond.Reason != "TaskLost" || !strings.Contains(cond.Message, testTaskURI) {
		t.Fatalf("lost task condition = %+v, want TaskLost naming %q", cond, testTaskURI)
	}
	if got := dpu.Annotations[carbide.AnnSimBFBInstallTask]; got != testTaskURI {
		t.Fatalf("lost task annotation = %q, want %q retained for diagnosis", got, testTaskURI)
	}

	// Error is terminal: another reconcile cannot poll or resubmit the lost task.
	if res := f.reconcileOnce(t); res != (ctrl.Result{}) {
		t.Fatalf("terminal Error: result=%+v, want no retry", res)
	}
	f.requireError(t, "lost")
	if len(f.bmc.posts) != 1 || f.bmc.gets != 1 {
		t.Fatalf("lost task: %d SimpleUpdate posts and %d polls, want 1 each", len(f.bmc.posts), f.bmc.gets)
	}
}

func TestOSInstallTaskPollFailureRetries(t *testing.T) {
	for _, tc := range []struct {
		name string
		code int
		body string
	}{
		{name: "service unavailable", code: http.StatusServiceUnavailable},
		{name: "MAT route gap", code: http.StatusNotFound, body: "no router configured for forwarded_host/host/authority"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newInstallFixture(t, time.Second, time.Hour)
			f.step(t)
			f.bmc.set(func(b *fakeBMC) { b.taskCode, b.taskBody = tc.code, tc.body })

			advance, res := f.step(t)
			if advance || res.RequeueAfter != f.r.osInstaller.PollInterval {
				t.Fatalf("%d task: advance=%v requeue=%v, want poll interval retry", tc.code, advance, res.RequeueAfter)
			}
			if got := f.dpu(t).Status.Phase; got != provisioningv1.DPUOSInstalling {
				t.Fatalf("transient poll failure moved phase to %q, want OS Installing", got)
			}

			f.bmc.set(func(b *fakeBMC) { b.taskCode, b.taskState, b.taskBody = http.StatusOK, "Completed", "" })
			if advance, _ := f.step(t); !advance {
				t.Fatal("task poll recovered: want advance")
			}
			if len(f.bmc.posts) != 1 || f.bmc.gets != 2 {
				t.Fatalf("transient poll failure: %d SimpleUpdate posts and %d polls, want 1 post and 2 polls", len(f.bmc.posts), f.bmc.gets)
			}
		})
	}
}

func TestOSInstallUpdateRejected(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.bmc.set(func(b *fakeBMC) { b.updateCode = http.StatusBadRequest })

	if advance, _ := f.step(t); advance {
		t.Fatal("rejected SimpleUpdate: must not advance")
	}
	f.requireError(t, "400")
}

func TestOSInstallAmbiguousSubmissionDoesNotRetry(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.bmc.srv.Close()
	advance, _ := f.step(t)
	if advance {
		t.Fatal("unknown submission must not advance")
	}
	f.requireError(t, "submission outcome is unknown")
	if _, ok := f.dpu(t).Annotations[carbide.AnnSimBFBInstallStarted]; !ok {
		t.Fatal("submission marker was lost")
	}
}

func TestOSInstallWithoutBFBSkips(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	dpu := f.dpu(t)
	dpu.Spec.BFB, dpu.Spec.BlueFieldSoftware = "", "bfs"

	advance, _, err := f.r.osInstallStep(context.Background(), f.device, dpu)
	if err != nil || !advance {
		t.Fatalf("BlueFieldSoftware DPU: advance=%v err=%v, want advance without a BMC call", advance, err)
	}
	if len(f.bmc.posts) != 0 {
		t.Fatal("a BlueFieldSoftware DPU must not get a BFB install")
	}
}

func TestOSInstallMissingSecretRetries(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.r.osInstaller.SecretName = "absent"

	advance, res := f.step(t)
	if advance || res.RequeueAfter == 0 {
		t.Fatalf("no credentials: advance=%v requeue=%v, want retry", advance, res.RequeueAfter)
	}
	if len(f.bmc.posts) != 0 {
		t.Fatal("must not call the BMC without credentials")
	}
}

// reconcileOnce drives the full Reconcile for the fixture's DPUDevice.
func (f *installFixture) reconcileOnce(t *testing.T) ctrl.Result {
	t.Helper()
	res, err := f.r.Reconcile(context.Background(), ctrl.Request{
		NamespacedName: types.NamespacedName{Namespace: testNamespace, Name: f.device.Name},
	})
	if err != nil {
		t.Fatalf("Reconcile: %v", err)
	}
	return res
}

// TestReconcileOSInstallAdvancesOnlyAfterTask walks the real Reconcile: the
// phase holds while the install runs and moves to Rebooting when it completes,
// clearing the task annotation for the next phase.
func TestReconcileOSInstallAdvancesOnlyAfterTask(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.r.PhaseDwell = time.Millisecond

	f.reconcileOnce(t)
	f.reconcileOnce(t)
	if got := f.dpu(t).Status.Phase; got != provisioningv1.DPUOSInstalling {
		t.Fatalf("phase = %q while install runs, want %q", got, provisioningv1.DPUOSInstalling)
	}

	f.bmc.set(func(b *fakeBMC) { b.taskState = "Completed" })
	f.reconcileOnce(t)
	dpu := f.dpu(t)
	if dpu.Status.Phase != provisioningv1.DPURebooting {
		t.Fatalf("phase = %q after install, want %q", dpu.Status.Phase, provisioningv1.DPURebooting)
	}
	if _, ok := dpu.Annotations[carbide.AnnSimBFBInstallTask]; ok {
		t.Error("task annotation survived leaving OS Installing")
	}
	if _, ok := dpu.Annotations[carbide.AnnSimBFBInstallStarted]; ok {
		t.Error("submission marker survived leaving OS Installing")
	}
	if len(f.bmc.posts) != 1 {
		t.Errorf("SimpleUpdate posts = %d, want exactly 1", len(f.bmc.posts))
	}
}

// TestReconcileOSInstallDisabledNeverCallsBMC pins the default: without the
// Redfish install the phase only dwells, and no BMC is contacted.
func TestReconcileOSInstallDisabledNeverCallsBMC(t *testing.T) {
	f := newInstallFixture(t, time.Hour, time.Hour)
	f.r.osInstaller = nil
	f.r.PhaseDwell = time.Millisecond

	f.reconcileOnce(t)
	if got := f.dpu(t).Status.Phase; got != provisioningv1.DPURebooting {
		t.Fatalf("phase = %q, want %q: dwell-only mode advances OS Installing", got, provisioningv1.DPURebooting)
	}
	if len(f.bmc.posts) != 0 || f.bmc.gets != 0 {
		t.Fatalf("BMC saw %d posts and %d gets with the install disabled", len(f.bmc.posts), f.bmc.gets)
	}
}

func TestBMCBaseURL(t *testing.T) {
	ip, port, other := "10.1.2.3", uint32(8443), "10.9.9.9"
	ipv6, bracketed := "2001:db8::5", "[2001:db8::5]"
	httpsPort := uint32(443)
	for _, tc := range []struct {
		name    string
		spec    provisioningv1.DPUDeviceSpec
		status  provisioningv1.DPUDeviceStatus
		want    string
		wantErr bool
	}{
		{name: "spec with port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ip, BMCPort: &port}, want: "https://10.1.2.3:8443"},
		{name: "default port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ip}, want: "https://10.1.2.3"},
		{name: "explicit HTTPS port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ip, BMCPort: &httpsPort}, want: "https://10.1.2.3"},
		{name: "IPv6 default port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ipv6}, want: "https://[2001:db8::5]"},
		{name: "IPv6 with port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ipv6, BMCPort: &port}, want: "https://[2001:db8::5]:8443"},
		{name: "bracketed IPv6 with port", spec: provisioningv1.DPUDeviceSpec{BMCIP: &bracketed, BMCPort: &port}, want: "https://[2001:db8::5]:8443"},
		{name: "status wins", spec: provisioningv1.DPUDeviceSpec{BMCIP: &ip, BMCPort: &port}, status: provisioningv1.DPUDeviceStatus{BMCIP: &other, BMCPort: &httpsPort}, want: "https://10.9.9.9"},
		{name: "no address", wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := bmcBaseURL(&provisioningv1.DPUDevice{Spec: tc.spec, Status: tc.status})
			if (err != nil) != tc.wantErr || got != tc.want {
				t.Fatalf("bmcBaseURL = %q, %v; want %q (err=%v)", got, err, tc.want, tc.wantErr)
			}
		})
	}
}

// A failed patch can mean no write, or a committed write whose response was lost.
type failInstallPatch struct {
	client.Client
	key       string
	commit    bool
	remaining int
}

func (c *failInstallPatch) Patch(ctx context.Context, obj client.Object, patch client.Patch, opts ...client.PatchOption) error {
	if obj.GetAnnotations()[c.key] != "" && c.remaining > 0 {
		c.remaining--
		if c.commit {
			if err := c.Client.Patch(ctx, obj, patch, opts...); err != nil {
				return err
			}
		}
		return errors.New("injected annotation write failure")
	}
	return c.Client.Patch(ctx, obj, patch, opts...)
}

func TestOSInstallTaskPersistenceFailureNeverResubmits(t *testing.T) {
	for _, committed := range []bool{false, true} {
		t.Run(strconv.FormatBool(committed), func(t *testing.T) {
			f := newInstallFixture(t, time.Second, time.Hour)
			api := f.r.Client
			f.r.DPUReader = api
			f.r.Client = &failInstallPatch{Client: api, key: carbide.AnnSimBFBInstallTask, commit: committed, remaining: 1}
			if _, _, err := f.r.osInstallStep(context.Background(), f.device, f.dpu(t)); err == nil {
				t.Fatal("task annotation failure must surface")
			}
			cached := f.dpu(t)
			if _, ok := cached.Annotations[carbide.AnnSimBFBInstallStarted]; !ok {
				t.Fatal("marker must be durable before POST")
			}
			// Model a restart and an informer that has not observed a saved task.
			delete(cached.Annotations, carbide.AnnSimBFBInstallTask)
			f.r.osInstaller = newOSInstaller(f.r.osInstaller.OSInstallConfig, f.r.SecretReader, testNamespace)
			f.bmc.set(func(b *fakeBMC) { b.taskState = "Completed" })
			advance, _, err := f.r.osInstallStep(context.Background(), f.device, cached)
			if err != nil {
				t.Fatal(err)
			}
			if advance != committed {
				t.Fatalf("advance=%v, task save committed=%v", advance, committed)
			}
			if !committed {
				f.requireError(t, "durable marker has no task path")
			}
			f.bmc.mu.Lock()
			defer f.bmc.mu.Unlock()
			if len(f.bmc.posts) != 1 {
				t.Fatalf("POSTs=%d, want exactly one", len(f.bmc.posts))
			}
		})
	}
}

func TestOSInstallMarkerWithoutTaskFailsClosed(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	dpu := f.dpu(t)
	if err := f.r.setInstallAnnotation(context.Background(), dpu, carbide.AnnSimBFBInstallStarted, "true"); err != nil {
		t.Fatal(err)
	}
	if advance, _ := f.step(t); advance {
		t.Fatal("marker without task must not advance")
	}
	f.requireError(t, "durable marker has no task path")
	if len(f.bmc.posts) != 0 {
		t.Fatal("marker must prevent a new POST")
	}
}

func TestOSInstallMarkerWriteFailurePreventsPost(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.r.Client = &failInstallPatch{Client: f.r.Client, key: carbide.AnnSimBFBInstallStarted, remaining: 1}
	if _, _, err := f.r.osInstallStep(context.Background(), f.device, f.dpu(t)); err == nil {
		t.Fatal("claim failure must surface")
	}
	if len(f.bmc.posts) != 0 {
		t.Fatal("POST preceded durable claim")
	}
}

func TestOSInstallStaleClaimCannotSubmitAgain(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	stale := f.dpu(t)
	f.step(t)
	if _, _, err := f.r.osInstallStep(context.Background(), f.device, stale); !apierrors.IsConflict(err) {
		t.Fatalf("stale claim: %v, want conflict", err)
	}
	if len(f.bmc.posts) != 1 {
		t.Fatal("stale claimant sent a second POST")
	}
}

func TestOSInstallRecoveryDoesNotChangeReplacementOrAdvancedDPU(t *testing.T) {
	for _, changeUID := range []bool{false, true} {
		t.Run(strconv.FormatBool(changeUID), func(t *testing.T) {
			f := newInstallFixture(t, time.Second, time.Hour)
			dpu := f.dpu(t)
			if err := f.r.setInstallAnnotation(context.Background(), dpu, carbide.AnnSimBFBInstallStarted, "true"); err != nil {
				t.Fatal(err)
			}
			stale := dpu.DeepCopy()
			fresh := f.dpu(t)
			if changeUID {
				stale.UID = "old-dpu"
			} else {
				fresh.Status.Phase = provisioningv1.DPURebooting
				if err := f.r.Status().Update(context.Background(), fresh); err != nil {
					t.Fatal(err)
				}
			}
			f.r.DPUReader = f.r.Client
			advance, _, err := f.r.osInstallStep(context.Background(), f.device, stale)
			if advance || err != nil {
				t.Fatalf("advance=%v err=%v", advance, err)
			}
			if got := f.dpu(t); got.Status.Phase != fresh.Status.Phase {
				t.Fatalf("changed DPU phase from %q to %q", fresh.Status.Phase, got.Status.Phase)
			}
			if len(f.bmc.posts) != 0 {
				t.Fatal("stale recovery posted an install")
			}
		})
	}
}

func TestOSInstallDelayedTaskSaveCannotOverwriteNewVersion(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	dpu := f.dpu(t)
	if err := f.r.setInstallAnnotation(context.Background(), dpu, carbide.AnnSimBFBInstallStarted, "true"); err != nil {
		t.Fatal(err)
	}
	stale := dpu.DeepCopy()
	fresh := f.dpu(t)
	fresh.Status.Phase = provisioningv1.DPURebooting
	if err := f.r.Status().Update(context.Background(), fresh); err != nil {
		t.Fatal(err)
	}
	if err := f.r.setInstallAnnotation(context.Background(), stale, carbide.AnnSimBFBInstallTask, testTaskURI); !apierrors.IsConflict(err) {
		t.Fatalf("delayed task save: %v, want conflict", err)
	}
	if f.dpu(t).Annotations[carbide.AnnSimBFBInstallTask] != "" {
		t.Fatal("old task was attached to a changed DPU")
	}
}

func TestOSInstallAcceptedRequestWithLostResponseNeverResubmits(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.bmc.set(func(b *fakeBMC) { b.dropUpdateResponse = true })
	if advance, _ := f.step(t); advance {
		t.Fatal("unknown accepted task must not advance")
	}
	f.requireError(t, "submission outcome is unknown")
	// A later observation must retain Error and the durable marker, not POST.
	f.step(t)
	f.bmc.mu.Lock()
	defer f.bmc.mu.Unlock()
	if len(f.bmc.posts) != 1 {
		t.Fatalf("POSTs=%d, want one", len(f.bmc.posts))
	}
}

func TestOSInstallAcceptedWithoutTaskNeverResubmits(t *testing.T) {
	f := newInstallFixture(t, time.Second, time.Hour)
	f.bmc.set(func(b *fakeBMC) {
		b.taskLocation = ""
		b.updateBody = `{}`
	})
	if advance, _ := f.step(t); advance {
		t.Fatal("accepted response without a task must not advance")
	}
	f.requireError(t, "submission outcome is unknown")
	if f.dpu(t).Annotations[carbide.AnnSimBFBInstallTask] != "" {
		t.Fatal("response without a task created a task annotation")
	}
	if _, ok := f.dpu(t).Annotations[carbide.AnnSimBFBInstallStarted]; !ok {
		t.Fatal("submission marker must remain durable")
	}
	f.step(t)
	f.bmc.mu.Lock()
	defer f.bmc.mu.Unlock()
	if len(f.bmc.posts) != 1 {
		t.Fatalf("POSTs=%d, want exactly one", len(f.bmc.posts))
	}
}
