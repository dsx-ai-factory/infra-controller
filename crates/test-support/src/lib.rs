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

//! Tiny shared test helpers: table-driven tests, and a query counter for
//! pinning database round-trip counts (see [`query_counter`]).
//!
//! Write a test as a list of labeled cases — each a `scenario`, an `input`, and an
//! `expect`ed result — then run them all through one operation, written once.
//! [`check_cases`] does the running and the asserting, [`Outcome`] is what a
//! fallible operation should produce, and [`Case`] is one row. For concise
//! grouped tables, use [`scenarios!`] or [`value_scenarios!`].
//!
//! # A table of cases
//!
//! ```
//! use carbide_test_support::Outcome::*;
//! use carbide_test_support::{Case, check_cases};
//!
//! check_cases(
//!     [
//!         Case {
//!             scenario: "valid",
//!             input: "42",
//!             expect: Yields(42),
//!         },
//!         Case {
//!             scenario: "not numeric",
//!             input: "x",
//!             expect: Fails,
//!         },
//!         Case {
//!             scenario: "out of range",
//!             input: "999",
//!             expect: FailsWith("bad byte: 999".to_string()),
//!         },
//!     ],
//!     // the operation under test, written once:
//!     |s| s.parse::<u8>().map_err(|_| format!("bad byte: {s}")),
//! );
//! ```
//!
//! # Grouped scenarios
//!
//! When several rows belong to the same named condition, [`scenarios!`] keeps the
//! inputs and expected outcomes visible while removing the repeated [`Case`]
//! fields. Pass the operation under test as a `run =` closure -- the form most
//! tables use, because the same syntax also takes an inline expression or a row
//! that destructures several inputs:
//!
//! ```
//! use carbide_test_support::Outcome::*;
//! use carbide_test_support::scenarios;
//!
//! fn parse_byte(input: &str) -> Result<u8, String> {
//!     input.parse::<u8>().map_err(|_| format!("bad byte: {input}"))
//! }
//!
//! scenarios!(run = |input| parse_byte(input);
//!     "valid bytes" {
//!         "0" => Yields(0),
//!         "42" => Yields(42),
//!     }
//!
//!     "invalid bytes" {
//!         "999" => FailsWith("bad byte: 999".to_string()),
//!         "x" => Fails,
//!     }
//! );
//! ```
//!
//! For a bare named function, the `fn:` shorthand reads a little cleaner --
//! `scenarios!(parse_byte: ...)` expands to exactly the same table.
//!
//! # A single case
//!
//! A one-off test is just one [`Case`]; `.check(run)` is the single-row
//! counterpart to [`check_cases`]:
//!
//! ```
//! use carbide_test_support::Case;
//! use carbide_test_support::Outcome::*;
//!
//! Case {
//!     scenario: "in range",
//!     input: "42",
//!     expect: Yields(42),
//! }
//! .check(|s| s.parse::<u8>().map_err(|_| ()));
//! ```
//!
//! # Async
//!
//! For an `async` operation, use [`check_cases_async`] (or [`Case::check_async`])
//! and `.await` it — [`Outcome`], [`Case`], and the asserting are unchanged:
//!
//! ```
//! use carbide_test_support::Outcome::*;
//! use carbide_test_support::{Case, check_cases_async};
//!
//! # async fn example() {
//! check_cases_async(
//!     [
//!         Case {
//!             scenario: "doubles",
//!             input: 2u8,
//!             expect: Yields(4),
//!         },
//!         Case {
//!             scenario: "overflows",
//!             input: 200u8,
//!             expect: Fails,
//!         },
//!     ],
//!     |n| async move { n.checked_mul(2).ok_or(()) },
//! )
//! .await;
//! # }
//! ```
//!
//! # A plain value (no `Result`)
//!
//! When the operation returns a plain value rather than a `Result` — a `bool`
//! predicate, a getter — use [`check_values`] with [`Check`]. There's no
//! [`Outcome`] to dress up; `expect` is just the value:
//!
//! ```
//! use carbide_test_support::{Check, check_values};
//!
//! check_values(
//!     [
//!         Check {
//!             scenario: "even",
//!             input: 4,
//!             expect: true,
//!         },
//!         Check {
//!             scenario: "odd",
//!             input: 7,
//!             expect: false,
//!         },
//!     ],
//!     |n| n % 2 == 0,
//! );
//! ```
//!
//! The [`value_scenarios!`] macro provides the same grouping for total
//! operations:
//!
//! ```
//! use carbide_test_support::value_scenarios;
//!
//! fn is_even(input: u8) -> bool {
//!     input % 2 == 0
//! }
//!
//! value_scenarios!(run = |n| is_even(n);
//!     "parity" {
//!         2 => true,
//!         7 => false,
//!     }
//! );
//! ```
//!
//! # Several inputs per row
//!
//! When a row carries more than one input, keep using the macro: declare a
//! *local* struct with content-named fields, use it as the row `input`, and
//! destructure it in the `run =` closure. The struct keeps each row readable; the
//! closure does the work:
//!
//! ```
//! use carbide_test_support::value_scenarios;
//!
//! struct Row {
//!     left: u8,
//!     right: u8,
//! }
//!
//! value_scenarios!(run = |Row { left, right }| left.saturating_add(right);
//!     "saturating add" {
//!         Row { left: 1, right: 2 } => 3,
//!         Row { left: 250, right: 10 } => 255,
//!     }
//! );
//! ```
//!
//! # What's shared, and what stays a convention
//!
//! [`Outcome`] is the one piece worth sharing -- every fallible test otherwise
//! re-invents the same "succeeds / fails / fails-with-a-specific-error" enum.
//! [`scenarios!`] and [`value_scenarios!`] are intentionally thin syntax over
//! [`Case`] and [`Check`]. [`assert_outcome`] is the primitive they are built on;
//! reach for it directly only when a case needs a fully hand-written body that
//! neither a macro nor [`Case::check`] can express.

pub mod query_counter;

use std::fmt::Debug;
use std::future::Future;

/// The expected result of a fallible operation under test.
///
/// Pick the most specific variant that still says what you mean: [`Fails`] when
/// you only care *that* it errors, [`FailsWith`] when the exact error is part of
/// the contract.
///
/// [`Fails`]: Outcome::Fails
/// [`FailsWith`]: Outcome::FailsWith
#[derive(Debug)]
pub enum Outcome<T, E> {
    /// Succeeds, yielding this value.
    Yields(T),
    /// Fails — the specific error doesn't matter to this case.
    Fails,
    /// Fails with exactly this error.
    FailsWith(E),
}

/// One row of a table test: a labeled `input` and its expected [`Outcome`].
///
/// For rows with more than a single input (or several expected values), prefer a
/// *local* `struct Case` whose fields are named for their contents.
pub struct Case<I, T, E> {
    /// What this row exercises; used as the failure label.
    pub scenario: &'static str,
    /// The input handed to the operation under test.
    pub input: I,
    /// What the operation should produce.
    pub expect: Outcome<T, E>,
}

impl<I, T, E> Case<I, T, E> {
    /// Run this case's `input` through `run` and assert its [`Outcome`], naming a
    /// failure by `scenario`. The single-row counterpart to [`check_cases`] — for
    /// a test that is one labeled input and one expected result.
    pub fn check(self, run: impl FnOnce(I) -> Result<T, E>)
    where
        T: PartialEq + Debug,
        E: PartialEq + Debug,
    {
        assert_outcome(run(self.input), self.expect, self.scenario);
    }

    /// Async counterpart to [`check`](Case::check): runs an `async` operation and
    /// awaits its result before asserting.
    pub async fn check_async<Fut>(self, run: impl FnOnce(I) -> Fut)
    where
        Fut: Future<Output = Result<T, E>>,
        T: PartialEq + Debug,
        E: PartialEq + Debug,
    {
        assert_outcome(run(self.input).await, self.expect, self.scenario);
    }
}

/// Run each case's `input` through `run` and assert its [`Outcome`]. `run` is the
/// operation under test, written once.
pub fn check_cases<I, T, E>(
    cases: impl IntoIterator<Item = Case<I, T, E>>,
    run: impl Fn(I) -> Result<T, E>,
) where
    T: PartialEq + Debug,
    E: PartialEq + Debug,
{
    for case in cases {
        case.check(&run);
    }
}

/// Async counterpart to [`check_cases`]: each case's `input` is run through the
/// `async` operation `run` and its result awaited. Call it from an async test
/// (e.g. `#[tokio::test]`) and `.await` it.
pub async fn check_cases_async<I, T, E, Fut>(
    cases: impl IntoIterator<Item = Case<I, T, E>>,
    run: impl Fn(I) -> Fut,
) where
    Fut: Future<Output = Result<T, E>>,
    T: PartialEq + Debug,
    E: PartialEq + Debug,
{
    for case in cases {
        case.check_async(&run).await;
    }
}

/// Assert that `got` matches the expected [`Outcome`], attributing any mismatch to
/// `scenario`. This is the primitive [`Case::check`] and [`check_cases`] are built
/// on — reach for it directly when a case needs a hand-written body.
pub fn assert_outcome<T, E>(got: Result<T, E>, expect: Outcome<T, E>, scenario: &str)
where
    T: PartialEq + Debug,
    E: PartialEq + Debug,
{
    use Outcome::*;
    match (got, expect) {
        (Ok(got), Yields(want)) => assert_eq!(got, want, "{scenario}"),
        (Err(_), Fails) => {}
        (Err(got), FailsWith(want)) => assert_eq!(got, want, "{scenario}"),
        (got, expect) => panic!("{scenario}: got {got:?}, but expected {expect:?}"),
    }
}

/// One row of a *total* table test: a labeled `input` and the plain value the
/// operation is expected to return. The counterpart to [`Case`] for an operation
/// that can't fail — a predicate, a getter, any `fn(I) -> T`.
pub struct Check<I, T> {
    /// What this row exercises; used as the failure label.
    pub scenario: &'static str,
    /// The input handed to the operation under test.
    pub input: I,
    /// The value the operation should return.
    pub expect: T,
}

impl<I, T> Check<I, T> {
    /// Run this check's `input` through `run` and assert it returns `expect`,
    /// naming a failure by `scenario`. The single-row counterpart to
    /// [`check_values`].
    pub fn check(self, run: impl FnOnce(I) -> T)
    where
        T: PartialEq + Debug,
    {
        assert_eq!(run(self.input), self.expect, "{}", self.scenario);
    }
}

/// Run each check's `input` through `run` and assert the returned value equals its
/// `expect`. The total-function counterpart to [`check_cases`]: reach for it when
/// the operation under test returns a plain value — a `bool` predicate, a getter —
/// rather than a `Result`.
pub fn check_values<I, T>(checks: impl IntoIterator<Item = Check<I, T>>, run: impl Fn(I) -> T)
where
    T: PartialEq + Debug,
{
    for check in checks {
        check.check(&run);
    }
}

/// Run grouped fallible table rows through one operation.
///
/// Each group label is combined with the row input expression in failure output,
/// so a failure points at both the broad scenario and the concrete input. This is
/// equivalent to writing [`Case`] rows and passing them to [`check_cases`].
///
/// ```
/// use carbide_test_support::Outcome::*;
/// use carbide_test_support::scenarios;
///
/// fn checked_double(input: u8) -> Result<u8, &'static str> {
///     input.checked_mul(2).ok_or("overflow")
/// }
///
/// scenarios!(checked_double:
///     "doubles" {
///         2 => Yields(4),
///     }
///
///     "overflow" {
///         200 => Fails,
///     }
/// );
/// ```
#[macro_export]
macro_rules! scenarios {
    ($run:path: $($scenario:literal { $($input:expr => $expect:expr),+ $(,)? })+ $(,)?) => {
        $crate::check_cases(
            [
                $($(
                    $crate::Case {
                        scenario: concat!($scenario, " / ", stringify!($input)),
                        input: $input,
                        expect: $expect,
                    },
                )+)+
            ],
            $run,
        )
    };
    (run = $run:expr; $($scenario:literal { $($input:expr => $expect:expr),+ $(,)? })+ $(,)?) => {
        $crate::check_cases(
            [
                $($(
                    $crate::Case {
                        scenario: concat!($scenario, " / ", stringify!($input)),
                        input: $input,
                        expect: $expect,
                    },
                )+)+
            ],
            $run,
        )
    };
}

/// Run grouped total-value table rows through one operation.
///
/// This is the total-function counterpart to [`scenarios!`], equivalent to
/// writing [`Check`] rows and passing them to [`check_values`].
///
/// ```
/// use carbide_test_support::value_scenarios;
///
/// fn double(input: u8) -> u8 {
///     input * 2
/// }
///
/// value_scenarios!(double:
///     "doubles" {
///         2 => 4,
///         9 => 18,
///     }
/// );
/// ```
#[macro_export]
macro_rules! value_scenarios {
    ($run:path: $($scenario:literal { $($input:expr => $expect:expr),+ $(,)? })+ $(,)?) => {
        $crate::check_values(
            [
                $($(
                    $crate::Check {
                        scenario: concat!($scenario, " / ", stringify!($input)),
                        input: $input,
                        expect: $expect,
                    },
                )+)+
            ],
            $run,
        )
    };
    (run = $run:expr; $($scenario:literal { $($input:expr => $expect:expr),+ $(,)? })+ $(,)?) => {
        $crate::check_values(
            [
                $($(
                    $crate::Check {
                        scenario: concat!($scenario, " / ", stringify!($input)),
                        input: $input,
                        expect: $expect,
                    },
                )+)+
            ],
            $run,
        )
    };
}

/// Install the shared test log subscriber, naming `component` in the failure message.
///
/// Every test binary that wants tracing output needs the same registry, the same
/// `TestWriter`, and the same set of noise-suppressing directives. `api-db` and `api-core`
/// each grew their own copy of it -- `api-db`'s carried a comment saying as much, and
/// declining to depend on `api-test-helper` was the reason. This crate is a leaf with no
/// such problem, so the copy lives here now and both call it.
///
/// `component` only appears in the panic text, to say which binary tripped over an
/// already-installed subscriber.
///
/// Panics if a global subscriber is already set. That's deliberate -- a test binary should
/// initialize logging in exactly one place, and swallowing this hides the second one.
pub fn setup_test_logging(component: &str) {
    use tracing::metadata::LevelFilter;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt::TestWriter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;

    if let Err(e) = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::default()
                .compact()
                .with_writer(TestWriter::new),
        )
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy()
                .add_directive("sqlx=warn".parse().unwrap())
                .add_directive("tower=warn".parse().unwrap())
                .add_directive("rustify=off".parse().unwrap())
                .add_directive("rustls=warn".parse().unwrap())
                .add_directive("hyper=warn".parse().unwrap())
                .add_directive("h2=warn".parse().unwrap())
                // Silence permissive mode related messages
                .add_directive("carbide_api_core::auth=error".parse().unwrap()),
        )
        .try_init()
    {
        panic!(
            "Failed to initialize trace logging for {component} tests. It's possible some earlier \
            code path has already set a global default log subscriber: {e}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Outcome::*;
    use super::{Case, Check, assert_outcome, check_cases, check_values};

    #[test]
    fn check_runs_a_single_case() {
        Case {
            scenario: "doubles",
            input: 21u8,
            expect: Yields(42),
        }
        .check(|n| n.checked_mul(2).ok_or(()));
    }

    #[test]
    fn check_cases_runs_every_row() {
        check_cases(
            [
                Case {
                    scenario: "doubles",
                    input: 2u8,
                    expect: Yields(4),
                },
                Case {
                    scenario: "overflows",
                    input: 200u8,
                    expect: Fails,
                },
            ],
            |n| n.checked_mul(2).ok_or("overflow"),
        );
    }

    #[test]
    fn check_values_runs_every_row() {
        check_values(
            [
                Check {
                    scenario: "even",
                    input: 4,
                    expect: true,
                },
                Check {
                    scenario: "odd",
                    input: 7,
                    expect: false,
                },
            ],
            |n| n % 2 == 0,
        );
    }

    #[test]
    fn scenarios_macro_runs_grouped_fallible_rows() {
        fn checked_double(input: u8) -> Result<u8, &'static str> {
            input.checked_mul(2).ok_or("overflow")
        }

        crate::scenarios!(checked_double:
            "doubles" {
                2 => Yields(4),
                21 => Yields(42),
            }

            "overflow" {
                200 => Fails,
            }
        );
    }

    #[test]
    fn scenarios_macro_accepts_run_expression() {
        crate::scenarios!(run = |n| n.checked_add(1).ok_or("overflow");
            "increments" {
                1u8 => Yields(2),
            }

            "overflow" {
                u8::MAX => FailsWith("overflow"),
            }
        );
    }

    #[test]
    fn value_scenarios_macro_runs_grouped_total_rows() {
        fn double(input: u8) -> u8 {
            input * 2
        }

        crate::value_scenarios!(double:
            "doubles" {
                2 => 4,
                21 => 42,
            }
        );
    }

    #[test]
    #[should_panic(expected = "label / 1")]
    fn value_scenarios_macro_labels_failures_with_group_and_input() {
        crate::value_scenarios!(run = |n| n;
            "label" {
                1 => 2,
            }
        );
    }

    #[test]
    fn check_runs_a_single_value() {
        Check {
            scenario: "doubles",
            input: 21,
            expect: 42,
        }
        .check(|n| n * 2);
    }

    #[test]
    #[should_panic(expected = "the value is wrong")]
    fn flags_an_unexpected_value() {
        Check {
            scenario: "the value is wrong",
            input: 1,
            expect: 2,
        }
        .check(|n| n);
    }

    #[test]
    fn assert_outcome_matches_each_variant() {
        assert_outcome(Ok::<_, String>(7), Yields(7), "yields the value");
        assert_outcome(Err::<u8, _>("boom".to_string()), Fails, "fails, any error");
        assert_outcome(
            Err::<u8, _>("nope".to_string()),
            FailsWith("nope".to_string()),
            "fails with the exact error",
        );
    }

    #[test]
    #[should_panic(expected = "wanted a value")]
    fn flags_an_unexpected_error() {
        assert_outcome(Err::<u8, _>("x".to_string()), Yields(1), "wanted a value");
    }

    #[test]
    #[should_panic(expected = "wanted a failure")]
    fn flags_an_unexpected_success() {
        assert_outcome(Ok::<_, String>(1), Fails, "wanted a failure");
    }

    #[test]
    #[should_panic(expected = "wanted the other error")]
    fn flags_the_wrong_error() {
        assert_outcome(
            Err::<u8, _>("a".to_string()),
            FailsWith("b".to_string()),
            "wanted the other error",
        );
    }
}
