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

//! The `$filter` expressions collections serve: the OData subset DSP0266
//! asks of a service that advertises `FilterQuery`.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// A parsed `$filter`: comparisons of a member's properties against
/// literals, combined with `and`, `or`, `not`, and parentheses. A property
/// is a `/`-separated path into the member, `Status/Health` or
/// `Links/OriginOfCondition/@odata.id`; a literal is a `'quoted string'`,
/// a number, `true`, `false`, `null`, or an RFC 3339 instant, quoted or
/// bare.
#[derive(Debug)]
pub(super) enum Filter {
    Compare {
        path: Vec<String>,
        operator: Operator,
        literal: Literal,
    },
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
    Not(Box<Filter>),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Operator {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug)]
pub(super) enum Literal {
    Str(String),
    Number(f64),
    Bool(bool),
    Null,
    /// Any literal that reads as an RFC 3339 instant, quoted or not, so the
    /// parse happens once here rather than once per member.
    Instant(DateTime<Utc>),
}

/// Why an expression is not a [`Filter`]. Carries property paths and token
/// positions, never the expression or a literal from it, so it can be logged
/// as is: a filter's literals are the client's data.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum FilterError {
    #[error("a string literal is not terminated")]
    UnterminatedString,
    #[error("a `(` is not closed")]
    UnbalancedOpen,
    #[error("token {at} is a `)` that closes nothing")]
    UnbalancedClose { at: usize },
    #[error("token {at} follows a complete expression")]
    TrailingToken { at: usize },
    #[error("expected a property name at token {at}")]
    ExpectedProperty { at: usize },
    #[error("`{property}` is not a property path")]
    InvalidPropertyPath { property: String },
    #[error("expected one of eq, ne, gt, ge, lt, le after `{property}`")]
    ExpectedOperator { property: String },
    #[error("expected a literal to compare `{property}` with")]
    ExpectedLiteral { property: String },
    #[error(
        "the literal compared with `{property}` is not a quoted string, number, boolean, null, or RFC 3339 instant"
    )]
    InvalidLiteral { property: String },
}

impl Filter {
    pub(super) fn parse(expression: &str) -> Result<Self, FilterError> {
        let tokens = tokenize(expression)?;
        let mut parser = Parser {
            tokens: &tokens,
            at: 0,
        };
        let filter = parser.or()?;
        match parser.tokens.get(parser.at) {
            None => Ok(filter),
            Some(Token::Close) => Err(FilterError::UnbalancedClose { at: parser.at }),
            Some(_) => Err(FilterError::TrailingToken { at: parser.at }),
        }
    }

    /// Whether `resource` satisfies this filter. A property the resource
    /// lacks compares as `null`.
    pub(super) fn admits(&self, resource: &Value) -> bool {
        match self {
            Filter::Compare {
                path,
                operator,
                literal,
            } => {
                let actual = path.iter().fold(resource, |value, segment| {
                    value.get(segment).unwrap_or(&Value::Null)
                });
                operator.holds(actual, literal)
            }
            Filter::And(left, right) => left.admits(resource) && right.admits(resource),
            Filter::Or(left, right) => left.admits(resource) || right.admits(resource),
            Filter::Not(inner) => !inner.admits(resource),
        }
    }
}

impl Operator {
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "eq" => Self::Eq,
            "ne" => Self::Ne,
            "gt" => Self::Gt,
            "ge" => Self::Ge,
            "lt" => Self::Lt,
            "le" => Self::Le,
            _ => return None,
        })
    }

    fn holds(self, actual: &Value, literal: &Literal) -> bool {
        let ordering = compare(actual, literal);
        match self {
            Self::Eq => ordering == Some(Ordering::Equal),
            Self::Ne => ordering != Some(Ordering::Equal),
            Self::Gt => ordering == Some(Ordering::Greater),
            Self::Ge => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
            Self::Lt => ordering == Some(Ordering::Less),
            Self::Le => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
        }
    }
}

/// How `actual` orders against `literal`, or `None` when the two are not
/// comparable, which only `ne` counts as satisfied. A string property
/// compares as an instant or a number when the literal is one and the
/// property reads as one: Redfish types `Id` and `Created` as strings, and
/// clients compare them against the integer and the instant they hold.
fn compare(actual: &Value, literal: &Literal) -> Option<Ordering> {
    match (actual, literal) {
        (Value::String(actual), Literal::Str(literal)) => {
            Some(actual.as_str().cmp(literal.as_str()))
        }
        (Value::String(actual), Literal::Instant(literal)) => {
            instant(actual).map(|actual| actual.cmp(literal))
        }
        (Value::String(actual), Literal::Number(literal)) => number(actual)?.partial_cmp(literal),
        (Value::Number(actual), Literal::Number(literal)) => actual.as_f64()?.partial_cmp(literal),
        (Value::Number(actual), Literal::Str(literal)) => {
            actual.as_f64()?.partial_cmp(&number(literal)?)
        }
        (Value::Bool(actual), Literal::Bool(literal)) => Some(actual.cmp(literal)),
        (Value::Null, Literal::Null) => Some(Ordering::Equal),
        _ => None,
    }
}

/// `text` as an RFC 3339 instant. A `+` in the offset that a client left
/// unencoded arrives as a space once the query is decoded, and no instant
/// contains a space of its own, so one is read back as the `+`.
fn instant(text: &str) -> Option<DateTime<Utc>> {
    let text = text.replacen(' ', "+", 1);
    DateTime::parse_from_rfc3339(&text)
        .ok()
        .map(|instant| instant.with_timezone(&Utc))
}

fn number(text: &str) -> Option<f64> {
    text.parse::<f64>().ok().filter(|number| number.is_finite())
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Open,
    Close,
    Quoted(String),
    Word(String),
}

fn tokenize(expression: &str) -> Result<Vec<Token>, FilterError> {
    let mut tokens = Vec::new();
    let mut chars = expression.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            '\'' => {
                let mut text = String::new();
                loop {
                    match chars.next() {
                        // OData doubles a quote to include one in a string.
                        Some('\'') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            text.push('\'');
                        }
                        Some('\'') => break,
                        Some(c) => text.push(c),
                        None => return Err(FilterError::UnterminatedString),
                    }
                }
                tokens.push(Token::Quoted(text));
            }
            c => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '(' | ')' | '\'') {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                tokens.push(Token::Word(word));
            }
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
}

impl Parser<'_> {
    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn take_word(&mut self, word: &str) -> bool {
        if self.tokens.get(self.at) == Some(&Token::Word(word.to_owned())) {
            self.at += 1;
            return true;
        }
        false
    }

    fn or(&mut self) -> Result<Filter, FilterError> {
        let mut left = self.and()?;
        while self.take_word("or") {
            let right = self.and()?;
            left = Filter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Filter, FilterError> {
        let mut left = self.unary()?;
        while self.take_word("and") {
            let right = self.unary()?;
            left = Filter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Filter, FilterError> {
        let at = self.at;
        match self.next() {
            Some(Token::Word(word)) if word == "not" => Ok(Filter::Not(Box::new(self.unary()?))),
            Some(Token::Open) => {
                let inner = self.or()?;
                match self.next() {
                    Some(Token::Close) => Ok(inner),
                    _ => Err(FilterError::UnbalancedOpen),
                }
            }
            Some(Token::Word(property)) => self.comparison(property),
            _ => Err(FilterError::ExpectedProperty { at }),
        }
    }

    fn comparison(&mut self, property: String) -> Result<Filter, FilterError> {
        let path: Vec<String> = property.split('/').map(str::to_owned).collect();
        if path.iter().any(String::is_empty) {
            return Err(FilterError::InvalidPropertyPath { property });
        }
        let operator = match self.next() {
            Some(Token::Word(word)) => Operator::parse(&word),
            _ => None,
        }
        .ok_or_else(|| FilterError::ExpectedOperator {
            property: property.clone(),
        })?;
        let literal = self.literal(&property)?;
        Ok(Filter::Compare {
            path,
            operator,
            literal,
        })
    }

    fn literal(&mut self, property: &str) -> Result<Literal, FilterError> {
        match self.next() {
            Some(Token::Quoted(text)) => {
                Ok(instant(&text).map_or(Literal::Str(text), Literal::Instant))
            }
            Some(Token::Word(word)) => {
                // An unquoted instant whose offset `+` decoded to a space
                // arrives as two words.
                if let Some(Token::Word(offset)) = self.tokens.get(self.at)
                    && let Some(rejoined) = instant(&format!("{word}+{offset}"))
                {
                    self.at += 1;
                    return Ok(Literal::Instant(rejoined));
                }
                word_literal(&word).ok_or_else(|| FilterError::InvalidLiteral {
                    property: property.to_owned(),
                })
            }
            _ => Err(FilterError::ExpectedLiteral {
                property: property.to_owned(),
            }),
        }
    }
}

fn word_literal(word: &str) -> Option<Literal> {
    Some(match word {
        "true" => Literal::Bool(true),
        "false" => Literal::Bool(false),
        "null" => Literal::Null,
        _ => {
            if let Some(number) = number(word) {
                Literal::Number(number)
            } else {
                Literal::Instant(instant(word)?)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use serde_json::json;

    use super::*;

    fn admits(expression: &str, resource: &Value) -> Result<bool, FilterError> {
        Filter::parse(expression).map(|filter| filter.admits(resource))
    }

    #[test]
    fn comparisons_follow_the_property_type() {
        let entry = json!({
            "Id": "12",
            "Created": "2026-02-12T02:06:58+00:00",
            "Severity": "OK",
            "Count": 3,
            "Resolved": false,
            "Links": {"OriginOfCondition": {"@odata.id": "/redfish/v1/Systems/S"}},
        });
        check_values(
            [
                Check {
                    scenario: "quoted instant in another offset orders as an instant",
                    input: "Created gt '2026-02-11T18:06:57-08:00'",
                    expect: true,
                },
                Check {
                    scenario: "bare instant, inclusive bound",
                    input: "Created ge 2026-02-12T02:06:58Z",
                    expect: true,
                },
                Check {
                    scenario: "bare instant whose `+` decoded to a space",
                    input: "Created le 2026-02-12T02:06:58 00:00",
                    expect: true,
                },
                Check {
                    scenario: "quoted instant whose `+` decoded to a space is not lexically after itself",
                    input: "Created gt '2026-02-12T02:06:58 00:00'",
                    expect: false,
                },
                Check {
                    scenario: "a string Id compares numerically against an integer",
                    input: "Id gt 9",
                    expect: true,
                },
                Check {
                    scenario: "strings compare lexically",
                    input: "Severity eq 'OK'",
                    expect: true,
                },
                Check {
                    scenario: "numbers compare numerically",
                    input: "Count lt 10",
                    expect: true,
                },
                Check {
                    scenario: "a number compares against a quoted number",
                    input: "Count eq '3'",
                    expect: true,
                },
                Check {
                    scenario: "booleans",
                    input: "Resolved eq false",
                    expect: true,
                },
                Check {
                    scenario: "nested path",
                    input: "Links/OriginOfCondition/@odata.id eq '/redfish/v1/Systems/S'",
                    expect: true,
                },
                Check {
                    scenario: "an absent property is null",
                    input: "MessageId eq null",
                    expect: true,
                },
                Check {
                    scenario: "mismatched types satisfy only ne",
                    input: "Severity ne 3 and not Severity gt 3",
                    expect: true,
                },
                Check {
                    scenario: "or, and, parentheses, not",
                    input: "(Severity eq 'Critical' or Count eq 3) and not Resolved eq true",
                    expect: true,
                },
                Check {
                    scenario: "and binds tighter than or",
                    input: "Severity eq 'Critical' and Count eq 3 or Resolved eq false",
                    expect: true,
                },
                Check {
                    scenario: "a doubled quote is one quote",
                    input: "Severity eq 'O''K'",
                    expect: false,
                },
            ],
            |expression| admits(expression, &entry).unwrap(),
        );
    }

    #[test]
    fn malformed_expressions_are_rejected_without_repeating_them() {
        let property = |property: &str| property.to_owned();
        check_values(
            [
                Check {
                    scenario: "empty",
                    input: "",
                    expect: FilterError::ExpectedProperty { at: 0 },
                },
                Check {
                    scenario: "property alone",
                    input: "Created",
                    expect: FilterError::ExpectedOperator {
                        property: property("Created"),
                    },
                },
                Check {
                    scenario: "operator without a literal",
                    input: "Created ge",
                    expect: FilterError::ExpectedLiteral {
                        property: property("Created"),
                    },
                },
                Check {
                    scenario: "unknown operator",
                    input: "Created between 1 and 2",
                    expect: FilterError::ExpectedOperator {
                        property: property("Created"),
                    },
                },
                Check {
                    scenario: "a bare word that is no literal",
                    input: "Created ge secret-value",
                    expect: FilterError::InvalidLiteral {
                        property: property("Created"),
                    },
                },
                Check {
                    scenario: "unterminated string",
                    input: "Created ge 'secret",
                    expect: FilterError::UnterminatedString,
                },
                Check {
                    scenario: "unclosed parenthesis",
                    input: "(Created ge 2026-02-12T02:06:58Z",
                    expect: FilterError::UnbalancedOpen,
                },
                Check {
                    scenario: "stray parenthesis",
                    input: "Created ge 2026-02-12T02:06:58Z)",
                    expect: FilterError::UnbalancedClose { at: 3 },
                },
                Check {
                    scenario: "two expressions without a connective",
                    input: "Created ge 2026-02-12T02:06:58Z Severity eq 'secret'",
                    expect: FilterError::TrailingToken { at: 3 },
                },
                Check {
                    scenario: "empty path segment",
                    input: "Created/ ge 1",
                    expect: FilterError::InvalidPropertyPath {
                        property: property("Created/"),
                    },
                },
            ],
            |expression| {
                let error = Filter::parse(expression).unwrap_err();
                assert!(
                    !error.to_string().contains("secret"),
                    "{error} repeats a literal"
                );
                error
            },
        );
    }
}
