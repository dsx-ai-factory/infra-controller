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

//! XML element views and edits that preserve untouched source text.

use std::ops::Range;

use eyre::WrapErr;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};

/// Read-only element view. Parsing and byte offsets stay behind this helper;
/// consumers select direct children and request edits without streaming state.
pub(super) struct XmlElement<'a> {
    xml: &'a str,
    start: BytesStart<'a>,
    full: Range<usize>,
    content: Option<Range<usize>>,
}

impl<'a> XmlElement<'a> {
    pub(super) fn is_named(&self, name: &str) -> bool {
        self.start.name().as_ref() == name.as_bytes()
    }

    pub(super) fn children(&self) -> eyre::Result<Vec<Self>> {
        let range = self.content.clone().unwrap_or(self.full.end..self.full.end);
        Self::read_children(self.xml, range)
    }

    pub(super) fn child(&self, name: &str) -> eyre::Result<Self> {
        self.optional_child(name)?
            .ok_or_else(|| eyre::eyre!("XML must contain one {name} element"))
    }

    pub(super) fn optional_child(&self, name: &str) -> eyre::Result<Option<Self>> {
        let mut children = self
            .children()?
            .into_iter()
            .filter(|child| child.is_named(name));
        let child = children.next();
        eyre::ensure!(
            children.next().is_none(),
            "XML contains multiple {name} elements"
        );
        Ok(child)
    }

    pub(super) fn as_xml(&self) -> &'a str {
        &self.xml[self.full.clone()]
    }

    pub(super) fn replace_children(&self, name: &str, replacement: &str) -> eyre::Result<XmlEdit> {
        let content = self.content.clone().unwrap_or(self.full.end..self.full.end);
        let mut cursor = content.start;
        let mut updated = String::new();
        for child in self
            .children()?
            .into_iter()
            .filter(|child| child.is_named(name))
        {
            updated.push_str(&self.xml[cursor..child.full.start]);
            cursor = child.full.end;
        }
        updated.push_str(&self.xml[cursor..content.end]);
        updated.push_str(replacement);
        match &self.content {
            Some(_) => Ok(XmlEdit {
                range: content,
                replacement: updated,
            }),
            None => Ok(XmlEdit {
                range: self.full.clone(),
                replacement: expanded_element(&self.start, &updated)?,
            }),
        }
    }

    pub(super) fn read_children(xml: &'a str, range: Range<usize>) -> eyre::Result<Vec<Self>> {
        let mut reader = Reader::from_str(&xml[range.clone()]);
        let mut children = Vec::new();
        loop {
            let from = range.start + reader.buffer_position() as usize;
            match reader.read_event().context("could not parse XML")? {
                Event::Start(start) => {
                    let content = reader
                        .read_to_end(start.name())
                        .context("could not parse XML element")?;
                    children.push(Self {
                        xml,
                        start,
                        full: from..range.start + reader.buffer_position() as usize,
                        content: Some(
                            range.start + content.start as usize
                                ..range.start + content.end as usize,
                        ),
                    });
                }
                Event::Empty(start) => children.push(Self {
                    xml,
                    start,
                    full: from..range.start + reader.buffer_position() as usize,
                    content: None,
                }),
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(children)
    }
}

/// An owned edit plan which can be applied after the element views are dropped.
pub(super) struct XmlEdit {
    range: Range<usize>,
    replacement: String,
}

impl XmlEdit {
    pub(super) fn apply(self, xml: String) -> String {
        let mut xml = xml;
        xml.replace_range(self.range, &self.replacement);
        xml
    }
}

fn expanded_element(start: &BytesStart<'_>, content: &str) -> eyre::Result<String> {
    let mut writer = Writer::new(Vec::new());
    writer.write_event(Event::Start(start.clone()))?;
    writer.get_mut().extend_from_slice(content.as_bytes());
    writer.write_event(Event::End(start.to_end()))?;
    String::from_utf8(writer.into_inner()).context("generated XML element is invalid UTF-8")
}
