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

use std::cmp::min;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, de};

const DEFAULT_PAGE_RECORD_LIMIT: usize = 50;
const MAX_PAGE_RECORD_LIMIT: usize = 100;

/// Serde deserialization decorator to map empty Strings to None.
pub(super) fn empty_string_as_none<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: fmt::Display,
{
    let opt = Option::<String>::deserialize(de)?;
    match opt.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => FromStr::from_str(s).map_err(de::Error::custom).map(Some),
    }
}

#[derive(Deserialize, Debug, Default)]
pub(super) struct PaginationParams {
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub(super) limit: Option<usize>,
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub(super) current_page: Option<usize>,
}

pub(super) struct PaginationInfo {
    current_page: usize,
    limit: usize,
    total_items: usize,
}

impl PaginationInfo {
    fn pages(&self) -> usize {
        if self.limit == 0 {
            if self.total_items == 0 { 0 } else { 1 }
        } else {
            self.total_items.div_ceil(self.limit)
        }
    }

    fn previous(&self) -> usize {
        self.current_page.saturating_sub(1)
    }

    fn next(&self) -> usize {
        self.current_page.saturating_add(1)
    }

    fn page_range_start(&self) -> usize {
        self.current_page.saturating_sub(3)
    }

    fn page_range_end(&self) -> usize {
        min(self.current_page.saturating_add(4), self.pages())
    }
}

/// Shared pagination context for Askama templates. Embeds `PaginationInfo` and
/// adds the URL path and extra query parameters needed to render page links.
pub(super) struct PageContext {
    info: PaginationInfo,
    pub(super) path: String,
    pub(super) extra_query_params: String,
}

impl PageContext {
    pub(super) fn new(info: PaginationInfo, path: impl Into<String>) -> Self {
        Self {
            info,
            path: path.into(),
            extra_query_params: String::new(),
        }
    }

    /// Create a PageContext from a pre-computed page count (for handlers that
    /// perform database-level pagination and don't know the total item count).
    pub(super) fn from_page_count(
        current_page: usize,
        limit: usize,
        pages: usize,
        path: impl Into<String>,
    ) -> Self {
        Self {
            info: PaginationInfo {
                current_page,
                limit,
                total_items: pages.saturating_mul(limit),
            },
            path: path.into(),
            extra_query_params: String::new(),
        }
    }

    /// Create a PageContext representing the "show all" view: a single page
    /// holding every item. Used by handlers that opt into the `limit=0` "All"
    /// pagination option and already know the total item count.
    pub(super) fn all(total_items: usize, path: impl Into<String>) -> Self {
        Self {
            info: PaginationInfo {
                current_page: 0,
                limit: 0,
                total_items,
            },
            path: path.into(),
            extra_query_params: String::new(),
        }
    }

    pub(super) fn with_extra_params(mut self, extra: String) -> Self {
        self.extra_query_params = extra;
        self
    }

    pub(super) fn current_page(&self) -> usize {
        self.info.current_page
    }

    pub(super) fn limit(&self) -> usize {
        self.info.limit
    }

    pub(super) fn total_items(&self) -> usize {
        self.info.total_items
    }

    pub(super) fn pages(&self) -> usize {
        self.info.pages()
    }

    pub(super) fn previous(&self) -> usize {
        self.info.previous()
    }

    pub(super) fn next(&self) -> usize {
        self.info.next()
    }

    pub(super) fn page_range_start(&self) -> usize {
        self.info.page_range_start()
    }

    pub(super) fn page_range_end(&self) -> usize {
        self.info.page_range_end()
    }
}

/// Resolve raw pagination params into a concrete `current_page` and `limit`.
fn resolve_params(params: &PaginationParams) -> (usize, usize) {
    let current_page = params.current_page.unwrap_or(0);
    let limit = params
        .limit
        .map_or(DEFAULT_PAGE_RECORD_LIMIT, |l| min(l, MAX_PAGE_RECORD_LIMIT));
    (current_page, limit)
}

/// Paginate an already-collected Vec (e.g. after in-memory filtering).
/// Drains elements outside the page window so only the current page remains.
pub(super) fn paginate_vec<T>(
    items: Vec<T>,
    params: &PaginationParams,
) -> (PaginationInfo, Vec<T>) {
    let (current_page, limit) = resolve_params(params);
    let total_items = items.len();
    let info = PaginationInfo {
        current_page,
        limit,
        total_items,
    };

    if limit == 0 {
        return (info, items);
    }

    let offset = current_page.saturating_mul(limit);
    if offset >= total_items {
        return (info, vec![]);
    }

    let page_items: Vec<T> = items.into_iter().skip(offset).take(limit).collect();
    (info, page_items)
}

#[cfg(test)]
mod tests {
    use askama::Template;
    use axum::extract::Query;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};
    use http::Uri;

    use super::*;

    #[derive(Debug, PartialEq)]
    struct PageResult {
        current_page: usize,
        limit: usize,
        total_items: usize,
        pages: usize,
        items: Vec<i32>,
    }

    #[derive(Debug, PartialEq)]
    struct NavigationResult {
        pages: usize,
        previous: usize,
        next: usize,
        range_start: usize,
        range_end: usize,
    }

    #[derive(Debug, PartialEq)]
    struct ContextResult {
        current_page: usize,
        limit: usize,
        total_items: usize,
        path: String,
        extra_query_params: String,
    }

    #[test]
    fn paginate_vec_cases() {
        value_scenarios!(run = |(items, params)| {
            let (info, page) = paginate_vec(items, &params);
            PageResult {
                current_page: info.current_page,
                limit: info.limit,
                total_items: info.total_items,
                pages: info.pages(),
                items: page,
            }
        };
            "bounded pages" {
                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(0),
                        limit: Some(1),
                    },
                ) => PageResult {
                    current_page: 0,
                    limit: 1,
                    total_items: 5,
                    pages: 5,
                    items: vec![0],
                },

                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(1),
                        limit: Some(1),
                    },
                ) => PageResult {
                    current_page: 1,
                    limit: 1,
                    total_items: 5,
                    pages: 5,
                    items: vec![1],
                },

                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(4),
                        limit: Some(1),
                    },
                ) => PageResult {
                    current_page: 4,
                    limit: 1,
                    total_items: 5,
                    pages: 5,
                    items: vec![4],
                },

                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(10),
                        limit: Some(1),
                    },
                ) => PageResult {
                    current_page: 10,
                    limit: 1,
                    total_items: 5,
                    pages: 5,
                    items: vec![],
                },
            }

            "new boundaries" {
                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(1),
                        limit: Some(3),
                    },
                ) => PageResult {
                    current_page: 1,
                    limit: 3,
                    total_items: 5,
                    pages: 2,
                    items: vec![3, 4],
                },

                (
                    (0..150).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(0),
                        limit: Some(MAX_PAGE_RECORD_LIMIT + 1),
                    },
                ) => PageResult {
                    current_page: 0,
                    limit: MAX_PAGE_RECORD_LIMIT,
                    total_items: 150,
                    pages: 2,
                    items: (0..100).collect::<Vec<i32>>(),
                },

                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: Some(usize::MAX),
                        limit: Some(2),
                    },
                ) => PageResult {
                    current_page: usize::MAX,
                    limit: 2,
                    total_items: 5,
                    pages: 3,
                    items: vec![],
                },
            }

            "unbounded and default limits" {
                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: None,
                        limit: Some(0),
                    },
                ) => PageResult {
                    current_page: 0,
                    limit: 0,
                    total_items: 5,
                    pages: 1,
                    items: (0..5).collect::<Vec<i32>>(),
                },

                (
                    (0..5).collect::<Vec<i32>>(),
                    PaginationParams {
                        current_page: None,
                        limit: None,
                    },
                ) => PageResult {
                    current_page: 0,
                    limit: DEFAULT_PAGE_RECORD_LIMIT,
                    total_items: 5,
                    pages: 1,
                    items: (0..5).collect::<Vec<i32>>(),
                },
            }

            "empty collection" {
                (
                    vec![],
                    PaginationParams {
                        current_page: None,
                        limit: None,
                    },
                ) => PageResult {
                    current_page: 0,
                    limit: DEFAULT_PAGE_RECORD_LIMIT,
                    total_items: 0,
                    pages: 0,
                    items: vec![],
                },
            }
        );
    }

    #[test]
    fn pagination_info_helpers() {
        value_scenarios!(run = |info: PaginationInfo| NavigationResult {
            pages: info.pages(),
            previous: info.previous(),
            next: info.next(),
            range_start: info.page_range_start(),
            range_end: info.page_range_end(),
        };
            "empty and unbounded collections" {
                PaginationInfo {
                    current_page: 0,
                    limit: 0,
                    total_items: 0,
                } => NavigationResult {
                    pages: 0,
                    previous: 0,
                    next: 1,
                    range_start: 0,
                    range_end: 0,
                },

                PaginationInfo {
                    current_page: 0,
                    limit: 0,
                    total_items: 5,
                } => NavigationResult {
                    pages: 1,
                    previous: 0,
                    next: 1,
                    range_start: 0,
                    range_end: 1,
                },
            }

            "partial and bounded ranges" {
                PaginationInfo {
                    current_page: 0,
                    limit: 2,
                    total_items: 5,
                } => NavigationResult {
                    pages: 3,
                    previous: 0,
                    next: 1,
                    range_start: 0,
                    range_end: 3,
                },

                PaginationInfo {
                    current_page: 4,
                    limit: 2,
                    total_items: 10,
                } => NavigationResult {
                    pages: 5,
                    previous: 3,
                    next: 5,
                    range_start: 1,
                    range_end: 5,
                },
            }

            "saturating navigation" {
                PaginationInfo {
                    current_page: usize::MAX,
                    limit: 1,
                    total_items: usize::MAX,
                } => NavigationResult {
                    pages: usize::MAX,
                    previous: usize::MAX - 1,
                    next: usize::MAX,
                    range_start: usize::MAX - 3,
                    range_end: usize::MAX,
                },
            }
        );
    }

    #[test]
    fn page_context_constructors_and_accessors() {
        value_scenarios!(run = |context: PageContext| ContextResult {
            current_page: context.current_page(),
            limit: context.limit(),
            total_items: context.total_items(),
            path: context.path,
            extra_query_params: context.extra_query_params,
        };
            "direct context" {
                PageContext::new(
                    PaginationInfo {
                        current_page: 4,
                        limit: 2,
                        total_items: 10,
                    },
                    "/machines",
                )
                .with_extra_params("state=ready".to_string()) => ContextResult {
                    current_page: 4,
                    limit: 2,
                    total_items: 10,
                    path: "/machines".to_string(),
                    extra_query_params: "state=ready".to_string(),
                },
            }

            "precomputed page count" {
                PageContext::from_page_count(2, 25, 4, "/switches") => ContextResult {
                    current_page: 2,
                    limit: 25,
                    total_items: 100,
                    path: "/switches".to_string(),
                    extra_query_params: String::new(),
                },
            }

            "show all" {
                PageContext::all(7, "/domains") => ContextResult {
                    current_page: 0,
                    limit: 0,
                    total_items: 7,
                    path: "/domains".to_string(),
                    extra_query_params: String::new(),
                },
            }
        );
    }

    #[test]
    fn page_context_navigation_delegates_to_info() {
        let info = PaginationInfo {
            current_page: 4,
            limit: 2,
            total_items: 10,
        };
        let expected = NavigationResult {
            pages: info.pages(),
            previous: info.previous(),
            next: info.next(),
            range_start: info.page_range_start(),
            range_end: info.page_range_end(),
        };
        let context = PageContext::new(info, "/machines");

        assert_eq!(
            NavigationResult {
                pages: context.pages(),
                previous: context.previous(),
                next: context.next(),
                range_start: context.page_range_start(),
                range_end: context.page_range_end(),
            },
            expected,
        );
    }

    #[derive(Template)]
    #[template(path = "pages_footer.html")]
    struct PaginationFooter {
        page: PageContext,
    }

    #[test]
    fn pagination_footer_renders_ordinary_and_maximum_pages() {
        value_scenarios!(run = |current_page| {
            let info = PaginationInfo {
                current_page,
                limit: 25,
                total_items: 26,
            };
            let html = PaginationFooter {
                page: PageContext::new(info, "/admin/dpu/versions"),
            }
            .render()
            .expect("footer writes to a String");

            html.lines()
                .map(str::trim)
                .find(|line| line.contains("page-status"))
                .map(str::to_owned)
        };
            "ordinary first page" {
                0 => Some(r#"<span class="page-status">Page 1 of 2</span>"#.to_string()),
            }

            "maximum page index" {
                usize::MAX => Some(format!(r#"<span class="page-status">Page {} of 2</span>"#, usize::MAX)),
            }
        );
    }

    #[test]
    fn pagination_query_deserialization() {
        scenarios!(run = |raw_uri: &str| {
            let uri = raw_uri.parse::<Uri>().expect("test URI must be valid");
            Query::<PaginationParams>::try_from_uri(&uri)
                .map(|Query(params)| (params.limit, params.current_page))
                .map_err(drop)
        };
            "missing and empty values" {
                "/machines" => Yields((None, None)),
                "/machines?limit=&current_page=" => Yields((None, None)),
            }

            "numeric values" {
                "/machines?limit=25&current_page=3" => Yields((Some(25), Some(3))),
            }

            "invalid values" {
                "/machines?limit=many" => Fails,
                "/machines?current_page=-1" => Fails,
            }
        );
    }

    #[test]
    fn pagination_params_accept_null_values() {
        let params: PaginationParams =
            serde_json::from_str(r#"{"limit":null,"current_page":null}"#)
                .expect("null pagination fields must deserialize");

        assert_eq!(params.limit, None);
        assert_eq!(params.current_page, None);
    }
}
