// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"time"
)

const defaultSlackTimeout = 5 * time.Second

type SlackClient struct {
	webHookUrl string
	timeOut    time.Duration
}

type SlackMessage struct {
	IconEmoji   string       `json:"icon_emoji,omitempty"`
	Text        string       `json:"text,omitempty"`
	Attachments []Attachment `json:"attachments,omitempty"`
}

type Attachment struct {
	Color         string `json:"color,omitempty"`
	Fallback      string `json:"fallback,omitempty"`
	CallbackID    string `json:"callback_id,omitempty"`
	ID            int    `json:"id,omitempty"`
	AuthorID      string `json:"author_id,omitempty"`
	AuthorName    string `json:"author_name,omitempty"`
	AuthorSubname string `json:"author_subname,omitempty"`
	AuthorLink    string `json:"author_link,omitempty"`
	AuthorIcon    string `json:"author_icon,omitempty"`
	Title         string `json:"title,omitempty"`
	TitleLink     string `json:"title_link,omitempty"`
	Pretext       string `json:"pretext,omitempty"`
	Text          string `json:"text,omitempty"`
	ImageURL      string `json:"image_url,omitempty"`
	ThumbURL      string `json:"thumb_url,omitempty"`
	// Fields and actions are not defined.
	MarkdownIn []string    `json:"mrkdwn_in,omitempty"`
	Ts         json.Number `json:"ts,omitempty"`
}

// SendSlackNotification will post to the Slack webhook URL
func (sc SlackClient) SendSlackNotification(sm SlackMessage) error {
	return sc.sendHttpRequest(sm)
}

func (sc SlackClient) sendHttpRequest(slackRequest SlackMessage) error {
	slackBody, err := json.Marshal(slackRequest)
	if err != nil {
		return err
	}

	req, err := http.NewRequest(http.MethodPost, sc.webHookUrl, bytes.NewBuffer(slackBody))
	if err != nil {
		return err
	}
	req.Header.Add("Content-Type", "application/json")
	if sc.timeOut == 0 {
		sc.timeOut = defaultSlackTimeout
	}
	client := &http.Client{Timeout: sc.timeOut}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("non-200 response returned from Slack: %d", resp.StatusCode)
	}

	buf := new(bytes.Buffer)
	_, err = buf.ReadFrom(resp.Body)
	if err != nil {
		return err
	}
	if buf.String() != "ok" {
		return fmt.Errorf("non-ok response returned from Slack: %s", buf.String())
	}
	return nil
}

// NewSlackClient returns a new SlackClient
func NewSlackClient(webHookUrl string) SlackClient {
	return SlackClient{
		webHookUrl: webHookUrl,
	}
}
