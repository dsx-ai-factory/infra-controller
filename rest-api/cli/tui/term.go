// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"fmt"
	"os"

	"golang.org/x/term"
)

const (
	KeyEnter     = '\r'
	KeyNewline   = '\n'
	KeyEscape    = 27
	KeyBackspace = 127
	KeyCtrlC     = 3
	KeyCtrlD     = 4
)

type SpecialKey int

const (
	KeyNone SpecialKey = iota
	KeyUp
	KeyDown
	KeyRight
	KeyLeft
)

type KeyEvent struct {
	Char    byte
	Special SpecialKey
}

func RawMode() (restore func() error, err error) {
	fd := int(os.Stdin.Fd())
	oldState, err := term.MakeRaw(fd)
	if err != nil {
		return nil, fmt.Errorf("entering raw mode: %w", err)
	}
	return func() error {
		err := term.Restore(fd, oldState)
		if err != nil {
			return fmt.Errorf("restoring terminal: %w", err)
		}
		return nil
	}, nil
}

func ReadKey() (KeyEvent, error) {
	buf := make([]byte, 1)
	_, err := os.Stdin.Read(buf)
	if err != nil {
		return KeyEvent{}, err
	}

	if buf[0] == KeyEscape {
		seq := make([]byte, 2)
		n, err := readEscapeSequence(os.Stdin, seq)
		if err != nil || n < 2 {
			return KeyEvent{Char: KeyEscape}, nil
		}
		if seq[0] == '[' {
			switch seq[1] {
			case 'A':
				return KeyEvent{Special: KeyUp}, nil
			case 'B':
				return KeyEvent{Special: KeyDown}, nil
			case 'C':
				return KeyEvent{Special: KeyRight}, nil
			case 'D':
				return KeyEvent{Special: KeyLeft}, nil
			}
		}
		return KeyEvent{Char: KeyEscape}, nil
	}

	return KeyEvent{Char: buf[0]}, nil
}

func ClearLine() {
	fmt.Print("\033[2K\r")
}

func ClearDown() {
	fmt.Print("\033[J")
}

func MoveUp(n int) {
	if n > 0 {
		fmt.Printf("\033[%dA", n)
	}
}

func MoveDown(n int) {
	if n > 0 {
		fmt.Printf("\033[%dB", n)
	}
}

func MoveToColumn(n int) {
	fmt.Printf("\033[%dG", n)
}

func HideCursor() {
	fmt.Print("\033[?25l")
}

func ShowCursor() {
	fmt.Print("\033[?25h")
}

func Bold(s string) string {
	return "\033[1m" + s + "\033[0m"
}

func Dim(s string) string {
	return "\033[2m" + s + "\033[0m"
}

func Reverse(s string) string {
	return "\033[7m" + s + "\033[0m"
}

func Cyan(s string) string {
	return "\033[36m" + s + "\033[0m"
}

func Green(s string) string {
	return "\033[32m" + s + "\033[0m"
}

func Red(s string) string {
	return "\033[31m" + s + "\033[0m"
}

func Yellow(s string) string {
	return "\033[33m" + s + "\033[0m"
}
