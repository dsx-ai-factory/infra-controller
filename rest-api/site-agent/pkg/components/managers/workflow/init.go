// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"errors"
	"os"
	"path/filepath"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
	"github.com/rs/zerolog/log"
	"gopkg.in/fsnotify.v1"
)

// CertExpirationMetric is a prometheus metric for Site Agent Temporal
// certificate expiration. Registered in Init rather than here, because the
// namespace comes from config that is not loaded yet at package init.
var CertExpirationMetric prometheus.Gauge

const (
	// MetricTemporalConnAttempted - Metric Temporal Conn Attempted
	MetricTemporalConnAttempted = "temporal_connection_attempted"
	// MetricTemporalConnSucc - Metric Temporal Conn Succ
	MetricTemporalConnSucc = "temporal_connection_succeeded"
	// MetricTemporalConnStatus - Metric Temporal Conn Status
	MetricTemporalConnStatus = "temporal_connection_status"
	// Kubernetes projects Secret keys through the ..data symlink and atomically
	// replaces that symlink on updates.
	kubernetesSecretDataSymlink = "..data"
)

// Init - initialize the workflow orchestrator
func (wflow *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Workflow: Initializing workflow orchestrator")

	CertExpirationMetric = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
		Name:      "temporal_cert_expiration_timestamp_seconds",
		Help:      "Unix timestamp in seconds when the Temporal client certificate expires",
	})

	health := func() float64 {
		return float64(ManagerAccess.Data.EB.Managers.Workflow.State.HealthStatus.Load())
	}
	prometheus.MustRegister(
		// Keep `temporal_connection_status` as a counter for compatibility.
		// Health can decrease, so new consumers should use the
		// `temporal_connection_health` gauge.
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricTemporalConnStatus,
			Help:      "Temporal health status of the Site Agent",
		}, health),
		prometheus.NewGaugeFunc(prometheus.GaugeOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      "temporal_connection_health",
			Help:      "Current Temporal connection health of the Site Agent: 0 unhealthy, 1 healthy; 2 reserved for unknown",
		}, health),
	)
	ManagerAccess.Data.EB.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompUnhealthy))

	// The `_total` names identify cumulative counters. Retain the legacy
	// names for existing dashboards and alerts.
	for _, name := range []string{MetricTemporalConnAttempted, "temporal_connection_attempted_total"} {
		prometheus.MustRegister(
			prometheus.NewCounterFunc(prometheus.CounterOpts{
				Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
				Name:      name,
				Help:      "Temporal connections attempted by the Site Agent",
			},
				func() float64 {
					return float64(ManagerAccess.Data.EB.Managers.Workflow.State.ConnectionAttempted.Load())
				}))
	}

	for _, name := range []string{MetricTemporalConnSucc, "temporal_connection_succeeded_total"} {
		prometheus.MustRegister(
			prometheus.NewCounterFunc(prometheus.CounterOpts{
				Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
				Name:      name,
				Help:      "Temporal connections succeeded by the Site Agent",
			},
				func() float64 {
					return float64(ManagerAccess.Data.EB.Managers.Workflow.State.ConnectionSucc.Load())
				}))
	}
}

// Start the workflow orchestrator
func (wflow *API) Start() {
	ManagerAccess.Data.EB.Log.Info().Msg("Workflow: Starting the workflow orchestrator")
	Orchestrator()
	wflow.WatchCertFile()
}

// WatchCertFile - Watch Cert File
func (wflow *API) WatchCertFile() {
	fileName, caCertPath := ManagerAccess.Conf.EB.Temporal.GetTemporalCACertFilePath()
	kpFileName, clientCertPath := ManagerAccess.Conf.EB.Temporal.GetTemporalClientCertFilePath()
	root := ManagerAccess.Conf.EB.Temporal.TemporalCertPath
	file := make(map[string]bool)
	file[filepath.Clean(caCertPath+fileName)] = true
	for _, v := range kpFileName {
		file[filepath.Clean(clientCertPath+v)] = true
	}

	w, err := newCertWatcher(root, []string{caCertPath, clientCertPath})
	if err != nil {
		log.Panic().Msgf("Workflow: Failed to watch Temporal cert files: %v", err)
	}
	go watchCertFiles(w, root, file, Orchestrator)
}

// newCertWatcher watches the cert mount root and each cert directory.
//
// Kubelet mounts the Secret items as `ca` and `client` symlinks through `..data`,
// so a watch on either directory follows the symlink into the versioned data
// directory. Kubelet deletes that directory after each update, which drops the
// watch. The root watch survives every update and reports the `..data` swap.
// The directory watches still cover mounts that write the files in place.
func newCertWatcher(root string, dirs []string) (*fsnotify.Watcher, error) {
	log.Info().Msgf("Workflow: Watching Temporal cert files under %v", root)

	w, err := fsnotify.NewWatcher()
	if err != nil {
		return nil, err
	}

	for _, dir := range append([]string{root}, dirs...) {
		if err := w.Add(dir); err != nil {
			w.Close()
			return nil, err
		}
	}

	return w, nil
}

// isTemporalCertUpdate reports whether event changes a watched cert file or
// swaps in a new Kubernetes Secret data directory under root.
func isTemporalCertUpdate(event fsnotify.Event, files map[string]bool, root string) bool {
	eventPath := filepath.Clean(event.Name)
	if files[eventPath] {
		return true
	}

	return event.Op&fsnotify.Create != 0 &&
		filepath.Dir(eventPath) == filepath.Clean(root) &&
		filepath.Base(eventPath) == kubernetesSecretDataSymlink
}

// watchCertFiles calls reload for every cert update until w is closed.
func watchCertFiles(w *fsnotify.Watcher, root string, files map[string]bool, reload func()) {
	defer w.Close()

	for {
		select {
		// Read from Errors.
		case err, ok := <-w.Errors:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return
			}
			// The kqueue backend opens every entry of a watched directory, so it
			// reports the versioned directory kubelet just deleted. The root
			// watch still sees the `..data` swap, so no update is lost.
			if errors.Is(err, os.ErrNotExist) {
				log.Warn().Err(err).Msg("Workflow: Temporal cert files watcher lost a deleted entry")
				continue
			}
			log.Panic().Msgf("Workflow: Temporal cert files watcher closed: %v", err.Error())
		// Read from Events.
		case e, ok := <-w.Events:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return
			}

			if !isTemporalCertUpdate(e, files, root) {
				continue
			}
			reload()
			log.Info().Msgf("Workflow: Resuming Temporal cert files watcher for: %v ", e.String())
		}
	}
}
