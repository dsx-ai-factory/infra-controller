// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package sshclient provides SSH-based operations for NV-Switch NVOS management.
// This is used for CPLD and NVOS firmware updates that require SSH access.
package sshclient

import (
	"context"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"

	"golang.org/x/crypto/ssh"
)

// safePathRe matches paths that contain only alphanumerics, slashes,
// hyphens, underscores, and dots -- the characters safe to use in a
// shell argument without quoting.
var safePathRe = regexp.MustCompile(`^[a-zA-Z0-9/_.\-]+$`)

// NVOSClient provides SSH-based operations for NV-Switch NVOS.
type NVOSClient struct {
	nvos   *nvos.NVOS
	client *ssh.Client
}

// New creates a new NVOSClient for the given NVOS subsystem using its configured port.
func New(ctx context.Context, n *nvos.NVOS) (*NVOSClient, error) {
	return NewWithPort(ctx, n, n.GetPort())
}

// NewWithPort creates a new NVOSClient for the given NVOS subsystem with a custom port.
func NewWithPort(ctx context.Context, n *nvos.NVOS, port int) (*NVOSClient, error) {
	if n == nil {
		return nil, fmt.Errorf("NVOS is nil")
	}

	if n.Credential == nil {
		return nil, fmt.Errorf("NVOS credentials not set")
	}

	config := &ssh.ClientConfig{
		User: n.Credential.User,
		Auth: []ssh.AuthMethod{
			ssh.Password(n.Credential.Password.Value),
		},
		HostKeyCallback: ssh.InsecureIgnoreHostKey(),
		Timeout:         30 * time.Second,
	}

	addr := net.JoinHostPort(n.IP.String(), strconv.Itoa(port))
	client, err := ssh.Dial("tcp", addr, config)
	if err != nil {
		return nil, fmt.Errorf("failed to connect to NVOS at %s: %v", addr, err)
	}

	return &NVOSClient{
		nvos:   n,
		client: client,
	}, nil
}

// Close closes the SSH connection.
func (c *NVOSClient) Close() error {
	if c.client != nil {
		return c.client.Close()
	}
	return nil
}

// RunCommand executes a command on the NVOS and returns the output.
func (c *NVOSClient) RunCommand(cmd string) (string, error) {
	session, err := c.client.NewSession()
	if err != nil {
		return "", fmt.Errorf("failed to create session: %v", err)
	}
	defer session.Close()

	output, err := session.CombinedOutput(cmd)
	if err != nil {
		return string(output), fmt.Errorf("command failed: %v (output: %s)", err, string(output))
	}

	return string(output), nil
}

// CopyFile copies a local file to the remote NVOS.
func (c *NVOSClient) CopyFile(localPath, remotePath string) error {
	remotePath = filepath.Clean(remotePath)
	if !filepath.IsAbs(remotePath) {
		return fmt.Errorf("remote path must be absolute: %s", remotePath)
	}

	remoteDir := filepath.Dir(remotePath)
	remoteFileName := filepath.Base(remotePath)

	if !safePathRe.MatchString(remoteDir) {
		return fmt.Errorf("remote directory contains invalid characters: %s", remoteDir)
	}
	if !safePathRe.MatchString(remoteFileName) {
		return fmt.Errorf("remote filename contains invalid characters: %s", remoteFileName)
	}

	localFile, err := os.Open(localPath)
	if err != nil {
		return fmt.Errorf("failed to open local file: %v", err)
	}
	defer localFile.Close()

	fileInfo, err := localFile.Stat()
	if err != nil {
		return fmt.Errorf("failed to stat local file: %v", err)
	}

	session, err := c.client.NewSession()
	if err != nil {
		return fmt.Errorf("failed to create session: %v", err)
	}
	defer session.Close()

	stdin, err := session.StdinPipe()
	if err != nil {
		return fmt.Errorf("failed to get stdin pipe: %v", err)
	}

	go func() {
		defer stdin.Close()
		fmt.Fprintf(stdin, "C0644 %d %s\n", fileInfo.Size(), remoteFileName)
		io.Copy(stdin, localFile)
		fmt.Fprint(stdin, "\x00")
	}()

	err = session.Run(fmt.Sprintf("scp -t %s", remoteDir))
	if err != nil {
		return fmt.Errorf("scp failed: %v", err)
	}

	return nil
}
