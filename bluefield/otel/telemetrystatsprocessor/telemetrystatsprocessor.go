/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package telemetrystatsprocessor

import (
	"context"
	"fmt"
	"net/http"
	"regexp"
	"strings"
	"sync"
	"time"

	"go.opentelemetry.io/collector/pdata/pcommon"
	"go.opentelemetry.io/collector/pdata/plog"
	"go.opentelemetry.io/collector/pdata/pmetric"
	"go.uber.org/zap"
)

var (
	// log stats are exported to a prometheus endpoint
	singletonExporter *logStatsExporter
	exporterOnce      sync.Once

	// stats about telemetry_stats
	telemetryStatCountsOnce         sync.Once
	telemetryStatCountsLock         sync.Mutex
	telemetryStatCounts             map[string]count
	telemetryStatCountsReporter     *telemetryStatsProcessor
	telemetryStatCountsReporterLock sync.Mutex

	// regular expressions
	rePromInvalid = regexp.MustCompile(`[^a-zA-Z0-9_]`)
)

type count struct {
	value     int64
	startTime pcommon.Timestamp
}

type telemetryStatsProcessor struct {
	logger             *zap.Logger
	config             *Config
	logCounts          map[string]count
	metricCounts       map[string]count
	logCountsRWLock    sync.RWMutex
	metricCountsRWLock sync.RWMutex
	metricStatsChannel chan telemetryStatsDatapoint
	exporter           *logStatsExporter
	stopChannel        chan struct{}
	stopWaiters        sync.WaitGroup
}

type logStatsExporter struct {
	logger         *zap.Logger
	server         *http.Server
	processors     []*telemetryStatsProcessor
	requestsRWLock sync.RWMutex // in progress HTTP requests
}

type telemetryStatsDatapoint struct {
	name      string
	startTime pcommon.Timestamp
	value     int64
	labels    map[string]string
}

// processor constructor
func newTelemetryStatsProcessor(
	config *Config,
	logger *zap.Logger,
) (*telemetryStatsProcessor, error) {
	telemetryStatCountsOnce.Do(func() {
		telemetryStatCounts = make(map[string]count)
	})

	p := &telemetryStatsProcessor{
		logger:      logger,
		config:      config,
		stopChannel: make(chan struct{}),
	}

	if len(config.LogGroupings) > 0 {
		p.logCounts = make(map[string]count)
		exporter, err := getLogStatsExporter(p)
		if err != nil {
			return nil, fmt.Errorf("failed to create log stats exporter: %w", err)
		}
		p.exporter = exporter
	}

	if len(config.MetricGroupings) > 0 {
		p.metricCounts = make(map[string]count)
		p.metricStatsChannel = make(chan telemetryStatsDatapoint, 128)
		p.stopWaiters.Add(1)
		go p.metricStatsLoop()
	}

	return p, nil
}

// processor destructor
func (p *telemetryStatsProcessor) cleanup() {
	close(p.stopChannel)
	p.stopWaiters.Wait()

	if p.exporter != nil {
		p.exporter.Shutdown()
		p.exporter.removeProcessor(p)
		p.exporter = nil
	} else {
		telemetryStatCountsReporterLock.Lock()
		if telemetryStatCountsReporter == p {
			telemetryStatCountsReporter = nil
		}
		telemetryStatCountsReporterLock.Unlock()
	}
}

func (p *telemetryStatsProcessor) processLogs(
	ctx context.Context,
	ld plog.Logs,
) (plog.Logs, error) {
	now := pcommon.NewTimestampFromTime(time.Now())

	p.logCountsRWLock.Lock()
	defer p.logCountsRWLock.Unlock()

	for i := 0; i < ld.ResourceLogs().Len(); i++ {
		rls := ld.ResourceLogs().At(i)
		resourceAttrs := rls.Resource().Attributes()
		for j := 0; j < rls.ScopeLogs().Len(); j++ {
			sl := rls.ScopeLogs().At(j)
			scopeAttrs := sl.Scope().Attributes()
			for k := 0; k < sl.LogRecords().Len(); k++ {
				lr := sl.LogRecords().At(k)
				logAttrs := lr.Attributes()
				attrs := NewAttributes(resourceAttrs, scopeAttrs, logAttrs)
				for _, grouping := range p.config.LogGroupings {
					key := generateLogKey(grouping, attrs)
					count, ok := p.logCounts[key]
					if !ok {
						count.startTime = now
					}
					count.value++
					p.logCounts[key] = count
				}
			}
		}
	}

	return ld, nil
}

func (p *telemetryStatsProcessor) processMetrics(
	ctx context.Context,
	md pmetric.Metrics,
) (pmetric.Metrics, error) {
	now := pcommon.NewTimestampFromTime(time.Now())

	// Step 1: Process incoming metrics from the pipeline.
	p.metricCountsRWLock.Lock()
	for i := 0; i < md.ResourceMetrics().Len(); i++ {
		rm := md.ResourceMetrics().At(i)
		resourceAttrs := rm.Resource().Attributes()
		for j := 0; j < rm.ScopeMetrics().Len(); j++ {
			sm := rm.ScopeMetrics().At(j)
			scopeAttrs := sm.Scope().Attributes()
			for k := 0; k < sm.Metrics().Len(); k++ {
				metric := sm.Metrics().At(k)
				p.processMetric(metric, resourceAttrs, scopeAttrs, now)
			}
		}
	}
	p.metricCountsRWLock.Unlock()

	// Step 2: Drain p.metricStatsChannel of all available datapoints
	// generated from the metric stats accumulated in "Step 1" on the
	// configured metric_scrape_interval and append them to incoming
	// metrics forwarded to the next stage in the pipeline.
	//
	// Step 2a: Create a new resource level object for metric stats
	// distinct from incoming metrics and append it to the incoming
	// metrics. This avoids mixing scope level attributes.
	rmStats := md.ResourceMetrics().AppendEmpty()
	smStats := rmStats.ScopeMetrics().AppendEmpty()
	smStats.Scope().SetName(ProcessorName)
	smStats.Scope().SetVersion(Version)
	// Step 2b: Copy the resource attributes of incoming metrics to the new
	// metric stats.
	incomingResourceAttrs := md.ResourceMetrics().At(0).Resource().Attributes()
	resourceAttrs := rmStats.Resource().Attributes()
	incomingResourceAttrs.CopyTo(resourceAttrs)
	// Step 2c: Overwrite resource attributes of metric stats with any
	// configured labels. If a configured label would overwrite an existing
	// resource label, the existing label was already preserved as a
	// renamed datapoint label.
	for _, configuredLabel := range p.config.Labels {
		resourceAttrs.PutStr(configuredLabel.Name, configuredLabel.Value)
	}
	// Step 2d: Add a datapoint to the new metric stats for each item
	// received from the channel.
	for {
		select {
		case dp := <-p.metricStatsChannel:
			emitTime := pcommon.NewTimestampFromTime(time.Now())
			if emitTime < dp.startTime {
				emitTime = dp.startTime
			}
			metric := smStats.Metrics().AppendEmpty()
			metric.SetName(dp.name)
			metric.SetDescription("Number of datapoints counted")
			metric.SetUnit("1")
			sum := metric.SetEmptySum()
			sum.SetIsMonotonic(true)
			sum.SetAggregationTemporality(
				pmetric.AggregationTemporalityCumulative)
			datapoint := sum.DataPoints().AppendEmpty()
			datapoint.SetIntValue(dp.value)
			datapoint.SetStartTimestamp(dp.startTime)
			datapoint.SetTimestamp(emitTime)
			for k, v := range dp.labels {
				datapoint.Attributes().PutStr(k, v)
			}
		default:
			// No more metric stats to process
			return md, nil
		}
	}
}

func (p *telemetryStatsProcessor) processMetric(
	metric pmetric.Metric,
	resourceAttrs pcommon.Map,
	scopeAttrs pcommon.Map,
	now pcommon.Timestamp,
) {
	// In case log stats written to the configured prometheus endpoint pass
	// through this processor again, exclude them here.
	if strings.HasPrefix(metric.Name(), prefixStr) {
		return
	}

	for i := range p.config.MetricGroupings {
		grouping := &p.config.MetricGroupings[i]
		p.processMetricGrouping(metric, grouping, resourceAttrs, scopeAttrs, now)
	}
}

func (p *telemetryStatsProcessor) processMetricGrouping(
	metric pmetric.Metric,
	grouping *MetricGrouping,
	resourceAttrs pcommon.Map,
	scopeAttrs pcommon.Map,
	now pcommon.Timestamp,
) {
	var datapointCount int

	switch metric.Type() {
	case pmetric.MetricTypeGauge:
		datapointCount = metric.Gauge().DataPoints().Len()
	case pmetric.MetricTypeSum:
		datapointCount = metric.Sum().DataPoints().Len()
	case pmetric.MetricTypeHistogram:
		datapointCount = metric.Histogram().DataPoints().Len()
	case pmetric.MetricTypeSummary:
		datapointCount = metric.Summary().DataPoints().Len()
	default:
		return // ignore unsupported metric type
	}

	// Process datapoints
	for i := 0; i < datapointCount; i++ {
		var datapointAttrs pcommon.Map

		switch metric.Type() {
		case pmetric.MetricTypeGauge:
			datapointAttrs = metric.Gauge().DataPoints().At(i).Attributes()
		case pmetric.MetricTypeSum:
			datapointAttrs = metric.Sum().DataPoints().At(i).Attributes()
		case pmetric.MetricTypeHistogram:
			datapointAttrs = metric.Histogram().DataPoints().At(i).Attributes()
		case pmetric.MetricTypeSummary:
			datapointAttrs = metric.Summary().DataPoints().At(i).Attributes()
		}

		attrs := NewAttributes(resourceAttrs, scopeAttrs, datapointAttrs)
		p.processDatapoint(metric, grouping, attrs, now)
	}
}

func (p *telemetryStatsProcessor) processDatapoint(
	metric pmetric.Metric,
	grouping *MetricGrouping,
	attrs *Attributes,
	now pcommon.Timestamp,
) {
	if !includeMetricDatapoint(grouping, metric, attrs) {
		return
	}
	key := generateMetricKey(grouping, metric, attrs)
	count, ok := p.metricCounts[key]

	if !ok {
		count.startTime = now
	}

	count.value++
	p.metricCounts[key] = count
}

func (p *telemetryStatsProcessor) metricStatsLoop() {
	defer p.stopWaiters.Done()

	ticker := time.NewTicker(p.config.MetricScrapeInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			p.scrapeMetricStats()
		case <-p.stopChannel:
			return
		}
	}
}

func (p *telemetryStatsProcessor) scrapeMetricStats() {
	// Step 1: While holding the read lock, traverse the map of accumulated
	// metric counts and generate a datapoint for each map entry.
	p.metricCountsRWLock.RLock()
	datapoints := make([]telemetryStatsDatapoint, 0, len(p.metricCounts))
	for key, count := range p.metricCounts {
		parts := strings.Split(key, ":")
		labels := make(map[string]string)
		labels["source"] = sourceStr
		labels["grouping"] = parts[0]
		for _, part := range parts[1:] {
			kv := strings.SplitN(part, "=", 2)
			if len(kv) == 2 {
				switch kv[0] {
				case "__name":
					labels["metric_name"] = kv[1]
				case "__type":
					labels["metric_type"] = kv[1]
				default:
					labels[kv[0]] = kv[1]
				}
			}
		}
		for _, configuredLabel := range p.config.Labels {
			// If a configured label would overwrite an existing
			// label, rename the existing label. The configured
			// label will be written later as a resource attribute.
			if value, exists := labels[configuredLabel.Name]; exists {
				delete(labels, configuredLabel.Name)
				labels["metric_"+configuredLabel.Name] = value
			}
		}
		datapoints = append(datapoints, telemetryStatsDatapoint{
			name:      telemetryStatName("datapoints_total"),
			value:     count.value,
			startTime: count.startTime,
			labels:    labels,
		})
	}
	p.metricCountsRWLock.RUnlock()

	if p.config.IncludeTelemetryStats {
		p.updateTelemetryStatCounts(datapoints, telemetryStatName("datapoints_total"))
	}

	// Step 2: Without holding the read lock, send the generated datapoints
	// to the channel read by processMetrics(), blocking whenever the
	// channel is full. Each call to process incoming metrics will drain
	// the channel until all data points have been added to the pipeline.
	for _, dp := range datapoints {
		p.metricStatsChannel <- dp
	}
	if p.isReportTelemetryStatCounts() {
		statDatapoints := p.getTelemetryStatCounts()
		for _, dp := range statDatapoints {
			p.metricStatsChannel <- dp
		}
	}
}

// Limit reporting of telemetry stat counts to a single processor on each
// scrape interval so they are monotonically increasing.
func (p *telemetryStatsProcessor) isReportTelemetryStatCounts() bool {
	if !p.config.IncludeTelemetryStats {
		return false
	}

	reportTelemetryStatCounts := false

	telemetryStatCountsReporterLock.Lock()
	if telemetryStatCountsReporter == nil {
		telemetryStatCountsReporter = p
	}
	if telemetryStatCountsReporter == p {
		reportTelemetryStatCounts = true
	}
	telemetryStatCountsReporterLock.Unlock()

	return reportTelemetryStatCounts
}

func (p *telemetryStatsProcessor) updateTelemetryStatCounts(
	datapoints []telemetryStatsDatapoint,
	updatedTelemetryStatName string,
) {
	now := pcommon.NewTimestampFromTime(time.Now())

	telemetryStatCountsLock.Lock()
	defer telemetryStatCountsLock.Unlock()

	count, ok := telemetryStatCounts[updatedTelemetryStatName]

	if !ok {
		count.startTime = now
	}

	count.value += int64(len(datapoints))
	telemetryStatCounts[updatedTelemetryStatName] = count
}

func (p *telemetryStatsProcessor) getTelemetryStatCounts() []telemetryStatsDatapoint {
	var statDatapointsCount int
	thisStatName := telemetryStatName("datapoints_total")
	now := pcommon.NewTimestampFromTime(time.Now())

	// ensure that the metric reporting the requested count is itself
	// included in the count
	telemetryStatCountsLock.Lock()
	statCount, ok := telemetryStatCounts[thisStatName]
	if !ok {
		statCount.startTime = now
	}
	statDatapointsCount = len(telemetryStatCounts)
	statCount.value += int64(statDatapointsCount)
	telemetryStatCounts[thisStatName] = statCount
	counts := copyCounts(telemetryStatCounts)
	telemetryStatCountsLock.Unlock()

	statDatapoints := make([]telemetryStatsDatapoint, 0, statDatapointsCount)
	for name, count := range counts {
		labels := make(map[string]string)
		labels["source"] = sourceStr
		labels["metric_name"] = name
		labels["metric_type"] = "Counter"
		for _, configuredLabel := range p.config.Labels {
			labels["metric_"+configuredLabel.Name] = configuredLabel.Value
		}
		statDatapoints = append(statDatapoints, telemetryStatsDatapoint{
			name:      thisStatName,
			value:     count.value,
			startTime: count.startTime,
			labels:    labels,
		})
	}

	return statDatapoints
}

// logStatsExporter constructor
func getLogStatsExporter(p *telemetryStatsProcessor) (*logStatsExporter, error) {
	exporterOnce.Do(func() {
		singletonExporter = &logStatsExporter{
			logger:     p.logger,
			processors: make([]*telemetryStatsProcessor, 0),
		}
	})

	e := singletonExporter
	e.requestsRWLock.Lock()
	defer e.requestsRWLock.Unlock()

	if e.server == nil {
		server := &http.Server{
			Addr:    p.config.GetLogStatsEndpoint(),
			Handler: e,
		}

		errorChannel := make(chan error, 1)
		go func() {
			if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				p.logger.Error("HTTP server error",
					zap.Error(err),
					zap.String("address", server.Addr),
				)
				errorChannel <- err
			}
		}()

		// Wait a short time to check whether the server failed to start
		var serverErr error
		select {
		case err := <-errorChannel:
			serverErr = fmt.Errorf("failed to start server: %w", err)
		case <-time.After(100 * time.Millisecond):
			// done waiting for it to fail
		}

		if serverErr != nil {
			return nil, fmt.Errorf("failed to start server: %w", serverErr)
		}

		e.server = server
	}

	e.processors = append(e.processors, p)

	return e, nil
}

// logStatsExporter destructor
func (e *logStatsExporter) Shutdown() {
	e.requestsRWLock.Lock() // Block new requests and wait for existing ones to complete
	defer e.requestsRWLock.Unlock()

	if e.server == nil {
		return // server is already shutdown
	}

	e.logger.Info("Shutting down log stats processor HTTP server")
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Attempt to gracefully shut down the server
	err := e.server.Shutdown(ctx)
	if err != nil {
		e.logger.Error("Error shutting down log stats processor HTTP server",
			zap.Error(err))

		// If graceful shutdown fails, force close
		if closeErr := e.server.Close(); closeErr != nil {
			e.logger.Error("Error closing log stats processor HTTP server",
				zap.Error(closeErr))
		}
	} else {
		e.logger.Info("log stats processor HTTP server shut down successfully")
	}

	e.server = nil
}

func (e *logStatsExporter) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	e.requestsRWLock.RLock()
	defer e.requestsRWLock.RUnlock()

	if r.URL.Path != "/metrics" {
		http.NotFound(w, r)
		return
	}

	for _, processor := range e.processors {
		scrapeLogStats(w, processor)
	}

	if len(e.processors) > 0 {
		p := e.processors[0]
		if p.config.IncludeTelemetryStats && len(p.config.MetricGroupings) == 0 {
			statDatapoints := p.getTelemetryStatCounts()
			for _, dp := range statDatapoints {
				formattedLabels := formatLabels(dp.labels)
				fmt.Fprintf(w, "%s{%s} %d\n", dp.name, formattedLabels, dp.value)
			}
		}
	}
}

func scrapeLogStats(w http.ResponseWriter, p *telemetryStatsProcessor) {
	// Step 1: While holding the read lock, traverse the map of accumulated
	// log counts and generate a datapoint for each map entry.
	p.logCountsRWLock.RLock()
	datapoints := make([]telemetryStatsDatapoint, 0, len(p.logCounts))
	for key, count := range p.logCounts {
		parts := strings.Split(key, ":")
		labels := make(map[string]string)
		labels["source"] = sourceStr
		labels["grouping"] = parts[0]
		for _, part := range parts[1:] {
			kv := strings.SplitN(part, "=", 2)
			if len(kv) == 2 {
				labels[kv[0]] = kv[1]
			}
		}
		for _, configuredLabel := range p.config.Labels {
			// If a configured label would overwrite an existing
			// label, rename the existing label.
			if value, exists := labels[configuredLabel.Name]; exists {
				delete(labels, configuredLabel.Name)
				labels["log_"+configuredLabel.Name] = value
			}
			// The pipeline that receives log stats from the
			// prometheus endpoint is responsible for writing the
			// configured label as a resource attribute.
		}
		datapoints = append(datapoints, telemetryStatsDatapoint{
			name:   telemetryStatName("log_records_total"),
			value:  count.value,
			startTime: count.startTime,
			labels: labels,
		})
	}
	p.logCountsRWLock.RUnlock()

	if p.config.IncludeTelemetryStats {
		p.updateTelemetryStatCounts(datapoints, telemetryStatName("log_records_total"))
	}

	// Step 2: Without holding the read lock, write the generated
	// datapoints to the configured prometheus endpoint.
	for _, dp := range datapoints {
		formattedLabels := formatLabels(dp.labels)
		fmt.Fprintf(w, "%s{%s} %d\n", dp.name, formattedLabels, dp.value)
	}
}

func (e *logStatsExporter) removeProcessor(p *telemetryStatsProcessor) {
	e.requestsRWLock.Lock()
	defer e.requestsRWLock.Unlock()

	for i := 0; i < len(e.processors); i++ {
		if e.processors[i] == p {
			copy(e.processors[i:], e.processors[i+1:])
			e.processors = e.processors[:len(e.processors)-1]
			break
		}
	}
}

func formatLabels(labels map[string]string) string {
	result := ""
	for k, v := range labels {
		result += fmt.Sprintf("%s=\"%s\",", rePromInvalid.ReplaceAllString(k, "_"), v)
	}
	if len(result) > 0 {
		result = result[:len(result)-1] // Remove trailing comma
	}
	return result
}

// Attributes encapsulates resource, scope, and datapoint level attributes,
// effectively combining them into a single map without the overhead of merging
// them, and provides a Get() function that gives precedence to attributes from
// more specific scopes (datapoint > scope > resource).
type Attributes struct {
	resource  pcommon.Map
	scope     pcommon.Map
	datapoint pcommon.Map
}

// NewAttributes creates a new Attributes instance.
func NewAttributes(resource, scope, datapoint pcommon.Map) *Attributes {
	return &Attributes{
		resource:  resource,
		scope:     scope,
		datapoint: datapoint,
	}
}

// Get retrieves the attribute value associated with the given name along with
// a boolean indicating whether the named attribute exists.
func (attrs *Attributes) Get(name string) (string, bool) {
	value, exists := attrs.getValue(name)
	if !exists || value.Type() != pcommon.ValueTypeStr {
		return "", false
	}
	return value.Str(), true
}

func (attrs *Attributes) getValue(name string) (pcommon.Value, bool) {
	if v, exists := attrs.datapoint.Get(name); exists {
		return v, true
	}
	if v, exists := attrs.scope.Get(name); exists {
		return v, true
	}
	if v, exists := attrs.resource.Get(name); exists {
		return v, true
	}
	return pcommon.NewValueEmpty(), false
}

// metricDatapointMatchesFilter returns true if (typeMatches AND (nameMatches OR labelMatches)).
//   - typeMatches is true if filter.MetricTypes is unspecified or if the metric
//     type matches any of the listed filter.MetricTypes
//   - nameMatches is true if the metric name matches any of the listed
//     filter.MetricNames or if it matches filter.MetricRegex
//   - labelMatches is true if any metric label matches any of the listed filter.Labels
//   - a metric label matches a filter Label if the names are equal AND
//   - the filter Label.Values and Label.ValueRegex are both unspecified OR
//   - the metric label value matches any of the listed Label.Values or if it
//     matches the Label.ValueRegex
func metricDatapointMatchesFilter(
	metric pmetric.Metric,
	attrs *Attributes,
	filter *MetricFilter,
) bool {
	if filter.MetricTypes != nil {
		found := false
		metricType := metricTypeToString(metric.Type())
		for _, t := range filter.MetricTypes {
			if strings.EqualFold(metricType, t) {
				found = true
				break
			}
		}
		if !found {
			return false
		}
		if filter.MetricNames == nil && filter.MetricRegex == "" && filter.Labels == nil {
			return true
		}
	}

	if filter.MetricNames != nil {
		for _, name := range filter.MetricNames {
			if metric.Name() == name {
				return true
			}
		}
	}

	if filter.MetricRegex != "" {
		matched, err := regexp.MatchString(filter.MetricRegex, metric.Name())
		if err == nil && matched {
			return true
		}
	}

	if filter.Labels != nil {
		for _, labelFilter := range filter.Labels {
			value, exists := attrs.Get(labelFilter.Name)
			if !exists {
				continue
			}

			if labelFilter.Values == nil && labelFilter.ValueRegex == "" {
				return true
			}

			if labelFilter.Values != nil {
				for _, v := range labelFilter.Values {
					if value == v {
						return true
					}
				}
			}

			if labelFilter.ValueRegex != "" {
				matched, err := regexp.MatchString(labelFilter.ValueRegex, value)
				if err == nil && matched {
					return true
				}
			}
		}
	}

	return false
}

func includeMetricDatapoint(
	grouping *MetricGrouping,
	metric pmetric.Metric,
	attrs *Attributes,
) bool {
	includeMatches := grouping.Include == nil ||
		metricDatapointMatchesFilter(metric, attrs, grouping.Include)
	excludeMatches := grouping.Exclude != nil &&
		metricDatapointMatchesFilter(metric, attrs, grouping.Exclude)

	return includeMatches && !excludeMatches
}

func metricTypeToString(metricType pmetric.MetricType) string {
	switch metricType {
	case pmetric.MetricTypeGauge:
		return "Gauge"
	case pmetric.MetricTypeSum:
		return "Counter"
	case pmetric.MetricTypeHistogram:
		return "Histogram"
	case pmetric.MetricTypeSummary:
		return "Summary"
	default:
		return "Unknown"
	}
}

// The format of the generated metric key is
// grouping:__name=<metricName>:__type=<metricType>[:<labelName>=<labelValue>...]
func generateMetricKey(
	grouping *MetricGrouping,
	metric pmetric.Metric,
	attrs *Attributes,
) string {
	var keyParts []string

	keyParts = append(keyParts, grouping.Name)

	if grouping.ByMetricName {
		keyParts = append(keyParts, fmt.Sprintf("__name=%s", metric.Name()))
	}

	if grouping.ByMetricType {
		keyParts = append(keyParts, fmt.Sprintf("__type=%s",
			metricTypeToString(metric.Type())))
	}

	if grouping.ByLabel != nil {
		for _, labelName := range grouping.ByLabel.Names {
			if labelValue, exists := attrs.Get(labelName); exists {
				keyParts = append(keyParts, fmt.Sprintf("%s=%s",
					labelName, labelValue))
			}
		}
	}

	return strings.Join(keyParts, ":")
}

// The format of the generated log key is
// grouping[:<labelName>=<labelValue>...]
func generateLogKey(grouping LogGrouping, attrs *Attributes) string {
	var keyParts []string

	keyParts = append(keyParts, grouping.Name)

	if grouping.ByLabel != nil {
		for _, labelName := range grouping.ByLabel.Names {
			if labelValue, exists := attrs.Get(labelName); exists {
				keyParts = append(keyParts, fmt.Sprintf("%s=%s",
					labelName, labelValue))
			}
		}
	}

	return strings.Join(keyParts, ":")
}

func copyCounts(counts map[string]count) map[string]count {
	copiedCounts := make(map[string]count, len(counts))
	for key, count := range counts {
		copiedCounts[key] = count
	}
	return copiedCounts
}

func totalCounts(counts map[string]count) int64 {
	var total int64
	for _, count := range counts {
		total += count.value
	}
	return total
}
