// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// mat-k8s-controller is a Kubernetes controller that reconciles Services
// for machine-a-tron mock BMC endpoints.
//
// It discovers machine-a-tron pods via their bmc-mock Services, polls each
// pod's /machines/status API, and creates/updates/deletes Kubernetes Services
// to expose Redfish (and optionally IPMI) endpoints for each mock BMC.
//
// It also publishes the discovered machine-a-tron identities and base URLs
// over a pod-local HTTP endpoint (see pkg/sourcelist) for sidecar containers
// that must not hold Kubernetes API credentials, and a liveness endpoint on
// the pod network (see pkg/healthz) for the kubelet.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"net/netip"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/controller"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/healthz"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/httpserver"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/sourcelist"
)

// Defaults of the endpoints the controller serves besides reconciling.
const (
	// defaultHealthAddr is the pod-network address of the liveness endpoint
	// (GET /healthz). Unlike the source list it is meant to be reached by the
	// kubelet, so it binds every interface.
	defaultHealthAddr = ":8091"
	// defaultHealthStaleAfter is how long the reconcile loop may go without
	// completing a pass before /healthz reports a stall. A full pass at scale
	// creates thousands of Services under the client rate limit, so the bound
	// is far above the sync interval.
	defaultHealthStaleAfter = 10 * time.Minute
)

// errUsage marks a command line the flag package has already reported on
// stderr together with the usage text.
var errUsage = errors.New("invalid command line")

// options are the parsed command line flags.
type options struct {
	namespace              string
	discoverySelector      string
	syncInterval           time.Duration
	kubeconfig             string
	targetSelector         string
	insecureSkipVerify     bool
	logLevel               string
	sourceListAddr         string
	sourceListDebounce     time.Duration
	healthAddr             string
	healthStaleAfter       time.Duration
	enableStateAnnotations bool
}

// parseOptions parses args (without the program name) with environment
// variables as flag defaults and validates the parsed values. lookupEnv has
// the contract of os.LookupEnv: a variable that is set is the flag's default
// even when its value is empty, which is how SOURCE_LIST_ADDR and HEALTH_ADDR
// disable their listeners, while an unset variable leaves the built-in
// default. A command line the flag package rejects returns an error wrapping
// errUsage; the flag package has already reported it together with the usage
// text.
func parseOptions(args []string, lookupEnv func(string) (string, bool)) (*options, error) {
	envOrDefault := func(key, defaultValue string) string {
		if v, ok := lookupEnv(key); ok {
			return v
		}
		return defaultValue
	}
	envBoolOrDefault := func(key string, defaultValue bool) bool {
		if v, ok := lookupEnv(key); ok {
			if b, err := strconv.ParseBool(v); err == nil {
				return b
			}
		}
		return defaultValue
	}
	durationOrDefault := func(key string, defaultValue time.Duration) time.Duration {
		if v, ok := lookupEnv(key); ok {
			if d, err := time.ParseDuration(v); err == nil {
				return d
			}
		}
		return defaultValue
	}

	opts := &options{}
	fs := flag.NewFlagSet("mat-k8s-controller", flag.ContinueOnError)
	fs.StringVar(&opts.namespace, "namespace", envOrDefault("NAMESPACE", "nico-system"),
		"Kubernetes namespace for Services and machine-a-tron discovery")
	fs.StringVar(&opts.discoverySelector, "discovery-selector", envOrDefault("DISCOVERY_SELECTOR", "nvidia-infra-controller/mat-service=true"),
		"Label selector for discovering machine-a-tron bmc-mock Services")
	fs.DurationVar(&opts.syncInterval, "sync-interval", durationOrDefault("SYNC_INTERVAL", 30*time.Second),
		"Interval between reconciliation passes")
	fs.StringVar(&opts.kubeconfig, "kubeconfig", envOrDefault("KUBECONFIG", ""),
		"Path to kubeconfig file (uses in-cluster config if empty, development only)")
	fs.StringVar(&opts.targetSelector, "target-selector", envOrDefault("TARGET_SELECTOR", "app.kubernetes.io/name=nico-machine-a-tron"),
		"Pod selector for Services (comma-separated key=value pairs)")
	fs.BoolVar(&opts.insecureSkipVerify, "insecure-skip-verify", envBoolOrDefault("INSECURE_SKIP_VERIFY", false),
		"Skip TLS certificate verification (use only for development with self-signed certs)")
	fs.StringVar(&opts.logLevel, "log-level", envOrDefault("LOG_LEVEL", "info"),
		"Log level (debug, info, warn, error)")
	fs.StringVar(&opts.sourceListAddr, "source-list-addr", envOrDefault("SOURCE_LIST_ADDR", sourcelist.DefaultAddr),
		"Listen address for the pod-local, unauthenticated source list endpoint; must be a loopback IP address (empty disables it)")
	fs.DurationVar(&opts.sourceListDebounce, "source-list-debounce", durationOrDefault("SOURCE_LIST_DEBOUNCE", sourcelist.DefaultDebounce),
		"Minimum age of a changed source set before a later discovery pass publishes it (0 publishes changes at once)")
	fs.StringVar(&opts.healthAddr, "health-addr", envOrDefault("HEALTH_ADDR", defaultHealthAddr),
		"Listen address for the liveness endpoint GET /healthz on the pod network (empty disables it)")
	fs.DurationVar(&opts.healthStaleAfter, "health-stale-after", durationOrDefault("HEALTH_STALE_AFTER", defaultHealthStaleAfter),
		"How long the reconcile loop may go without completing a pass before /healthz reports a stall (0 disables the check)")
	fs.BoolVar(&opts.enableStateAnnotations, "enable-state-annotations", envBoolOrDefault("ENABLE_STATE_ANNOTATIONS", false),
		"Include machine state annotations (api-state, power-state) on Services; causes frequent updates")
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return nil, err
		}
		return nil, fmt.Errorf("%w: %w", errUsage, err)
	}

	if opts.syncInterval <= 0 {
		return nil, fmt.Errorf("sync-interval must be positive, got %v", opts.syncInterval)
	}
	if opts.sourceListDebounce < 0 {
		return nil, fmt.Errorf("source-list-debounce must not be negative, got %v", opts.sourceListDebounce)
	}
	if opts.healthStaleAfter < 0 {
		return nil, fmt.Errorf("health-stale-after must not be negative, got %v", opts.healthStaleAfter)
	}
	if opts.sourceListAddr != "" && !isLoopbackAddr(opts.sourceListAddr) {
		return nil, fmt.Errorf("source-list-addr must be a loopback IP address such as %s, got %q", sourcelist.DefaultAddr, opts.sourceListAddr)
	}
	return opts, nil
}

// isLoopbackAddr reports whether addr is an ip:port whose IP literal is a
// loopback address. The source list endpoint is unauthenticated, so a host
// name or an address that other containers or pods can reach is refused.
func isLoopbackAddr(addr string) bool {
	ap, err := netip.ParseAddrPort(addr)
	return err == nil && ap.Addr().IsLoopback()
}

func main() {
	opts, err := parseOptions(os.Args[1:], os.LookupEnv)
	if err != nil {
		switch {
		case errors.Is(err, flag.ErrHelp):
			os.Exit(0)
		case errors.Is(err, errUsage):
			// The flag package has printed the error and the usage already.
			os.Exit(2)
		default:
			fmt.Fprintf(os.Stderr, "error: %v\n", err)
			os.Exit(1)
		}
	}

	// Setup logger
	level, err := zerolog.ParseLevel(opts.logLevel)
	if err != nil {
		level = zerolog.InfoLevel
	}
	logger := zerolog.New(zerolog.ConsoleWriter{Out: os.Stderr, TimeFormat: time.RFC3339}).
		Level(level).
		With().
		Timestamp().
		Str("component", "mat-k8s-controller").
		Logger()

	logger.Info().
		Str("namespace", opts.namespace).
		Str("discovery_selector", opts.discoverySelector).
		Dur("sync_interval", opts.syncInterval).
		Str("target_selector", opts.targetSelector).
		Bool("insecure_skip_verify", opts.insecureSkipVerify).
		Str("source_list_addr", opts.sourceListAddr).
		Dur("source_list_debounce", opts.sourceListDebounce).
		Str("health_addr", opts.healthAddr).
		Dur("health_stale_after", opts.healthStaleAfter).
		Bool("enable_state_annotations", opts.enableStateAnnotations).
		Msg("starting controller")

	// Create Kubernetes client
	var k8sConfig *rest.Config
	if opts.kubeconfig != "" {
		k8sConfig, err = clientcmd.BuildConfigFromFlags("", opts.kubeconfig)
	} else {
		k8sConfig, err = rest.InClusterConfig()
	}
	if err != nil {
		logger.Fatal().Err(err).Msg("failed to create Kubernetes config")
	}

	// Increase rate limits for bulk operations
	k8sConfig.QPS = 100
	k8sConfig.Burst = 200

	clientset, err := kubernetes.NewForConfig(k8sConfig)
	if err != nil {
		logger.Fatal().Err(err).Msg("failed to create Kubernetes clientset")
	}

	// Create machine-a-tron client options
	clientOpts := []matclient.Option{matclient.WithLogger(logger)}
	if opts.insecureSkipVerify {
		clientOpts = append(clientOpts, matclient.WithInsecureSkipVerify())
	}

	// Create service builder
	builder := &controller.ServiceBuilder{
		Namespace:              opts.namespace,
		BaseSelector:           parseSelector(opts.targetSelector),
		EnableStateAnnotations: opts.enableStateAnnotations,
	}

	// Create deployment client for owner reference lookups
	deployClient := &realDeploymentClient{clientset: clientset}

	discovery := controller.NewMatPodDiscovery(clientset, opts.namespace, opts.discoverySelector)
	newReconciler := func(discovery controller.Discovery) reconciler {
		return controller.NewReconciler(discovery, builder, controller.NewRealK8sServiceClient(clientset), deployClient, clientOpts, logger)
	}

	// Setup signal handling
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		sig := <-sigCh
		logger.Info().Str("signal", sig.String()).Msg("received shutdown signal")
		cancel()
	}()

	if err := run(ctx, opts, discovery, newReconciler, logger); err != nil {
		logger.Error().Err(err).Msg("controller stopped")
		os.Exit(1)
	}
	logger.Info().Msg("shutting down")
}

// reconciler is the part of controller.Reconciler the loop drives.
type reconciler interface {
	Reconcile(ctx context.Context) controller.ReconcileResult
}

// run wires discovery into the source list registry, serves the source list
// and liveness endpoints, and runs the reconcile loop until ctx is cancelled.
//
// newReconciler receives the discovery wrapped for the registry, so every
// successful discovery pass the reconciler makes is what the source list
// publishes. A listen failure on either endpoint ends the run with that
// error: a sidecar consumer depends on the source list, and a controller
// without its liveness endpoint would be restarted by the kubelet anyway.
func run(ctx context.Context, opts *options, discovery controller.Discovery, newReconciler func(controller.Discovery) reconciler, logger zerolog.Logger) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	registry := sourcelist.NewRegistry(sourcelist.WithDebounce(opts.sourceListDebounce))
	r := newReconciler(sourcelist.WrapDiscovery(discovery, registry))
	liveness := healthz.New(opts.healthStaleAfter)

	var servers sync.WaitGroup
	serverErrs := make(chan error, 2)
	serve := func(name, addr string, handler http.Handler) {
		servers.Add(1)
		go func() {
			defer servers.Done()
			if err := httpserver.Run(ctx, addr, handler, logger, name); err != nil {
				serverErrs <- fmt.Errorf("%s: %w", name, err)
				cancel()
			}
		}()
	}
	if opts.sourceListAddr != "" {
		serve("source list endpoint", opts.sourceListAddr, sourcelist.Handler(registry))
	}
	if opts.healthAddr != "" {
		serve("liveness endpoint", opts.healthAddr, liveness.Handler())
	}

	ticker := time.NewTicker(opts.syncInterval)
	defer ticker.Stop()

	// Run initial reconciliation
	runReconcile(ctx, r, logger)
	liveness.MarkProgress()

loop:
	for {
		select {
		case <-ctx.Done():
			break loop
		case <-ticker.C:
			runReconcile(ctx, r, logger)
			liveness.MarkProgress()
		}
	}

	servers.Wait()
	close(serverErrs)
	var errs []error
	for err := range serverErrs {
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

func runReconcile(ctx context.Context, r reconciler, logger zerolog.Logger) {
	start := time.Now()
	result := r.Reconcile(ctx)
	elapsed := time.Since(start)

	logEvent := logger.Info().
		Int("created", result.Created).
		Int("updated", result.Updated).
		Int("deleted", result.Deleted).
		Dur("elapsed", elapsed)

	if len(result.Errors) > 0 {
		logEvent = logger.Error().
			Int("created", result.Created).
			Int("updated", result.Updated).
			Int("deleted", result.Deleted).
			Int("errors", len(result.Errors)).
			Dur("elapsed", elapsed)

		for _, err := range result.Errors {
			logger.Error().Err(err).Msg("reconciliation error")
		}
	}

	logEvent.Msg("reconciliation complete")
}

func parseSelector(s string) map[string]string {
	result := make(map[string]string)
	if s == "" {
		return result
	}

	for _, pair := range strings.Split(s, ",") {
		if kv := strings.SplitN(pair, "=", 2); len(kv) == 2 {
			result[strings.TrimSpace(kv[0])] = strings.TrimSpace(kv[1])
		}
	}
	return result
}

// realDeploymentClient implements controller.DeploymentClient using the Kubernetes API.
type realDeploymentClient struct {
	clientset kubernetes.Interface
}

func (c *realDeploymentClient) Get(ctx context.Context, namespace, name string) (*metav1.OwnerReference, error) {
	deploy, err := c.clientset.AppsV1().Deployments(namespace).Get(ctx, name, metav1.GetOptions{})
	if err != nil {
		return nil, err
	}
	return &metav1.OwnerReference{
		APIVersion: "apps/v1",
		Kind:       "Deployment",
		Name:       deploy.Name,
		UID:        deploy.UID,
	}, nil
}
