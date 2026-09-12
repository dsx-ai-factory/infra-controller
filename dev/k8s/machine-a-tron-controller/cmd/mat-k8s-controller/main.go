// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// mat-k8s-controller is a Kubernetes controller that reconciles Services
// for machine-a-tron mock BMC endpoints.
//
// It discovers machine-a-tron pods via their bmc-mock Services, polls each
// pod's /machines/status API, and creates/updates/deletes Kubernetes Services
// to expose Redfish (and optionally IPMI) endpoints for each mock BMC.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/rs/zerolog"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/tools/clientcmd"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/controller"
	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
)

func main() {
	// Flags
	namespace := flag.String("namespace", envOrDefault("NAMESPACE", "nico-system"),
		"Kubernetes namespace for Services and machine-a-tron discovery")
	discoverySelector := flag.String("discovery-selector", envOrDefault("DISCOVERY_SELECTOR", "nvidia-infra-controller/mat-service=true"),
		"Label selector for discovering machine-a-tron bmc-mock Services")
	syncInterval := flag.Duration("sync-interval", parseDurationOrDefault("SYNC_INTERVAL", 30*time.Second),
		"Interval between reconciliation passes")
	kubeconfig := flag.String("kubeconfig", os.Getenv("KUBECONFIG"),
		"Path to kubeconfig file (uses in-cluster config if empty, development only)")
	targetSelector := flag.String("target-selector", envOrDefault("TARGET_SELECTOR", "app.kubernetes.io/name=nico-machine-a-tron"),
		"Pod selector for Services (comma-separated key=value pairs)")
	insecureSkipVerify := flag.Bool("insecure-skip-verify", envBoolOrDefault("INSECURE_SKIP_VERIFY", false),
		"Skip TLS certificate verification (use only for development with self-signed certs)")
	logLevel := flag.String("log-level", envOrDefault("LOG_LEVEL", "info"),
		"Log level (debug, info, warn, error)")

	flag.Parse()

	// Validate sync interval
	if *syncInterval <= 0 {
		fmt.Fprintf(os.Stderr, "error: sync-interval must be positive, got %v\n", *syncInterval)
		os.Exit(1)
	}

	// Setup logger
	level, err := zerolog.ParseLevel(*logLevel)
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
		Str("namespace", *namespace).
		Str("discovery_selector", *discoverySelector).
		Dur("sync_interval", *syncInterval).
		Str("target_selector", *targetSelector).
		Bool("insecure_skip_verify", *insecureSkipVerify).
		Msg("starting controller")

	// Create Kubernetes client
	var k8sConfig *rest.Config
	if *kubeconfig != "" {
		k8sConfig, err = clientcmd.BuildConfigFromFlags("", *kubeconfig)
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
	if *insecureSkipVerify {
		clientOpts = append(clientOpts, matclient.WithInsecureSkipVerify())
	}

	// Parse target selector
	selector := parseSelector(*targetSelector)

	// Create service builder
	builder := &controller.ServiceBuilder{
		Namespace:    *namespace,
		BaseSelector: selector,
	}

	// Create deployment client for owner reference lookups
	deployClient := &realDeploymentClient{clientset: clientset}

	// Create discovery and reconciler
	discovery := controller.NewMatPodDiscovery(clientset, *namespace, *discoverySelector)
	k8sClient := controller.NewRealK8sServiceClient(clientset)
	reconciler := controller.NewReconciler(discovery, builder, k8sClient, deployClient, clientOpts, logger)

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

	// Run reconciliation loop
	ticker := time.NewTicker(*syncInterval)
	defer ticker.Stop()

	// Run initial reconciliation
	runReconcile(ctx, reconciler, logger)

	for {
		select {
		case <-ctx.Done():
			logger.Info().Msg("shutting down")
			return
		case <-ticker.C:
			runReconcile(ctx, reconciler, logger)
		}
	}
}

func runReconcile(ctx context.Context, r *controller.Reconciler, logger zerolog.Logger) {
	start := time.Now()
	result := r.Reconcile(ctx)
	elapsed := time.Since(start)

	logEvent := logger.Info().
		Int("created", result.Created).
		Int("adopted", result.Adopted).
		Int("updated", result.Updated).
		Int("deleted", result.Deleted).
		Dur("elapsed", elapsed)

	if len(result.Errors) > 0 {
		logEvent = logger.Error().
			Int("created", result.Created).
			Int("adopted", result.Adopted).
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

func envOrDefault(key, defaultValue string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return defaultValue
}

func envBoolOrDefault(key string, defaultValue bool) bool {
	if v := os.Getenv(key); v != "" {
		if b, err := strconv.ParseBool(v); err == nil {
			return b
		}
	}
	return defaultValue
}

func parseDurationOrDefault(envKey string, defaultValue time.Duration) time.Duration {
	if v := os.Getenv(envKey); v != "" {
		if d, err := time.ParseDuration(v); err == nil {
			return d
		}
	}
	return defaultValue
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
