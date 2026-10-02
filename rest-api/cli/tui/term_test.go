// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build aix || darwin || dragonfly || freebsd || linux || netbsd || openbsd || solaris

package tui

import (
	"bytes"
	"os"
	"testing"
	"time"

	"github.com/creack/pty"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRawMode_ReturnsRestoreError(t *testing.T) {
	terminal, tty, err := pty.Open()
	require.NoError(t, err)
	t.Cleanup(func() {
		_ = terminal.Close()
		_ = tty.Close()
	})

	originalStdin := os.Stdin
	os.Stdin = tty
	t.Cleanup(func() {
		os.Stdin = originalStdin
	})

	restore, err := RawMode()
	require.NoError(t, err)
	require.NoError(t, tty.Close())

	err = restore()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "restoring terminal")
}

func TestSelectCancellationPreservesSentinel(t *testing.T) {
	terminal, tty, err := pty.Open()
	require.NoError(t, err)
	t.Cleanup(func() {
		_ = terminal.Close()
		_ = tty.Close()
	})

	originalStdin := os.Stdin
	originalStdout := os.Stdout
	os.Stdin = tty
	os.Stdout = tty
	t.Cleanup(func() {
		os.Stdin = originalStdin
		os.Stdout = originalStdout
	})

	result := make(chan error, 1)
	go func() {
		_, err := Select("Cancel", []SelectItem{{Label: "item", ID: "item"}})
		result <- err
	}()

	require.NoError(t, terminal.SetReadDeadline(time.Now().Add(time.Second)))
	output := make([]byte, 0, 256)
	buffer := make([]byte, 256)
	for !bytes.Contains(output, []byte("Cancel")) {
		n, readErr := terminal.Read(buffer)
		require.NoError(t, readErr)
		output = append(output, buffer[:n]...)
	}
	require.NoError(t, terminal.SetReadDeadline(time.Time{}))

	_, err = terminal.Write([]byte{KeyCtrlD})
	require.NoError(t, err)

	select {
	case err := <-result:
		assert.Same(t, errSelectionCancelled, err)
	case <-time.After(time.Second):
		t.Fatal("Select did not return after Ctrl+D")
	}
}

func TestReadKey_LoneEscapeReturnsWithoutWaitingForMoreInput(t *testing.T) {
	reader, writer, err := os.Pipe()
	require.NoError(t, err)
	t.Cleanup(func() {
		_ = reader.Close()
		_ = writer.Close()
	})

	originalStdin := os.Stdin
	os.Stdin = reader
	t.Cleanup(func() {
		os.Stdin = originalStdin
	})

	result := make(chan KeyEvent, 1)
	readErr := make(chan error, 1)
	go func() {
		key, err := ReadKey()
		if err != nil {
			readErr <- err
			return
		}
		result <- key
	}()

	_, err = writer.Write([]byte{KeyEscape})
	require.NoError(t, err)

	select {
	case key := <-result:
		assert.Equal(t, KeyEvent{Char: KeyEscape}, key)
	case err := <-readErr:
		require.NoError(t, err)
	case <-time.After(500 * time.Millisecond):
		_ = writer.Close()
		t.Fatal("lone Escape did not return before the timeout")
	}
}

func TestReadKey_StillRecognizesArrowEscapeSequences(t *testing.T) {
	reader, writer, err := os.Pipe()
	require.NoError(t, err)
	t.Cleanup(func() {
		_ = reader.Close()
		_ = writer.Close()
	})

	originalStdin := os.Stdin
	os.Stdin = reader
	t.Cleanup(func() {
		os.Stdin = originalStdin
	})

	_, err = writer.Write([]byte{KeyEscape, '[', 'A'})
	require.NoError(t, err)

	key, err := ReadKey()
	require.NoError(t, err)
	assert.Equal(t, KeyEvent{Special: KeyUp}, key)
}
