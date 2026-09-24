// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Command dpf-sim-controller runs a development-only simulator of the DPF
// operator so machine-a-tron simulated hosts can progress past dpuinit.
// See the package README for the NICo <-> DPF contract it reproduces.
package main

import (
	"flag"
	"fmt"
	"os"
	"time"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
	"k8s.io/apimachinery/pkg/runtime"
	clientgoscheme "k8s.io/client-go/kubernetes/scheme"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/cache"
	"sigs.k8s.io/controller-runtime/pkg/config"
	"sigs.k8s.io/controller-runtime/pkg/healthz"
	"sigs.k8s.io/controller-runtime/pkg/log/zap"
	metricsserver "sigs.k8s.io/controller-runtime/pkg/metrics/server"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/controller"
)

var scheme = runtime.NewScheme()

func init() {
	_ = clientgoscheme.AddToScheme(scheme)
	// Register the upstream DPF types — this is the whole point of importing
	// the doca-platform module: no local CRD/type authoring.
	_ = provisioningv1.AddToScheme(scheme)
}

func main() {
	var (
		metricsAddr  string
		probeAddr    string
		dpfNamespace string
		phaseDwell   time.Duration
		osInstall    time.Duration
	)
	flag.StringVar(&metricsAddr, "metrics-bind-address", ":8080", "metrics endpoint")
	flag.StringVar(&probeAddr, "health-probe-bind-address", ":8081", "health probe endpoint")
	flag.StringVar(&dpfNamespace, "dpf-namespace", "dpf-operator-system",
		"namespace where NICo creates DPF CRs (must match site config)")
	var cacheSyncTimeout time.Duration
	flag.DurationVar(&cacheSyncTimeout, "cache-sync-timeout", 15*time.Minute,
		"how long to wait for the initial informer cache sync; the 2m default is\n"+
			"too short once the namespace holds thousands of DPU/DPUDevice CRs")
	flag.DurationVar(&phaseDwell, "phase-dwell", 3*time.Second,
		"time each dwell-gated DPU phase lingers before advancing")
	flag.DurationVar(&osInstall, "os-install-dwell", 0,
		"time a DPU lingers in OS Installing; 0 (default) means the same as --phase-dwell.\n"+
			"A real BFB install takes minutes; set this to hold NICo in its DPF wait state that long")
	var reconcileConcurrency int
	flag.IntVar(&reconcileConcurrency, "reconcile-concurrency", 16,
		"parallel reconcile workers (fleet-scale walks serialize on one)")
	opts := zap.Options{Development: true}
	opts.BindFlags(flag.CommandLine)
	flag.Parse()

	// A negative dwell collapses every RequeueAfter/dwell gate into an
	// immediate hot-loop retry; zero (advance as fast as reconciles land) is
	// still a valid dev setting.
	if phaseDwell < 0 {
		fmt.Fprintf(os.Stderr, "invalid --phase-dwell %v: must be >= 0\n", phaseDwell)
		os.Exit(2)
	}
	if osInstall < 0 {
		fmt.Fprintf(os.Stderr, "invalid --os-install-dwell %v: must be >= 0\n", osInstall)
		os.Exit(2)
	}

	// controller-runtime only honors a manager-level CacheSyncTimeout that is
	// positive; zero or negative silently falls back to the per-controller
	// 2-minute default -- the very value this flag exists to raise past. Fail
	// loudly instead of letting "0 = no timeout" intuition reintroduce the
	// fleet-scale crash-loop.
	if cacheSyncTimeout <= 0 {
		fmt.Fprintf(os.Stderr, "invalid --cache-sync-timeout %v: must be > 0\n", cacheSyncTimeout)
		os.Exit(2)
	}

	// Raise with --cache-sync-timeout when simulating very large fleets.
	ctrl.SetLogger(zap.New(zap.UseFlagOptions(&opts)))
	setupLog := ctrl.Log.WithName("setup")

	mgr, err := ctrl.NewManager(ctrl.GetConfigOrDie(), ctrl.Options{
		Scheme:                 scheme,
		Metrics:                metricsserver.Options{BindAddress: metricsAddr},
		HealthProbeBindAddress: probeAddr,
		// Scope the cache to the DPF namespace: the reconciler only resolves
		// and creates CRs there, and a cluster-wide watch would pick up
		// DPUDevices from an unrelated namespace (e.g. a real DPF install
		// elsewhere on the cluster) and churn on them forever.
		Cache: cache.Options{
			DefaultNamespaces: map[string]cache.Config{dpfNamespace: {}},
		},
		// Simulator is single-instance dev tooling; no leader election.
		//
		// Fleet-scale caches need far longer than the 2-minute default to do
		// their initial LIST: at 4,500 hosts the DPF namespace holds ~9,000
		// DPUs and ~9,000 DPUDevices, the sync times out, and the manager
		// exits before reconciling anything ("failed to wait for dpudevice
		// caches to sync"). The pod then crash-loops and every machine sits
		// at dpfstate=waitingforready forever.
		Controller: config.Controller{
			CacheSyncTimeout: cacheSyncTimeout,
		},
	})
	if err != nil {
		setupLog.Error(err, "unable to start manager")
		os.Exit(1)
	}

	if err = (&controller.DPUDeviceReconciler{
		Client:         mgr.GetClient(),
		Scheme:         mgr.GetScheme(),
		Namespace:      dpfNamespace,
		PhaseDwell:     phaseDwell,
		OSInstallDwell: osInstall,
		Concurrency:    reconcileConcurrency,
	}).SetupWithManager(mgr); err != nil {
		setupLog.Error(err, "unable to create controller", "controller", "DPUDevice")
		os.Exit(1)
	}

	_ = mgr.AddHealthzCheck("healthz", healthz.Ping)
	_ = mgr.AddReadyzCheck("readyz", healthz.Ping)

	setupLog.Info("starting dpf-sim-controller", "dpfNamespace", dpfNamespace, "phaseDwell", phaseDwell, "osInstallDwell", osInstall)
	if err := mgr.Start(ctrl.SetupSignalHandler()); err != nil {
		setupLog.Error(err, "manager exited")
		os.Exit(1)
	}
}
