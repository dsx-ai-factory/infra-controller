// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"

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
)

// Init - initialize the workflow orchestrator
func (wflow *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Workflow: Initializing workflow orchestrator")

	CertExpirationMetric = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
		Name:      "temporal_cert_expiration",
		Help:      "The expiration date of the Temporal certificate",
	})

	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricTemporalConnStatus,
			Help:      "Temporal health status of the Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.Workflow.State.HealthStatus.Load())
			}))
	ManagerAccess.Data.EB.Managers.Workflow.State.HealthStatus.Store(uint64(computils.CompUnhealthy))

	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricTemporalConnAttempted,
			Help:      "Temporal connections attempted by the Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.Workflow.State.ConnectionAttempted.Load())
			}))

	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricTemporalConnSucc,
			Help:      "Temporal connections succeeded by the Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.Workflow.State.ConnectionSucc.Load())
			}))
}

// GetState - handle http request
func (wflow *API) GetState() []string {
	wc := ManagerAccess.Conf.EB.Temporal
	wt := ManagerAccess.Data.EB.Managers.Workflow
	var strs []string
	strs = append(strs, fmt.Sprintln("Temporal Host: ", wc.Host, wc.Port))
	strs = append(strs, fmt.Sprintln("Temporal Connection Attempted: ", wt.State.ConnectionAttempted.Load()))
	strs = append(strs, fmt.Sprintln("Temporal Connection Succeeded: ", wt.State.ConnectionSucc.Load()))
	strs = append(strs, fmt.Sprintln("Temporal Status: ", computils.CompStatus(wt.State.HealthStatus.Load()).String()))
	strs = append(strs, fmt.Sprintln("Temporal Last Error: ", wt.State.Err()))
	strs = append(strs, fmt.Sprintln("Temporal Connection Time: ", wt.State.ConnectionTime()))

	return strs
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
	path := []string{caCertPath, clientCertPath}
	file := make(map[string]bool)
	file[caCertPath+fileName] = true
	for _, v := range kpFileName {
		file[clientCertPath+v] = true
	}
	go wflow.watchFiles(path, file)
}

// watchFiles watch on the secret files
func (wflow *API) watchFiles(path []string, file map[string]bool) {
	log.Info().Msgf("Workflow: Watching Temporal cert files %v", path)

	// Create a new watcher.
	w, err := fsnotify.NewWatcher()
	if err != nil {
		log.Panic().Msgf("Workflow: Failed to create Temporal cert files watcher: %v", err)
	}
	defer w.Close()

	// Watch the directory, not the file itself.
	for _, v := range path {
		err = w.Add(v)
		if err != nil {
			log.Panic().Msgf("Workflow: Failed to add Temporal cert files to watcher: %v", err)
		}
	}

	for {
		select {
		// Read from Errors.
		case err, ok := <-w.Errors:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return
			}
			log.Panic().Msgf("Workflow: Temporal cert files watcher closed: %v", err.Error())
		// Read from Events.
		case e, ok := <-w.Events:
			if !ok { // Channel was closed (i.e. Watcher.Close() was called).
				return
			}

			// log.Info().Msgf("Got event %v ", e.String())
			_, ok = file[e.Name]
			if !ok {
				continue
			}
			Orchestrator()
			log.Info().Msgf("Workflow: Resuming Temporal cert files watcher for: %v ", e.String())
		}
	}
}
