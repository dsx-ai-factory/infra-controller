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

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use sqlx::{
    Database, Describe, Either, Execute, PgConnection, PgExecutor, PgPool, Postgres, SqlStr,
};

/// A trait describing a database handle intended for use in read-only database operations.
///
/// A DbReader can be implemented by:
/// - `&mut PgConnection`
/// - `&mut PgTransaction`
/// - `&mut PgPoolReader`
/// - `&PgPool`
///
/// To make a database function accept a DbReader, you have a few options:
///
/// # Short-cut: "Leaf" functions that run a single query
///
/// For database functions that directly execute exactly one SQL query, or which delegate to exactly
/// one function that does the same, your function can accept a simple `impl DbReader<'_>`:
///
/// ```
/// use db::db_read::DbReader;
/// async fn simple_db_func(db: impl DbReader<'_>) -> sqlx::Result<String> {
///     sqlx::query_scalar("SELECT 'test'").fetch_one(db).await
/// }
/// ```
///
/// Note, that if you accept an `impl DbReader`, then callers need to re-borrow in order to avoid
/// moves (`&mut *txn` or `txn.as_mut()`)
///
/// # More complex: Functions that run multiple queries or delegate to other functions
///
/// An issue with using `impl DbReader<'_>` is that passing it anywhere will move it out, and you
/// cannot use it again. If you need to pass a DbReader to more than one query, you'll need to use a
/// `&mut` reference, and make your function generic, with
/// [HRTB](https://doc.rust-lang.org/nomicon/hrtb.html), and use `&mut *db` to avoid moving the
/// value:
///
/// ```
/// use db::db_read::DbReader;
/// async fn two_db_calls<DB>(db: &mut DB) -> sqlx::Result<()>
/// where
///     for<'db> &'db mut DB: DbReader<'db>,
/// {
///     // Use `&mut *db` to avoid moving
///     sqlx::query_scalar::<_, String>("SELECT 'test'")
///         .fetch_one(&mut *db)
///         .await?;
///     sqlx::query_scalar::<_, String>("SELECT 'test'")
///         .fetch_one(db)
///         .await?;
///     Ok(())
/// }
/// ```
///
/// The `for<'db> &'db mut DB: DbReader<'db>` tells rust "I'm taking a `&mut` reference to
/// something, and the type of that something needs to be such that any `&mut` reference to it
/// implements the `DbReader` trait." This is because it's reference itself that implements
/// `DbReader`, not the thing it points to.
///
/// # Calling DbReader functions from writer functions
///
/// One important use case is calling "read-only" database functions when you're holding a
/// PgTransaction or PgConnection.
///
/// - [`&mut PgTransaction`] A PgTransaction can be passed via `.as_mut()` to turn it into a
///   `&mut PgConnection` first, which implements DbReader.
/// - [`&mut PgConnection`] This can be passed unaltered to a function expecting a DbReader, as
///   PgConnection implements the trait directly.
///
/// # Calling DbReader functions with PgPoolReader or a PgPool
///
/// Due to sqlx's trait limitations, a `&mut PgPool` cannot implement DbReader, only a
/// `&PgPool` can. So we have to choose if you want our db functions to take a generic `&mut DB` or a
/// generic `&DB`. But the former is usable by PgTransaction, PgConnection, and PgPoolReader, but a
/// `&DB` is only usable by PgPool.
///
/// So the guidance is to always accept a `&mut DB`, and if you want to call it from a PgPool, wrap
/// the PgPool in a PgPoolReader first.
///
/// # Complete example
///
/// ```
/// use db::db_read::PgPoolReader;
///
/// mod db_funcs {
///     use db::db_read::DbReader;
///
///     // This is callable from any of our db types: PgConnection, PgTransaction, PgPool, and
///     // PgPoolReader, and is simple to write. But the limitation is that you can only use it
///     // once, since it is "moved out" the first time it is used.
///     pub async fn callable_from_everything(db: impl DbReader<'_>) -> sqlx::Result<String> {
///         sqlx::query_scalar("SELECT 'test'").fetch_one(db).await
///     }
///
///     // This is callable from any `&mut` type: PgConnection, PgTransaction, and PgPoolReader. But
///     // the downside is that you have to type out the generics and HRTB lines:
///     pub async fn callable_from_all_but_pgpool<DB>(db: &mut DB) -> sqlx::Result<String>
///     where
///         for<'db> &'db mut DB: DbReader<'db>,
///     {
///         sqlx::query_scalar("SELECT 'test'").fetch_one(db).await
///     }
///
///     /// This is a not-recommended example: It's callable by PgPool, but not PgTransaction,
///     /// PgConnection, nor PgPoolReader.
///     pub async fn callable_from_pgpool_only<DB>(db: &DB) -> sqlx::Result<String>
///     where
///         for<'db> &'db DB: DbReader<'db>,
///     {
///         sqlx::query_scalar("SELECT 'test'").fetch_one(db).await
///     }
/// }
///
/// async fn has_a_transaction(txn: &mut sqlx::PgTransaction<'_>) -> sqlx::Result<()> {
///     // Transactions always must be passed with `.as_mut()` to convert them to a PgConnection
///     db_funcs::callable_from_everything(txn.as_mut()).await?;
///     db_funcs::callable_from_all_but_pgpool(txn.as_mut()).await?;
///     db_funcs::callable_from_all_but_pgpool(txn.as_mut()).await?;
///     Ok(())
/// }
///
/// async fn has_a_pg_connection(conn: &mut sqlx::PgConnection) -> sqlx::Result<()> {
///     // Passing to `impl DbReader<'_>` functions means doing `&mut *` to avoid moving
///     db_funcs::callable_from_everything(&mut *conn).await?;
///
///     // When passing to functions taking `&mut DB` with HRTB, you can just pass the reference
///     // as-is, and don't need to re-borrow.
///     db_funcs::callable_from_all_but_pgpool(conn).await?;
///     db_funcs::callable_from_all_but_pgpool(conn).await?;
///     Ok(())
/// }
///
/// async fn has_a_pg_pool_reader(pool: &mut PgPoolReader) -> sqlx::Result<()> {
///     // Passing to `impl DbReader<'_>` functions means doing `&mut *` to avoid moving
///     db_funcs::callable_from_everything(&mut *pool).await?;
///
///     // When passing to functions taking `&mut DB` with HRTB, you can just pass the reference
///     // as-is, and don't need to re-borrow.
///     db_funcs::callable_from_all_but_pgpool(pool).await?;
///     db_funcs::callable_from_all_but_pgpool(pool).await?;
///     Ok(())
/// }
///
/// async fn has_a_pg_pool(pool: &sqlx::PgPool) -> sqlx::Result<()> {
///     // Passing to `impl DbReader<'_>` functions means doing `&*` to avoid moving
///     db_funcs::callable_from_everything(&*pool).await?;
///
///     // When passing to functions taking `&DB` with HRTB, you can just pass the reference as-is,
///     // and don't need to re-borrow.
///     db_funcs::callable_from_pgpool_only(pool).await?;
///     db_funcs::callable_from_pgpool_only(pool).await?;
///     Ok(())
/// }
/// ```
///
/// # Why go through the effort?
///
/// For cases where you're only reading data, in order to avoid long-running work while holding open
/// a PgConnection or PgTransaction, it is beneficial to pass a reference to the *database pool*
/// around, rather than a connection or transaction. This avoids consuming database resources until
/// you actually need to execute a query. Passing a PgConnection or PgTransaction around should be
/// reserved for cases where we know we need to *write* to the database, where we need to ensure
/// that it is rolled back on failure, and that multiple writes are committed atomically.
///
/// But there's an ergonomics issue with passing a PgPool around: You still need to actually acquire
/// a connection to call database functions, or create a transaction and immediately commit it. This
/// gets confusing, because it's not clear from reading the code whether the transaction is being
/// created because we're actually writing to the database, or if we're just making a transaction so
/// that we can pass a live PgConnection to a db function.
///
/// # Why not have db functions accept PgPool?
///
/// If a function accepts a PgPool, then it can *not* be called from a context where we really *do*
/// want to hold a transaction. If we want to write to data, then in the same transaction read back
/// the data we just wrote to, the PgPool would check out a new connection to perform the read, and
/// not see the data that was just written. This means we'd have to have two of every relevant
/// function: One which takes a PgPool, and one which takes a PgConnection/PgTransaction.
///
/// # Why not accept a sqlx::Executor trait instead?
///
/// The reason is complicated and comes down to the choices sqlx made for their trait.
/// [`sqlx::Executor`] is sqlx's way of having functions be agnostic to whether you call it from a
/// Connection/Transaction or via a Pool. But it has all the same drawbacks as above: If you want
/// to use it more than once, you need to take a `&mut` reference and use generics and HRTB's. But
/// it's worse, because if you do take a `&mut` reference, then you *don't* support PgPool, since
/// PgPool only implements Executor if it's an *immutable* reference.
///
/// In addition, the DbReader trait gives us the control to separate read-only functions from
/// writable ones. This allows cases where we intentionally don't want to be able to write to the
/// database, by only putting a PgPoolReader in scope instead of a PgPool, preventing any code paths
/// from writing to the database at all.
pub trait DbReader<'txn>: PgExecutor<'txn> {}

/// A database handle that can only be sent to database functions accepting a `DbReader` trait. It
/// cannot be passed to functions expecting a PgConnection or PgTransaction.
#[derive(Debug, Clone)]
pub struct PgPoolReader {
    inner: PgPool,
}

impl From<PgPool> for PgPoolReader {
    fn from(pool: PgPool) -> Self {
        Self { inner: pool }
    }
}

impl<'a> DbReader<'a> for &'a mut PgPoolReader {}
impl<'c> DbReader<'c> for &'c mut PgConnection {}
impl<'c, 'txn> DbReader<'c> for &'c mut crate::Transaction<'txn> {}
impl<'c> DbReader<'c> for &'c PgPool {}
impl AsMut<PgPoolReader> for PgPoolReader {
    fn as_mut(&mut self) -> &mut PgPoolReader {
        self
    }
}

impl<'c> sqlx::Executor<'c> for &'c mut PgPoolReader {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<
        'e,
        Result<
            Either<<Self::Database as Database>::QueryResult, <Self::Database as Database>::Row>,
            sqlx::Error,
        >,
    >
    where
        'c: 'e,
        E: 'q + Execute<'q, Self::Database>,
    {
        self.inner.fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<<Self::Database as Database>::Row>, sqlx::Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Self::Database>,
    {
        self.inner.fetch_optional(query)
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [<Self::Database as Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Self::Database as Database>::Statement, sqlx::Error>>
    where
        'c: 'e,
    {
        self.inner.prepare_with(sql, parameters)
    }

    fn describe<'e>(
        self,
        sql: SqlStr,
    ) -> BoxFuture<'e, Result<Describe<Self::Database>, sqlx::Error>>
    where
        'c: 'e,
    {
        self.inner.describe(sql)
    }
}

impl<'c, 'txn> sqlx::Executor<'c> for &'c mut crate::Transaction<'txn> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxStream<
        'e,
        Result<
            Either<<Self::Database as Database>::QueryResult, <Self::Database as Database>::Row>,
            sqlx::Error,
        >,
    >
    where
        'c: 'e,
        E: 'q + Execute<'q, Self::Database>,
    {
        self.inner.fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> BoxFuture<'e, Result<Option<<Self::Database as Database>::Row>, sqlx::Error>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Self::Database>,
    {
        self.inner.fetch_optional(query)
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [<Self::Database as Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Self::Database as Database>::Statement, sqlx::Error>>
    where
        'c: 'e,
    {
        self.inner.prepare_with(sql, parameters)
    }

    fn describe<'e>(
        self,
        sql: SqlStr,
    ) -> BoxFuture<'e, Result<Describe<Self::Database>, sqlx::Error>>
    where
        'c: 'e,
    {
        self.inner.describe(sql)
    }
}
