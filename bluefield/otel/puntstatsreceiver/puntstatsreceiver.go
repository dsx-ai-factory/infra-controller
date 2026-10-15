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

package puntstatsreceiver

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
	"time"

	"go.opentelemetry.io/collector/component"
	"go.opentelemetry.io/collector/consumer"
	"go.opentelemetry.io/collector/pdata/pcommon"
	"go.opentelemetry.io/collector/pdata/pmetric"
	"go.opentelemetry.io/collector/receiver"
	"go.uber.org/zap"
)

// Old punt stats:
//
//	PUNT miss pkts:570458 bytes:44652348
//	PUNT miss drop pkts:0 bytes:0
//	PUNT control pkts:6623177 bytes:629089559
//	PUNT control drop pkts:0 bytes:0
//	ACL PUNT pkts:4 bytes:438
//	ACL drop pkts:0 bytes:0
//
// New punt stats:
//
//	catch_all pkts:4 bytes:240
//	catch_all drop pkts:0 bytes:0
//	arp pkts:36 bytes:2160
//	arp drop pkts:0 bytes:0
//	bfd pkts:0 bytes:0
//	bfd drop pkts:0 bytes:0
//	bgp pkts:1038740 bytes:92338211
//	bgp drop pkts:0 bytes:0
//	dhcp pkts:9 bytes:3255
//	dhcp drop pkts:0 bytes:0
//	ip2me pkts:13 bytes:2478
//	ip2me drop pkts:0 bytes:0
//	icmp pkts:8 bytes:626
//	icmp drop pkts:0 bytes:0
//	icmp6_neigh pkts:178078 bytes:14478132
//	icmp6_neigh drop pkts:0 bytes:0
type Format int

const (
	NullFormat     Format = iota
	OriginalFormat        // old punt stats
	ProtocolFormat        // new punt stats
)

var recognizedProtocols = map[string]struct{}{
	"catch_all":   {},
	"arp":         {},
	"bfd":         {},
	"bgp":         {},
	"dhcp":        {},
	"ip2me":       {},
	"icmp":        {},
	"icmp6_neigh": {},
}

type puntStatsReceiver struct {
	logger       *zap.Logger
	config       *Config
	nextConsumer consumer.Metrics
	cancel       context.CancelFunc
}

// receiver constructor
func newPuntStatsReceiver(
	set receiver.Settings,
	config *Config,
	next consumer.Metrics,
) (receiver.Metrics, error) {
	return &puntStatsReceiver{
		logger:       set.Logger,
		config:       config,
		nextConsumer: next,
	}, nil
}

func (r *puntStatsReceiver) Start(ctx context.Context, host component.Host) error {
	ctx, r.cancel = context.WithCancel(ctx)

	go func() {
		ticker := time.NewTicker(r.config.ScrapeInterval)
		defer ticker.Stop()

		for {
			select {
			case <-ticker.C:
				if err := r.scrapeAndSend(ctx); err != nil {
					r.logger.Warn("puntstats scrape failed", zap.Error(err))
				}
			case <-ctx.Done():
				return
			}
		}
	}()

	return nil
}

func (r *puntStatsReceiver) Shutdown(ctx context.Context) error {
	if r.cancel != nil {
		r.cancel()
	}

	return nil
}

func scrapePuntStats(
	ctx context.Context,
	filePath string,
	containerName string,
) (string, error) {
	var args []string

	if containerName == "" {
		// Read directly from DPU filesystem
		args = []string{"cat", filePath}
	} else {
		// 1. Get list of containers (as structured JSON, so the name match
		//    below is scoped to the container's name field)
		psCmd := exec.CommandContext(ctx, "crictl", "ps", "-o", "json")
		out, err := psCmd.Output()
		if err != nil {
			return "", fmt.Errorf("crictl ps failed: %w", err)
		}

		// 2. Find the container ID by exact name match
		var psResult struct {
			Containers []struct {
				ID       string `json:"id"`
				Metadata struct {
					Name string `json:"name"`
				} `json:"metadata"`
			} `json:"containers"`
		}
		if err := json.Unmarshal(out, &psResult); err != nil {
			return "", fmt.Errorf("failed to parse crictl ps output: %w", err)
		}

		var containerID string
		for _, c := range psResult.Containers {
			if c.Metadata.Name == containerName {
				containerID = c.ID
				break
			}
		}
		if containerID == "" {
			return "", fmt.Errorf("container %q not found", containerName)
		}

		// 3. Exec into the container and cat the file
		args = []string{"crictl", "exec", containerID, "cat", filePath}
	}

	cmd := exec.CommandContext(ctx, args[0], args[1:]...)

	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("command %q failed: %w: %s",
			strings.Join(args, " "), err, stderr.String())
	}

	return stdout.String(), nil
}

func parseLine(
	line string,
) (format Format, protocol string, dropped bool, pkts uint64, bytes uint64, err error) {
	fields := strings.Fields(line)
	if len(fields) < 3 {
		return NullFormat, "", false, 0, 0, fmt.Errorf("not enough fields")
	}

	format = ProtocolFormat
	protocol = fields[0]

	var pktsStr, bytesStr string

	if protocol == "PUNT" || protocol == "ACL" {
		format = OriginalFormat
		if protocol == "PUNT" && fields[1] == "miss" {
			protocol = "punt_miss"
		} else if protocol == "PUNT" && fields[1] == "control" {
			protocol = "punt_control"
		} else if protocol == "ACL" && (fields[1] == "PUNT" || fields[1] == "drop") {
			protocol = "acl"
		} else {
			// keep the label set bounded
			return NullFormat, "", false, 0, 0, fmt.Errorf("unrecognized original format: %q", line)
		}
		if fields[1] != "drop" {
			fields = fields[1:]
			if len(fields) < 3 {
				return NullFormat, "", false, 0, 0, fmt.Errorf("not enough fields (after shift)")
			}
		}
	} else if _, ok := recognizedProtocols[protocol]; !ok {
		// keep the label set bounded
		return NullFormat, "", false, 0, 0,
			fmt.Errorf("unrecognized protocol in new format: %q", protocol)
	}

	if fields[1] == "drop" {
		dropped = true
		if len(fields) < 4 {
			return NullFormat, "", false, 0, 0, fmt.Errorf("drop line missing fields")
		}
		pktsStr = strings.TrimPrefix(fields[2], "pkts:")
		bytesStr = strings.TrimPrefix(fields[3], "bytes:")
	} else {
		pktsStr = strings.TrimPrefix(fields[1], "pkts:")
		bytesStr = strings.TrimPrefix(fields[2], "bytes:")
	}

	pktsVal, err := strconv.ParseUint(pktsStr, 10, 64)
	if err != nil {
		return NullFormat, "", false, 0, 0, fmt.Errorf("bad pkts value: %w", err)
	}
	bytesVal, err := strconv.ParseUint(bytesStr, 10, 64)
	if err != nil {
		return NullFormat, "", false, 0, 0, fmt.Errorf("bad bytes value: %w", err)
	}

	return format, protocol, dropped, pktsVal, bytesVal, nil
}

func (r *puntStatsReceiver) scrapeAndSend(ctx context.Context) error {
	// Prepare metrics container.
	md := pmetric.NewMetrics()
	rm := md.ResourceMetrics().AppendEmpty()
	resAttrs := rm.Resource().Attributes()
	resAttrs.PutStr("source", sourceStr)
	sm := rm.ScopeMetrics().AppendEmpty()

	// Packets metric (monotonic sum).
	pktMetric := sm.Metrics().AppendEmpty()
	pktMetric.SetName(puntStatName("packets"))
	pktMetric.SetDescription("Total packets per protocol and drop status.")
	pktMetric.SetUnit("1")
	pktSum := pktMetric.SetEmptySum()
	pktSum.SetIsMonotonic(true)
	pktSum.SetAggregationTemporality(pmetric.AggregationTemporalityCumulative)

	// Bytes metric (monotonic sum).
	bytesMetric := sm.Metrics().AppendEmpty()
	bytesMetric.SetName(puntStatName("bytes"))
	bytesMetric.SetDescription("Total bytes per protocol and drop status.")
	bytesMetric.SetUnit("By")
	bytesSum := bytesMetric.SetEmptySum()
	bytesSum.SetIsMonotonic(true)
	bytesSum.SetAggregationTemporality(pmetric.AggregationTemporalityCumulative)

	now := pcommon.NewTimestampFromTime(time.Now())

	ctxScrape, cancel := context.WithTimeout(ctx, r.config.ScrapeInterval)
	defer cancel()

	stats, err := scrapePuntStats(ctxScrape, r.config.FilePath, r.config.ContainerName)
	if err != nil {
		return err
	}

	var prevFormat Format = NullFormat

	scanner := bufio.NewScanner(strings.NewReader(stats))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		format, proto, dropped, pkts, bytes, err := parseLine(line)
		if err != nil {
			r.logger.Debug("failed to parse line", zap.String("line", line), zap.Error(err))
			continue
		}
		if format == NullFormat {
			continue
		}
		if prevFormat != NullFormat && format != prevFormat {
			return fmt.Errorf("found inconsistent format: %q", line)
		}
		prevFormat = format

		// packets data point
		dpPkts := pktSum.DataPoints().AppendEmpty()
		dpPkts.SetTimestamp(now)
		dpPkts.SetIntValue(int64(pkts))
		dpPkts.Attributes().PutStr("protocol", proto)
		dpPkts.Attributes().PutBool("dropped", dropped)

		// bytes data point
		dpBytes := bytesSum.DataPoints().AppendEmpty()
		dpBytes.SetTimestamp(now)
		dpBytes.SetIntValue(int64(bytes))
		dpBytes.Attributes().PutStr("protocol", proto)
		dpBytes.Attributes().PutBool("dropped", dropped)
	}
	if err := scanner.Err(); err != nil {
		return err
	}

	return r.nextConsumer.ConsumeMetrics(ctx, md)
}
