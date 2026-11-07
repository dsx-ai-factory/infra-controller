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

package fileresourceprocessor

import (
	"bufio"
	"context"
        "fmt"
	"os"
	"strings"
	"sync"
	"time"

	"go.opentelemetry.io/collector/component"
	"go.opentelemetry.io/collector/pdata/pcommon"
	"go.uber.org/zap"
)

type fileResourceProcessor struct {
	config           *Config
	logger           *zap.Logger
        unreadFiles      map[string]struct{}
        attributesRWLock sync.RWMutex
	attributes       map[string]string
	ctx              context.Context
	cancel           context.CancelFunc
}

func newProcessor(cfg component.Config, logger *zap.Logger) (*fileResourceProcessor, error) {
	pCfg := cfg.(*Config)
	ctx, cancel := context.WithCancel(context.Background())
	p := &fileResourceProcessor{
		config:      pCfg,
		logger:      logger,
                unreadFiles: make(map[string]struct{}),
                attributes:  make(map[string]string),
		ctx:         ctx,
		cancel:      cancel,
	}

        for _, path := range p.config.FilePaths {
            p.unreadFiles[path] = struct{}{}
        }

	go p.pollFiles()

	return p, nil
}

func (p *fileResourceProcessor) pollFiles() {
	ticker := time.NewTicker(p.config.PollInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
                        for path := range p.unreadFiles {
                                // Continue without complaint while a file doesn't exist
                                if err := p.readFile(path); err == nil {
                                        p.logger.Info(fmt.Sprintf("Stop polling %s after successful read", path))
                                        delete(p.unreadFiles, path)
                                } else if !os.IsNotExist(err) {
                                        p.logger.Error("Failed to read file", zap.Error(err))
                                }
                        }
                        if len(p.unreadFiles) == 0 {
                                p.logger.Info("All files successfully read, stop polling")
                                return
                        }
		case <-p.ctx.Done():
			p.logger.Info("Stop polling due to context cancellation")
			return
		}
	}
}

func (p *fileResourceProcessor) cleanup() {
	p.cancel() // stop polling
}

func (p *fileResourceProcessor) readFile(path string) error {
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := scanner.Text()
		parts := strings.SplitN(line, "=", 2)
		if len(parts) == 2 {
                        name := strings.TrimSpace(parts[0])
                        value := strings.TrimSpace(parts[1])
                        if name != "" && value != "" {
                                p.attributesRWLock.Lock()
                                p.attributes[name] = value
                                p.attributesRWLock.Unlock()
                                // only reads the first name=value line
                                return nil
                        }
		}
	}

        if err := scanner.Err(); err != nil {
                return err
        }
        return fmt.Errorf("no valid key=value pair found in %s", path)
}

// processResource copies all attributes from the processor to the resource
// (assumed to be a small number), overwriting any existing attributes with the
// same names.
func (p *fileResourceProcessor) processResource(resource pcommon.Resource) {
        p.attributesRWLock.RLock()
        defer p.attributesRWLock.RUnlock()

        for name, value := range p.attributes {
                resource.Attributes().PutStr(name, value)
        }
}
