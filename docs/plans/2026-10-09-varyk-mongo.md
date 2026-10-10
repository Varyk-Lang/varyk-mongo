# varyk-mongo 0.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `varyk-mongo` 0.1.0, the official MongoDB package for Varyk: `mongo::connect` to a database, typed reads and writes of documents with native BSON types, shell-style query text with `?` values at allowlisted positions, aggregation, indexes, and transactions, in its own repository with CI and release-please like varyk-sql.

**Spec:** `docs/specs/2026-10-09-varyk-mongo-design.md` ("spec §n"), the authority where this plan is brief. Read it whole before any task; §4 (the query text and its safety rules) and §2.4 (transactions) carry most of the risk.

**Depends on:** varyk-std's typed serialize forms (spec §6.1), on `Varyk-Lang/varyk` branch `fix/typed-serialize`, unreleased while this plan runs. Until varyk 0.8.1 is on crates.io the package builds with the released `varyk` 0.8.0 binary and `VARYK_STD_PATH` set to that branch's `crates/varyk-std` in a local checkout, and `Cargo.toml` says `varyk-std = "0.8"` (the checkout is numbered 0.8.0, so a `"0.8.1"` line would not match it); Task 8 moves it to `"0.8.1"` after the release. The §6.1 commit (now f825ac2 on `fix/typed-serialize`) must be in the worktree before this plan's Task 3 starts (it is); the controller pushes `fix/typed-serialize` with its pull request before Task 7 relies on CI.

## Overview

Eight tasks. Task 1 lays out the repository and connects. Task 2 writes the query-text parser with every rule of spec §4 as Rust unit tests. Task 3 writes the serializer and the deserializer (spec §3). Task 4 builds the reading and inserting calls on them, and Task 5 the rest (updates, deletes, aggregation, indexes), each tested against a real server. Task 6 adds transactions. Task 7 adds the demo, the documents, and CI. Task 8, after varyk 0.8.1 is released, moves to the released `varyk-std` and makes the release ready.

## Context

Line numbers are approximate. "varyk-sql" is a checkout of `Varyk-Lang/varyk-sql` beside this one (`../varyk-sql`), the sister package this one mirrors.

- varyk-sql `src/lib.vr` (the surface by `pub use`), `src/db.rs` (`connect_with` 28, `Pool`/`Tx` with `#[derive(Clone)]`, the `Tx` failed flag and `statement_error` ~600, `connect_error` ~1017 and the rule that no message holds the URL, the `#[cfg(test)] mod tests` that `varyk test` runs), `src/tests.vr` (the `Config`/`url()` pattern 1-19, tests as helpers returning `Result` opened with `match` and `assert`, rows keyed by test name and deleted first).
- varyk-sql `.github/workflows/`: `ci.yml` (jobs `test` and `msrv`, `cargo install varyk --version '^0.8' --locked`, rustfmt with `--edition 2024`, clippy in the `varyk publish --assemble-only` folder, the demo step), `release-please.yml` (installs varyk, runs `varyk publish` with `CARGO_REGISTRY_TOKEN` from the `release` environment, the tag check), `commit-messages.yml`, `latest-varyk.yml`; `release-please-config.json`, `.release-please-manifest.json`; `CONTRIBUTING.md`, `AGENTS.md`, `.gitignore` (`target/`, `/.superpowers/`), `LICENSE-MIT`, `LICENSE-APACHE`.
- varyk-sql 0.3 plan `docs/plans/2026-10-07-varyk-sql-0.3.md`, Global Constraints: the CI shape for an unreleased varyk (a second `actions/checkout` of `Varyk-Lang/varyk` at a branch, `VARYK_STD_PATH`, clippy with a `patch.crates-io.varyk-std.path` `--config`), and its last task that swaps it for crates.io.
- `../varyk/docs/language.md` (`Varyk-Lang/varyk`) "Calling Rust" (~2394) for the facade shapes the spec uses; "Rust structs and methods" (~2544): `&self` is `self`, `&mut self` is `mut self`, a `self`-by-value method cannot be called.
- mongodb 3.9.1 and bson 3.1.0 sources under `~/.cargo/registry/src/index.crates.io-*/` (facts already checked by the spec's reviews: `compat-3-3-0` required; session drop aborts an in-progress transaction; code-20 rewrite at `client/executor.rs` ~543; transaction read preference must be primary; empty `insert_many` refused; `ConnectionString::parse` is synchronous).

## Development Approach

- TDD per task: Rust unit tests in the `.rs` file for what needs no server (the parser, the serializer, the deserializer), Varyk `#[test]`s in `src/tests.vr` for what does; failing first, then the code, then the gate.
- The test server: a single-node replica set. Locally it is the container `varyk-mongo-test` on port 27018 (already running, member `127.0.0.1:27018`; port 27017 belongs to another container and is not touched), so `MONGO_URL=mongodb://127.0.0.1:27018/varyk_test`. Tests read `MONGO_URL` through `env::parse` into an `Option<string>` and default to `mongodb://127.0.0.1:27017/varyk_test`, CI's address (spec §9).
- Each test works in its own collection, named after the test, and deletes its documents first; tests run in parallel.
- `src/db.rs` is the only file Varyk imports items from; `text.rs`, `write.rs`, `read.rs` are private modules `db.rs` reaches as `crate::text` and so on (spec §8). Each file keeps one job.
- Rules (spec §8, varyk-sql AGENTS.md): no `unwrap`, `expect`, panicking index or slice, `unreachable!`; every failure a `varyk_std::Error`; no URL, password, or value in a message the package writes; Rust 1.88 (`rust-version`), edition 2024.
- **The gate**, from the repository root (in Tasks 1 to 4 the clippy step adds `-A dead_code`, since parts of the parser, serializer, deserializer, and the collection's handle are reached only from tests until Task 5; no allow attribute is committed; from Task 5 on the full flags): `VARYK_STD_PATH=… varyk check && VARYK_STD_PATH=… MONGO_URL=mongodb://127.0.0.1:27018/varyk_test varyk test`; `RUSTUP_TOOLCHAIN=1.88 …` the same test run (install the toolchain once); `rustfmt --edition 2024 --check src/*.rs`; in the folder `varyk publish --assemble-only` prints, `cargo clippy --all-targets --config 'patch.crates-io.varyk-std.path="…"' -- -D warnings`.
- Commits: conventional prefixes, one concern each, no `<` or `>` in subjects, no attribution lines. Work on the branch `feat/0.1`, started from `docs/varyk-mongo-spec`; never on `main`; push over HTTPS with `gh`'s credential helper; the maintainer merges.
- Not needed: cargo features of the package's own; a `serde` dependency (use `varyk_std::serde`); a cursor, change stream, GridFS, or anything else spec §11 lists; a CHANGELOG edit.

## Tasks

### Task 1: The repository, the surface, and connecting

Spec §1, §2.1, §5 (Connecting), §8.

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `LICENSE-MIT`, `LICENSE-APACHE`, `src/lib.vr`, `src/db.rs`, `src/text.rs`, `src/write.rs`, `src/read.rs`, `src/tests.vr`

- [ ] write `Cargo.toml` as spec §8 lists, with `version = "0.0.0"` (release-please's start, as varyk-sql), `varyk-std = "0.8"` until Task 8, and no `tokio` yet (Task 6 adds it); the licenses copied from varyk-sql, `.gitignore` as varyk-sql's
- [ ] write `src/lib.vr` as spec §2 shows but without `pub use db::Find;` and `pub use db::Tx;` (Tasks 4 and 6 add them with their items), and empty private `text.rs`, `write.rs`, `read.rs` with a one-line module comment each
- [ ] in `db.rs`, add `Database` and `Collection` (`#[derive(Clone)]`), `connect(url)` (parse with `ConnectionString::parse`, require a database in the path, build the client, `ping`), `db.collection(name: &'static str)`, and `db.ping()`
- [ ] map connection failures to spec §5's step messages with no URL and no driver text for a URL that cannot be read, and the `authSource=admin` hint on a refused password
- [ ] write Varyk tests: `connect` to the test server and `ping`; no database in the URL; a URL that cannot be read; an unreachable server (`serverSelectionTimeoutMS=500`); a `mongodb+srv://nonexistent.invalid/app` name; a wrong password (against the test server with a made-up user); each message without the URL
- [ ] gate: the full gate

### Task 2: The query text

Spec §4, §3.3.

**Files:**
- Modify: `src/text.rs`

- [ ] write the parser for the relaxed JSON of spec §4 straight into `bson::Document` and `bson::Bson` (never through extended JSON), with `?` and `ObjectId(?)` as placeholders taken in order from a `Vec<varyk_std::Value>`
- [ ] give it one entry point taking the text's kind (filter, update, pipeline, sort, index keys, index options), the text, the call's values, and the number of the first value it may take, and giving the BSON and how many `?`s it took, plus a counting mode giving only the number of `?`s, so `db.rs` checks the total across a call's texts against the values before filling any; it tracks query context and update-operator values while parsing; a text error names the kind and a 1-based character position, a value error the value's 1-based number across the call
- [ ] map values per spec §3.3 (`Time` to milliseconds rounding toward the past, `Uuid` to binary subtype 4, `Bytes` to subtype 0, `Int` to int64)
- [ ] enforce every rule of spec §4: `?` never a key; the count across a call's texts; the allowlist (query context, `$jsonSchema` and `$expr` excluded, the eight update operators at any depth with `$each` the only `$`-key above a `?`, top-level `$skip` and `$limit`) with its message; `None` in query context; `$where`, `$function`, `$accumulator` keys; `$regex` values escaped and a non-`Text` refused; `$options` refused; an empty text; the kind's shape (document or array); an update document's top-level keys all starting with `$`
- [ ] write unit tests for each rule and each message, and for: `'?'` as text, bare and quoted keys, single and double quotes, escapes, trailing commas, int32 and int64 and double, numbers beyond int64, `ObjectId(?)` with good and bad text counting as one `?`, a `$regex` value of `.*` escaped, `$regularExpression` read as an ordinary key
- [ ] gate: the full gate

### Task 3: Writing and reading documents

Spec §3.1, §3.2, and §6.1's contract.

**Files:**
- Modify: `src/write.rs`, `src/read.rs`

- [ ] write the serializer from a Varyk value to a `bson::Document`, reporting `is_human_readable()` as `false`, matching `varyk_std::serde_names::TIME` and `UUID` newtypes and `serialize_bytes`, with spec §3.1's type table (a struct or `HashMap<string, V>` at the top; `u64` and `usize` above `i64` an `Error` naming the field)
- [ ] apply the `_id` rules of spec §3.1: a `None` `_id` left out, `string`, integer, and `Uuid` accepted, any other kind an `Error` naming its type, a 24-hex `string` kept a string
- [ ] write the deserializer from a `bson::Document` to `T` with spec §3.2's table and rules (missing field, `null`, `#[default]`, ranges, dates as `Time`'s ISO text, subtype 4 as hyphenated text, subtype 3 an `Error`, other binary as bytes, ObjectId as hex, Decimal128 as text, a scalar `T` from a one-field document or `_id` alone)
- [ ] make the deserializer's errors ignore the message given to `custom` and the values given to `invalid_type`, `invalid_value`, and `unknown_variant`, naming only the field and the expected type (spec §5)
- [ ] write unit tests for every row of both tables, the `_id` rules, the error texts holding no value, and a round trip of a struct with every type
- [ ] gate: the full gate

### Task 4: Reading and inserting

Spec §2.2, §2.3 (`insert`, `insert_many`), §5 (server errors).

**Files:**
- Modify: `src/lib.vr`, `src/db.rs`, `src/tests.vr`

- [ ] add `pub use db::Find;` to `src/lib.vr`
- [ ] add `one`, `first`, `all`, `count` on `Collection`, and `find` giving `Find` with `&self` methods `sort`, `skip`, `limit` (copies) and the finishers `one`, `first`, `all`; negative `skip`/`limit` and `limit(0)` an `Error` at the finisher
- [ ] add `insert` (giving the `_id` as text) and `insert_many` (ids in order; an empty `Vec` gives an empty `Vec` without sending; the driver's message on a failure)
- [ ] map server errors to the server's code and message, and other driver failures to the driver's message (spec §5)
- [ ] write Varyk tests for these calls and the spec §9 bullets they own: zero and several values; `one` with no match and `first` giving `None`; `find` with `sort`, `skip`, `limit` and the refused counts; `count`; the type round trip checked with `$type` (every type of §3.1, `u64` above `i64`, nested struct, `Vec`, `HashMap`, enum); ObjectId ids made (no `_id`, a `None` one) and queried back with `ObjectId(?)`; the `_id` rules and the 24-hex string `_id`; a duplicate key giving the server's message; `insert_many`'s ids in order, stopping at a failing document, and empty; a missing field, a `null` into a plain field, a field that does not fit; stored non-Uuid text and an unknown enum name failing without the stored text; the count of `?`s against the values (a filter with one `?` and two values, then none) naming both counts; `first("{ name: { $regex: ? } }", ".*")` finding only a document whose name holds the text `.*`
- [ ] gate: the full gate, still with `-A dead_code` (the update, pipeline, and index kinds of the parser are reached only from tests until Task 5)

### Task 5: Updating, deleting, aggregating, and indexes

Spec §2.2 (`aggregate`), §2.3, §4 through real calls, §7 (create_index at startup).

**Files:**
- Modify: `src/db.rs`, `src/tests.vr`

- [ ] add `update`, `update_many` (matched counts), `upsert`, `replace` (by the document's own `_id`), `delete`, `delete_many`, `aggregate`, and `create_index` with its fixed option list
- [ ] write Varyk tests for these calls and the spec §9 bullets they own: `upsert` inserting then updating; `replace` without an `_id`; `aggregate` into a struct and into a scalar; the allowlist and `None` rules through real calls (filter, `$match`, `$pull`, `$set` at depth, `$push`'s `$each`, `$skip`/`$limit` taking a `?`; `?` under `$expr`, in `$addFields`, in `$lookup`'s `from` and its pipeline's `$match`, in a `$search` stage, under `$jsonSchema`, under `$rename`, in an update given as stages, refused; `"$secret"` staying text in a filter, a `$match`, and under `$set`; a filter on a field named `from`; a `None` in a filter, a `$pull` condition, and a `$match` an `Error`, in `$set` null); `$text`'s `$search` taken; a `?` in a sort and in a `create_index` text refused; `create_index` twice and an unknown option; the read-only conversions (an update pipeline with `$toDouble` and `$toDecimal`, then a whole double read into an integer field, a fractional one an `Error`, and the decimal into a `string`); the count of `?`s across an update's two texts
- [ ] gate: the full gate with the full clippy flags (no `-A dead_code` from here on)

### Task 6: Transactions

Spec §2.4.

**Files:**
- Modify: `src/lib.vr`, `src/db.rs`, `Cargo.toml` (tokio `sync`), `src/tests.vr`

- [ ] add `pub use db::Tx;` to `src/lib.vr`
- [ ] add `Tx` with `db.begin()` (session, transaction with a primary read preference), `tx.collection(name)` giving a `Collection` bound to shared state behind tokio's mutex that holds the session and the `failed` and `finished` flags (a `Collection` cannot reach its `Tx`, so the flags live beside the session), and `tx.commit()` (`mut self`, setting `finished` there)
- [ ] apply the failure rule: a driver failure inside fails the `Tx` with the driver's or server's message and "; the transaction will roll back" or the temporary-conflict ending; later calls give "a call in this transaction failed; it will roll back" without reaching the server; `commit` on a failed `Tx` aborts, ignoring the abort's result, and gives "the transaction was rolled back: a call in it failed"; the package's own errors leave the `Tx` as it was
- [ ] give the unknown-commit-result failure its message, the standalone code-20 failure its message (matched on the code with either text), and a finished `Tx` "this transaction is finished"
- [ ] write Varyk tests per spec §9: commit; drop then a plain insert of the same `_id` succeeding at once; failed then commit; commit twice; a call on a failed `Tx`'s collection giving "a call in this transaction failed; it will roll back"; a call on a committed `Tx`; an update document with a key not starting with `$` inside a `Tx` leaving it usable; a package `Error` inside (`one` with no match) leaving it usable; the temporary-conflict message from two `Tx`s on one document; `first` inside under `readPreference=secondaryPreferred`
- [ ] check the standalone message by hand against a throwaway standalone `mongo:8` container on another free port (record the command and result in the report)
- [ ] gate: the full gate

### Task 7: The demo, the documents, and CI

Spec §7, §8, §9, §10.

**Files:**
- Create: `demo/users/Cargo.toml`, `demo/users/src/main.vr`, `README.md`, `CONTRIBUTING.md`, `AGENTS.md`, `.github/workflows/ci.yml`, `.github/workflows/release-please.yml`, `.github/workflows/commit-messages.yml`, `.github/workflows/latest-varyk.yml`, `release-please-config.json`, `.release-please-manifest.json`

- [ ] write the demo as spec §1's program (with its `delete_many` first), a path dependency on the package and `varyk-std = "0.8"` (as varyk-sql's demo), printing a fixed line CI can compare
- [ ] write the README with everything spec §9 lists (install with `varyk add varyk-mongo --rename mongo`, the example, configuration, TLS, the type table, the query text and its rules, transactions and the retry loop using `contains`, the development replica set, testing, the version table, the line on MongoDB, Inc., Rust 1.88), and every item of spec §7
- [ ] write CONTRIBUTING.md and AGENTS.md following varyk-sql's, with spec §8's rules and the replica-set test setup
- [ ] write CI as spec §9: jobs `test` (stable) and `msrv` (1.88), each starting `mongo:8` with `--replSet rs0`, initiating it, waiting for `isWritablePrimary`; until Task 8 also checking out `Varyk-Lang/varyk` at `fix/typed-serialize` with `VARYK_STD_PATH` as varyk-sql 0.3's plan did; `test` also runs rustfmt, clippy in the assembled crate, and the demo with `MONGO_URL`
- [ ] copy `release-please.yml`, `commit-messages.yml`, and `latest-varyk.yml` from varyk-sql (the latter starting the replica set too), and the release-please config and manifest at `0.0.0`
- [ ] gate: the full gate, plus the demo run against the local server
- [ ] rebase `feat/0.1` onto `origin/main` (the maintainer's empty first commit; ask for it if `main` has none), push, and open the pull request; CI runs on pull requests, red until `fix/typed-serialize` is pushed to `Varyk-Lang/varyk`

### Task 8: The released varyk-std (after varyk 0.8.1 is on crates.io)

Spec §7 (Versions), §10.

**Files:**
- Modify: `Cargo.toml`, `demo/users/Cargo.toml`, `.github/workflows/ci.yml`, `.github/workflows/release-please.yml`, `README.md`, `CONTRIBUTING.md`

- [ ] move `varyk-std` to `"0.8.1"` in both manifests
- [ ] make CI and the release job install `varyk` `^0.8.1` from crates.io and drop the checkout of `Varyk-Lang/varyk`, `VARYK_STD_PATH`, and clippy's `--config` patch
- [ ] check the README's version table and install lines and CONTRIBUTING's install line
- [ ] gate: the full gate without `VARYK_STD_PATH`; push and see CI green
