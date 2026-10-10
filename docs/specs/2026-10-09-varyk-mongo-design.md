# varyk-mongo: the MongoDB package

Date: 2026-10-09.

**Status.** Implemented in 0.1.0. `varyk-mongo` is the official
Varyk package for MongoDB, written in Varyk with a facade over the
official `mongodb` crate. It needs a small change in
`Varyk-Lang/varyk` (section 6.1), released as varyk 0.8.1 before this
package's 0.1.0; the `varyk add mongo` shorthand (section 6.2) follows
0.1.0. Varyk is experimental and pre-1.0: anything here may
change. The package is not affiliated with or endorsed by MongoDB, Inc.

## 1. Summary and scope

A program adds the package with `varyk add varyk-mongo --rename mongo`
(`varyk add mongo` once the varyk release after 0.1.0 has the
shorthand, section 6.2), and then

```varyk
struct Config {
    mongo_url: string,
}

struct User {
    #[rename("_id")]
    id: Uuid,
    email: string,
    name: string,
    created_at: Time,
}

async fn run() -> Result<bool, Error> {
    let config: Config = env::parse()?;
    let db = mongo::connect(config.mongo_url).await?;
    let users = db.collection("users");
    users.create_index("{ email: 1 }", "{ unique: true }").await?;

    users.delete_many("{ email: ? }", "ada@example.com").await?;   // a rerun starts clean
    let ada = User { id: Uuid::new(), email: "ada@example.com", name: "Ada", created_at: Time::now() };
    users.insert(ada).await?;

    let found: Option<User> = users.first("{ email: ? }", "ada@example.com").await?;
    let recent: Vec<User> = users.find("{ created_at: { $gte: ? } }", ada.created_at)
        .sort("{ created_at: -1 }").limit(20).all().await?;
    users.update("{ _id: ? }", "{ $set: { name: ? } }", ada.id, "Ada L.").await?;
    let gone: u64 = users.delete("{ _id: ? }", ada.id).await?;
    println!("{} {} {}", found.is_some(), recent.len(), gone);
    Ok(true)
}

async fn main() -> Result<bool, Error> {
    run().await
}
```

is a working program against a local server, a replica set, or Atlas,
chosen by the URL (`MONGO_URL=mongodb://127.0.0.1:27017/app`). The
package aims at what an ordinary production service does with MongoDB
and nothing more: connect over TLS, read and write typed documents,
filter with Mongo's own operators, aggregate, index, run transactions,
and test against a real server.

### 1.1 Decisions

- **Name `varyk-mongo`, key `mongo`.** Official names leave out the
  crate underneath (`docs/design.md`); `mongo` is what the roadmap
  reserved and what Go and Ruby call their drivers. The README's title
  and the crate description say "MongoDB".
- **Query text in the shell's syntax, values beside it.** A filter,
  update, pipeline, sort, or index is a literal (`&'static str`, M5b3
  §2.3) in the relaxed JSON of Mongo's docs, Compass, and the shell,
  with `?` where a value goes; the values follow as trailing arguments
  (M5b3 §2.2). This is varyk-sql's guard: input never becomes query
  text, so no value can become an operator (`{"$ne": null}`). A typed
  builder (`mongo::eq(..)`) was rejected as verbose and never complete;
  strict JSON was rejected because Varyk has no raw strings, so every
  key would need `\"`. Rust's `doc!` with values written inline needs a
  document literal in the language, a compiler milestone of its own;
  it is on the roadmap, and when it lands the package adds calls that
  take it beside the text ones (section 12). String templates with the
  values written inside the text (`"{ age: { $gte: ${min} } }"`) were
  weighed and rejected: `?` with values beside the text cannot be
  misused, since a Rust function taking a plain `&str` has nowhere to
  receive the values, while a template would look the same whether its
  destination binds or pastes; and a Mongo query is a document, which a
  document literal fits better than text with holes.
- **The official `mongodb` crate, 3.9, on Rust 1.88.** Every
  `mongodb` release after 3.6.0 (April 2026) needs Rust 1.88, so this
  package declares `rust-version = "1.88"`; the compiler and the other
  packages stay at 1.85. Pinning 3.6.0 would miss every fix since.
- **Native BSON types.** `Time` is stored as a BSON date, `Uuid` as
  binary subtype 4, `Bytes` as binary, so TTL indexes, date operators,
  Compass, and other drivers see real types. This needs varyk-std to
  hand the three types to the package typed (section 6.1).
- **`connect` gives the URL's database.** A service uses one database,
  and Mongo's URL has a place for its name, so there is no client
  object; pool size, timeouts, TLS, and replica-set options are URL
  options too, so there is no `connect_with`.
- **`insert` gives the new `_id` as a `string`**, as every driver gives
  the inserted id: Mongo makes an ObjectId when the document has no
  `_id`, and the program may need it. The README and the demo use a
  `Uuid` `_id` made with `Uuid::new()` (version 7, time-ordered like an
  ObjectId), so the typed path is the one shown.
- **Its own repository and release**, as varyk-sql.

## 2. Surface

Everything a program sees is in `src/lib.vr`:

```varyk
pub mod db;
mod read;
mod tests;
mod text;
mod write;
pub use db::connect;
pub use db::Database;
pub use db::Collection;
pub use db::Find;
pub use db::Tx;
```

Named with the key `mongo`, the items are `mongo::connect`,
`mongo::Database`, `mongo::Collection`, `mongo::Find`, and `mongo::Tx`.
`src/db.rs` declares them with the facade shapes of `docs/language.md`
("Calling Rust"). Every call below that reaches the server is `async`
and gives `Result<_, Error>`.

In the tables, a *text* is a literal (`&'static str`) in the syntax of
section 4, and `values..` are any number of trailing values (`Vec<varyk_std::Value>`),
which fill the `?`s of every text of the call from left to right.

### 2.1 Connecting

| Call | Gives | Does |
|---|---|---|
| `mongo::connect(url)` | `Database` | reads the URL, resolves a `mongodb+srv` name, sends `ping`, and gives the database the URL's path names |
| `db.collection(name)` | `Collection` | a handle to the collection `name`, a literal (`&'static str`) like every text, so input cannot choose what is queried; nothing is sent |
| `db.ping()` | `bool` | sends `ping`; `true`, for a readiness check |
| `db.begin()` | `Tx` | starts a session and a transaction (section 2.4) |

`url` is a `string`, usually read from `MONGO_URL` with `env::parse`. A
URL without a database (`mongodb://host:27017` or `…/?retryWrites=true`)
is an `Error` saying to add one after the host. The `ping` makes a wrong
URL, an unreachable cluster, or a refused password fail at startup
rather than at the first request; it waits as long as the URL's
`serverSelectionTimeoutMS` (the driver's default, 30 s). The URL's
database is also the one MongoDB signs the user in against unless the
URL says `authSource=`; a user made in the `admin` database (as Docker's
`MONGO_INITDB_ROOT_USERNAME` makes one) needs `?authSource=admin`, and
the refused-password message says so.

`Database` and `Collection` are handles with `#[derive(Clone)]`:
`db.clone()` is another handle on the same connection pool, for a
started task or a server's state. Both are cheap to make; a program may
call `db.collection("users")` where it needs it.

### 2.2 Reading

| Call | Gives | Does |
|---|---|---|
| `c.one(filter, values..)` | `T` | the first matching document read as `T`; an `Error` when none matches |
| `c.first(filter, values..)` | `Option<T>` | the first matching document, or `None` |
| `c.all(filter, values..)` | `Vec<T>` | every matching document |
| `c.count(filter, values..)` | `u64` | the number of matching documents |
| `c.find(filter, values..)` | `Find` | a query with options, sent by its `one`, `first`, or `all` |
| `f.sort(text)` | `Find` | the same query sorted (`"{ created_at: -1 }"`); no `?` |
| `f.skip(n)`, `f.limit(n)` | `Find` | the same query skipping `n`, or giving at most `n`, documents; `n` is an `i64`. A negative `n`, and a `limit` of 0 (which MongoDB reads as no limit), are an `Error` when the query is sent |
| `f.one()`, `f.first()`, `f.all()` | as on a `Collection` | sends the query |
| `c.aggregate(pipeline, values..)` | `Vec<T>` | runs the pipeline (a text that is an array of stages) and reads every document it gives |

`T` is chosen from where the result goes (M5b3 §2.1): `let user: User =
users.one(..).await?`. `sort`, `skip`, and `limit` are `&self` methods
giving a new `Find` that copies the text and values, so they chain on
the value `find` gives (Varyk calls no method that takes `self` by
value). `find`, `sort`, `skip`, and `limit` send nothing
and cannot fail: an error in their text or values is the `Error` of the
`one`, `first`, or `all` that sends the query. `all` and `aggregate`
read every document into memory, so a query that can match many says
`limit` (or `$limit`) itself; there are no cursors in this version.
`one` and `first` send the query with a limit of one.

### 2.3 Writing

| Call | Gives | Does |
|---|---|---|
| `c.insert(doc)` | `string` | inserts `doc`, a struct (section 3.1); gives its `_id` as text: a `Uuid` hyphenated, a `string` as it is, an integer in decimal, and an ObjectId the driver made, because `doc` had no `_id` (or a `None` one), as 24 hex digits |
| `c.insert_many(docs)` | `Vec<string>` | inserts a `Vec` of structs in order, and gives their `_id`s in the same order |
| `c.update(filter, update, values..)` | `u64` | updates the first matching document; the number matched, 0 or 1 |
| `c.update_many(filter, update, values..)` | `u64` | updates every matching document; the number matched |
| `c.upsert(filter, update, values..)` | `bool` | updates the first matching document, or inserts one built from the filter and the update when none matches; `true` when it inserted |
| `c.replace(doc)` | `bool` | replaces the document whose `_id` is `doc`'s with `doc`; `true` when one matched. A `doc` without an `_id`, or with a `None` one, is an `Error` |
| `c.delete(filter, values..)` | `u64` | deletes the first matching document; 0 or 1 |
| `c.delete_many(filter, values..)` | `u64` | deletes every matching document; the number deleted |
| `c.create_index(keys, options)` | `bool` | creates the index (`"{ email: 1 }"`, `"{ unique: true }"`); `true`. Creating an index that exists with the same options does nothing, so a program does it at startup. Neither text takes `?` |

`doc` and `docs` are read, not given away, as `json::stringify`'s
argument is (`docs/language.md`, the `&T: Serialize` shape), so `ada`
stays usable after `users.insert(ada)`. An `update` text is a document
of update operators (`"{ $set: { name: ? } }"`) or an array of pipeline
stages; in a document every top-level key must start with `$`, or the
call is a text `Error` (which leaves a `Tx` as it was), as `replace` is
the call for a whole document. `insert_many`
with an empty `Vec` gives an empty `Vec` without sending (the driver
would refuse it, and inside a `Tx` that would fail the transaction).
`insert_many` is ordered: on a failure the documents before it stay
inserted (outside a transaction), and the `Error` is the server's
message after "document N of the list:", N the failing document's
place; inside a `Tx` the failure fails the `Tx`, with the message of
section 2.4's Failure rule.

`create_index`'s options are a fixed list, and any other key is an
`Error` naming it, so a typo is not ignored: `unique`, `name`,
`sparse`, and `expireAfterSeconds` (a TTL index on a `Time` field).
`"{}"` is no options.

### 2.4 Transactions

```varyk
let mut tx = db.begin().await?;
let orders = tx.collection("orders");
let stock = tx.collection("stock");
orders.insert(order).await?;
let taken = stock.update("{ _id: ?, left: { $gte: ? } }", "{ $inc: { left: ? } }", item, n, -n).await?;
if taken == 0 {
    return Err(Error::new("out of stock"));   // drops tx: rolls back the insert
}
tx.commit().await?;
```

| Call | Gives | Does |
|---|---|---|
| `db.begin()` | `Tx` | starts a session and a transaction on it, reading from the primary whatever the URL's `readPreference` (MongoDB refuses any other read preference inside a transaction) |
| `tx.collection(name)` | `Collection` | a collection whose calls all run inside the transaction |
| `tx.commit()` | `bool` | a `mut self` method: commits and gives `true` (a facade cannot return `()` inside a `Result`); a second `commit` is an `Error`, "this transaction is finished" |

A transaction's collections are ordinary `Collection`s bound to the
`Tx`'s session, so every call of sections 2.2 and 2.3 works on them.
MongoDB refuses `create_index` inside a transaction on a collection
that already exists; indexes are made at startup, outside one. The
session is shared by the `Tx` and its collections behind an async
mutex (tokio's, section 8), held across the call's `.await`, so it is
used by one call at a time and a started call stays sendable to another
thread.

- **Failure.** A call stopped while it waits on MongoDB (its task
  stopped, as `Task::all` stops the others after an `Err`) fails the
  `Tx` too, since the write may or may not have applied. A call inside
  the transaction that the driver fails (a server error, a write
  conflict, a lost connection) marks the `Tx` failed; that call's own
  `Error` is the driver's or server's message followed by "; the
  transaction will roll back" (or by the temporary-conflict ending
  below). From then on every call on its collections is an `Error`, "a
  call in this transaction failed; it will roll back", without reaching
  the server, and `commit` aborts the transaction, ignoring the
  abort's own result (the server may have aborted it already and answer
  that there is no such transaction), and is an `Error`, "the
  transaction was rolled back: a call in it failed". The `Tx` is
  finished after it. An `Error` of the package's own (a text or value
  error, reading into `T`) leaves the `Tx` as it was. MongoDB aborts a
  transaction on most failed operations itself; the rule makes it one
  rule for every failure.
- **Rollback.** A `Tx` dropped without `commit` aborts: the driver
  aborts a session's open transaction when the session is dropped,
  which is when the `Tx` and its collections are all dropped. There is
  no `abort` call: return early, with `?` or otherwise. A transaction
  left open is also ended by the server after its
  `transactionLifetimeLimitSeconds` (60 s by default); a `Tx` dropped
  as the program exits is ended by the server this way, since the
  driver's abort cannot finish once the runtime stops.
- **Retries.** A failure MongoDB labels transient (a write conflict
  between two transactions, an election) means the whole transaction
  should run again, which only the program can do; its message ends
  "the transaction hit a temporary conflict; run it again" (Varyk's
  strings have `contains`, which the README's loop uses). The driver
  itself retries `commit` once when the result is unknown; when that
  retry fails too, with MongoDB's unknown-commit-result label, the
  message is "it is not known whether the transaction committed", and
  never says to run it again, since the commit may have applied. The
  README shows a loop that runs a transaction function up to three
  times on the temporary-conflict message.
- **Where.** Transactions need a replica set or a sharded cluster (Atlas
  is one). On a standalone server `begin` succeeds (the driver allows a
  transaction wherever the server reports sessions), and the first call
  inside it fails on the server with code 20 (IllegalOperation) and a
  message starting "Transaction numbers", which the driver rewrites to
  its own advice to add `retryWrites=false` (mongodb 3.9.1
  `client/executor.rs`, around line 543), wrong advice here. The
  package gives that call, matched on code 20 together with either
  text (code 20 alone is a general code), the message
  "transactions need a replica set or a sharded cluster", and the `Tx` is failed as after any failure.
  The README shows a single-node replica set for development (section
  7).
- A `Tx` is finished after any `commit`, whether it committed, rolled
  back a failed transaction, or itself failed (a temporary conflict, an
  unknown result). Calls on a finished `Tx`'s collections, and a
  second `commit`, are the `Error` "this
  transaction is finished".

## 3. Documents and types

### 3.1 Writing a document

`insert`, `insert_many`, and `replace` turn the argument into a BSON
document through the package's own serde serializer (`src/write.rs`),
which reports `is_human_readable()` as `false`, so `Time`, `Uuid`, and
`Bytes` reach it in the typed forms of section 6.1.
The argument must serialize as a struct or a `HashMap<string, V>`
(a `Vec` of those for `insert_many`), a map giving its keys as the
document's fields; anything else (a number, a string) is an `Error`.

| Varyk | BSON |
|---|---|
| `bool`, `string` | boolean, string |
| `f32`, `f64` | double |
| `i8`, `i16`, `i32`, `u8`, `u16` | int32 |
| `i64`, `u32` | int64 |
| `u64`, `usize` | int64 when at most `i64`'s largest; above it an `Error` naming the field (BSON has no unsigned integer) |
| `Time` | date, in milliseconds since 1970 (BSON dates hold no microseconds; `Time`'s are dropped, rounding toward the past) |
| `Uuid` | binary subtype 4, which Compass and the drivers show as `UUID("…")` |
| `Bytes` | binary subtype 0 |
| `Option`: `None` / `Some(x)` | null / `x` |
| `Vec<T>` | array |
| a struct | embedded document |
| `HashMap<string, V>` | embedded document |
| an enum (its variants carry no data) | its name, or its `#[rename]` |

The argument is what `json::stringify` takes (V0210 refuses the rest at
`varyk check`), so a `HashMap` key is a `string` and an enum's variants
carry no data. `#[rename]`, `#[skip]`, and `#[default]` apply as for JSON (M5a §2.2);
`#[rename("_id")]` is how a field becomes the document's `_id`. The
three types reach the serializer typed through section 6.1; without it
they would be strings.

The `_id` is checked before anything is sent. It may be a `string`, an
integer, or a `Uuid`. A `None` `_id` is left out of the document, so
the driver makes an ObjectId and `insert` gives it back
(`#[rename("_id")] id: Option<string>` reads a made id back as its hex
text; writing it back is a string `_id`, section 7). A made id needs
`Option<string>`: an `Option<Uuid>` or `Option<i64>` `_id` left `None`
gets an ObjectId that such a field cannot read back, and the
serializer cannot see the type inside a `None` to refuse it. Any
other kind (`bool`, a float, `Time`, `Bytes`, a struct, a `Vec`, a
map) is an `Error` naming its type. A 24-hex-digit `string` is stored as
a string, never turned into an ObjectId, which would be wrong for a
real string id.

### 3.2 Reading a document

A document is read into `T` through the package's own deserializer
(`src/read.rs`), by field name, with varyk-sql's rules: fields `T` has
no field for are ignored; a field `T` has but the document lacks is an
`Error` naming it, unless it is an `Option` (then `None`), has
`#[default]`, or is skipped; a `null` goes into an `Option` as `None`
and into any other field as an `Error` naming the field.

| BSON | Reads into |
|---|---|
| int32, int64 | an integer field when the value is in the field's range; a float field as Rust's `as` converts |
| double | a float field; an integer field only when it has no fractional part and is in range |
| boolean | `bool` |
| string | `string`; `Time` and `Uuid` when the text is one, as `parse` reads it; `Bytes` as base64, as JSON's |
| date | `Time`; `string` as `Time`'s ISO text. A date outside `Time`'s range is an `Error` naming the field |
| binary subtype 4 | `Uuid`; `string` hyphenated |
| other binary into a `string` | its bytes as text when they are UTF-8 (serde's string reads bytes so), otherwise an `Error` naming the field; `read.rs` does not try to tell a `string` field from a `Bytes` one here |
| binary subtype 3 (the legacy UUID of the old Java and C# drivers, in another byte order) | an `Error` naming the field |
| other binary | `Bytes`; a `Uuid` when it is 16 bytes, taken as the id's bytes in order (`read.rs` cannot tell a `Uuid` field from a `Bytes` one, since both ask for text and accept bytes) |
| ObjectId | `string`, as 24 hex digits |
| Decimal128 | `string`, as its decimal text (convert with `$toDouble` in an aggregation for a float) |
| embedded document | a struct, a `HashMap<string, V>` |
| array | `Vec<T>` |
| anything else (regex, JavaScript code, timestamp, min and max key, DBPointer) | an `Error` naming the field |

When `T` is a scalar (`let n: i64 = …`), the document must have exactly
one field other than `_id`, or `_id` alone, which is read as above; an
aggregation giving `{ _id: null, total: 12 }` reads into `i64` this way,
and `[{ $group: { _id: '$city' } }]` into `Vec<string>`. When `T` is an
`Option`, a document whose one field (or `_id` alone) is null reads as
`None`, as an `Option` scalar should; the reader cannot tell an
`Option` of a scalar from an `Option` of a one-field struct, so the
latter reads as `None` for such a document too (`first` is the call for
"maybe a document").

### 3.3 Values

A trailing value fills one `?` and is always data:

| `Value` | BSON |
|---|---|
| `Null` (`None`) | null, as an update operator's value; in query context an `Error` (section 4) |
| `Bool` | boolean |
| `Int` (every integer up to `i64`) | int64; MongoDB compares numbers across int32, int64, and double, so an int64 matches an int32 field. A `{ $set: { age: ? } }` therefore stores an int64 where `insert` of an `i32` field stores an int32; both read back into the field |
| `Float` | double |
| `Text` | string, or an ObjectId under `ObjectId(?)` (section 4) |
| `Time` | date, in milliseconds as in 3.1, so a `Time` read back from a document matches its own value |
| `Uuid` | binary subtype 4 |
| `Bytes` | binary subtype 0 |

## 4. The query text

Every text is parsed by the package (`src/text.rs`) straight into a
BSON document or array on each call, never through extended JSON:
bson's extended-JSON reader turns `$regularExpression`, `$date`,
`$oid`, and `$binary` keys into real BSON types, which would let a
`?` under `$regularExpression` carry a pattern past the `$regex` rule
below. In the text such keys are ordinary keys; the parse costs microseconds against a
network round trip. The syntax is the relaxed JSON of the shell:

- objects `{ key: value, … }` and arrays `[ value, … ]`, a trailing
  comma allowed;
- a key bare (letters, digits, `_`, `$`, and `.`, as in `$gte`,
  `address.city`) or quoted;
- a string in `'single'` or `"double"` quotes, with JSON's escapes; a
  `?` inside quotes is text;
- an integer (int32, or int64 when it does not fit; beyond int64 an
  `Error`), a number with `.` or an exponent (double), `true`, `false`,
  `null`;
- `?`, a value from the trailing values;
- `ObjectId(?)`: an ObjectId from the value, which must be a `Text` of
  24 hex digits; any other value is an `Error` naming its position. It
  counts as one `?`, for the number of values and for the allowlist
  below.

A filter, sort, index keys, and index options must be a document, a
pipeline an array of documents, and an update either; an empty text is
an `Error`. The rules that keep input out of the query:

- `?` stands only where a value goes, never as a key: `{ ?: 1 }` is an
  `Error`;
- a key written twice in one document is an `Error` naming its
  position, since the second would silently drop the first and its
  value;
- the number of `?`s across a call's texts must equal the number of
  values, or the call is an `Error` naming both counts, before anything
  is sent;
- each value becomes one BSON value of section 3.3, never parsed as
  text, so a string from a request stays a string;
- a value stands only where MongoDB reads plain data. This is an
  allowlist: a `?` is accepted
  - in query context: anywhere in a filter text, and anywhere in a
    top-level `$match` stage of a pipeline, except under `$expr` and under
    `$jsonSchema` (whose `pattern` would be a pattern from input);
  - as a value under the update operators `$set`, `$setOnInsert`,
    `$inc`, `$mul`, `$min`, `$max`, `$push`, and `$addToSet`, at any
    depth under the operator's field (`$set: { profile: { name: ? } }`,
    `$push: { sessions: { token: ?, at: ? } }`), where `$each` is the
    only `$`-key allowed above a `?` (under `$push` and `$addToSet`; the `$slice`, `$sort`, and `$position`
    modifiers are written in the text), and in the condition of `$pull`,
    which is query context;
  - as the value of a top-level `$skip` or `$limit` stage.

  The one place the allowlist accepts a syntax of MongoDB's own is
  `$text`'s `$search` in query context: there a `"` makes a phrase and a
  `-` negates a word, which changes only which documents match.

  Anywhere else a `?` is an `Error` naming its position, "a value can
  stand only in a filter, a `$match` stage, a value of `$set` or a like
  update operator, or `$skip` and `$limit`": under `$expr`, in any other
  pipeline stage and in every sub-pipeline (`$facet`, `$lookup`'s
  `pipeline`, `$unionWith`),
  in an update given as an array of stages, and under any other update
  operator (`$rename`, `$unset`, `$currentDate`, ...). In those places
  MongoDB reads a string as a field path (`"$passwordHash"`), a
  variable, a collection or field name (`$lookup`'s `from`, `$out`,
  `$rename`), or search syntax (Atlas's `$search` stages); the
  allowlist keeps input out of all of them without listing them, and
  an operator MongoDB adds later is refused until it is allowed. A
  pipeline that needs a value puts it in its `$match` stage, and a
  program that needs more picks between literal pipelines. Query
  context and update-operator values never read a string as a path or
  a name, so nothing is wrapped or rewritten there;
- a `None` value is never a match for a missing field. In query context
  `{ f: null }` matches every document where `f` is null or missing, so
  `users.first("{ reset_token: ? }", token)` with an absent token would
  find some user without one, and `{ $pull: { sessions: { token: ? } } }`
  with a `None` would remove every session without a token. There a
  `Null` value is an `Error` naming its position: "a `None` value cannot
  be matched; write `null` in the text to match a missing or null
  field". As an update operator's value a `None` is null;
- `$where`, `$function`, and `$accumulator`, which run JavaScript on the
  server, are an `Error` as a key, bare or quoted, anywhere in a text (a
  string value `'$where'` is only text);
- a value is never a pattern. A `?` that is the value of `$regex` must
  be a `Text` (another value is an `Error` naming its position), and the
  package escapes it: every character with a meaning in a pattern
  (`\ ^ $ . | ? * + ( ) [ ] { } -`, `#`, and whitespace, which the `x`
  option would ignore) gets a backslash, so the value matches its own
  text and nothing else, and no input can make a pattern slow.
  `{ name: { $regex: ?, $options: 'i' } }` is then a case-insensitive
  "contains" search, the search box's usual need. A `?` as the value of
  `$options` is an `Error`: the options are part of the query. A
  pattern of the program's own is written in the text
  (`{ sku: { $regex: '^A-[0-9]+$' } }`), where no input reaches it.
  Taking a pattern from input is not offered in this version.

Since every text is a literal, each error that depends on the text
shows on the first run of the call that has it, normally in a test;
those that depend on a value (a `Text` under `ObjectId(?)` that is not
hex, a `None` under `$regex`) show when such a value comes.

## 5. Errors

Every failure is a `varyk_std::Error` whose message the package writes.
No call panics: every driver `Result` is mapped, and the serializer,
deserializer, and parser return errors.

- **Connecting.** The URL is parsed with the driver's
  `ConnectionString::parse` first, with no network, then resolved and
  pinged. The message says which step failed: the URL cannot be
  read (a fixed message with no driver text, since the driver's parse
  errors repeat option values), a `mongodb+srv` name that does not
  resolve ("cannot be reached"), the URL names no database, the server
  cannot be reached (followed by the driver's message, which names hosts
  and never a password), or the user name or password was refused (with
  the `authSource=admin` hint of section 2.1). No message contains the
  URL, so a password cannot reach a log through an error.
- **Server errors** carry the server's code and message, as `E11000
  duplicate key error collection: app.users index: email_1 dup key: {
  email: "ada@example.com" }`. MongoDB puts the duplicate key's value in
  that message; whether to remove it is open, as in varyk-sql
  (section 12). The `errInfo` of a validation failure is not included.
- **Other driver failures** after connecting (a lost connection, a
  timeout) carry the driver's message, which names hosts and never a
  password or a value.
- **Text, values, and reading** name the text's position, the value's
  position, or the field, and never contain a value. `read.rs`'s
  `de::Error` ignores the message given to `custom` and the values
  given to `invalid_type`, `invalid_value`, and `unknown_variant`
  (varyk-std's `Time` and `Uuid` visitors put the text they read in
  their message, as serde does an enum's unknown name), and builds its
  message from the field and the expected type only.
- **Transactions** have the fixed messages of section 2.4.
- **Logging.** The driver's command tracing (`tracing-unstable`) stays
  off: it logs whole commands with their values. The package logs
  nothing of its own in this version.

## 6. Changes in Varyk-Lang/varyk

Section 6.1 and 6.3 are one pull request before this package's 0.1.0,
released as varyk 0.8.1; section 6.2 is a second pull request, merged
only after 0.1.0 is on crates.io, so `varyk add mongo` never names a
crate anyone else could register first. Section 6.1 and 6.2 are each a
`fix:` commit, and 6.3 a `docs:` commit beside 6.1's. `fix:`, not `feat:`, on
purpose: a `feat:` before 1.0 bumps the minor version to 0.9, and every
package's `varyk-std = "0.8"` requirement would then need a release of
its own. The change is additive within 0.8, so a patch release is
honest under semver, and each commit's subject says what it fixes for a
package ("Time, Uuid, and Bytes keep their type outside JSON"), not
"add a module", so the changelog line under fixes reads true.

### 6.1 `Time`, `Uuid`, and `Bytes` keep their type outside JSON

varyk-std's `Serialize` for the three types asks the serializer
`is_human_readable()`. A human-readable one, JSON and every text format,
gets what it gets today, byte for byte. Otherwise:

- `Time` serializes as `serialize_newtype_struct(varyk_std::serde_names::TIME, micros)`,
  `micros` its `i64` Unix microseconds;
- `Uuid` as `serialize_newtype_struct(varyk_std::serde_names::UUID, bytes)`,
  its 16 bytes through `serialize_bytes`;
- `Bytes` as `serialize_bytes`, serde's own byte type.

`varyk_std::serde_names` is a new public module with the two `&str`
constants (`"$__varyk_std_Time"` and `"$__varyk_std_Uuid"`), documented
as the contract a database package's serializer matches on. `Deserialize`
does not change: it already accepts text, and raw bytes for `Uuid` and
`Bytes`. The tagged form is therefore one-way, for a package that
reads it with its own serializer; MessagePack or bincode would not
read it back, and the module's documentation says so. Nothing Varyk ships uses a binary serde format today, so no
output of an existing program changes. Tests: the JSON of the three is
unchanged; a test serializer that is not human-readable sees each form
above. `docs/language.md` ("A facade for a package") gains a sentence
naming the module for facade authors.

### 6.2 `varyk add mongo`

`("mongo", "varyk-mongo")` joins the shorthand table of `varyk add`
(`crates/varyk/src/cli.rs`), and `mongo` joins the second list of the
same names, the "`varyk add sql` adds varyk-sql" note for an unknown
`mongo::` path (`crates/varyk/src/resolve/package_items.rs`, `not_added`),
each with its test. Everywhere `docs/language.md` names `http` and `sql`
as shorthands, `mongo` joins them; so it does in the doc comment of
`not_added`, which names both, and in the doc comment of `varyk add`
in `cli.rs`, which names only `sql` today and is brought up to all
three. The same pull request checks the
roadmap's `- [ ] varyk-mongo` item that section 6.3 adds. It ships in the varyk release after
varyk-mongo 0.1.0.

### 6.3 Documents

Section 6.1's pull request updates `docs/roadmap.md` (turning the sentence
"After 5b4, `varyk-mongo` and `varyk-redis` are the next packages"
under Milestone 5b4 into an unchecked item, `- [ ] varyk-mongo`, with a
link to this spec, keeping "`varyk-redis` is next" as a sentence after
it, and a new item: a document literal in the language, with
values written inline, for MongoDB and JSON), `docs/open-questions.md` (the document literal; a
prefix search from input; a transaction call that retries). README's
list of packages gains varyk-mongo in section 6.2's pull request, once
0.1.0 is on crates.io.

## 7. Production use

What a service does with the package, each covered or written down in
the README:

- **Configuration.** `MONGO_URL` through `env::parse`, with the
  database in the path; the `.env` form for development.
- **TLS.** The driver's `rustls-tls` feature with webpki roots, so
  Atlas (`mongodb+srv://…/app`, TLS on by default) works in a minimal
  container with no CA bundle and no OpenSSL; `tls=true` turns it on
  elsewhere, and `tlsCAFile=` names a private CA's file.
- **Pool and timeouts.** URL options: `maxPoolSize`,
  `serverSelectionTimeoutMS`, `connectTimeoutMS`, `w`, `readPreference`.
  The README lists the few a service sets.
- **Indexes.** `create_index` at startup, before serving, the way
  varyk-sql's `migrate` runs.
- **Readiness.** `db.ping()`.
- **A server's state.** `Database` is `Clone`, so it is the state of a
  `varyk-http` app as varyk-sql's `Pool` is.
- **Transactions.** The retry loop of section 2.4, and the single-node
  replica set for development:
  `docker run -d -p 27017:27017 mongo:8.0 --replSet rs0 --bind_ip_all`,
  then `rs.initiate({ _id: 'rs0', members: [{ _id: 0, host: '127.0.0.1:27017' }] })`
  through `mongosh`. The member's host must be one the program can
  reach; the container's own name is not.
- **Dates.** Stored in milliseconds (section 3.1): two `Time`s that
  differ only in microseconds are equal once stored.
- **Ids.** `#[rename("_id")] id: Uuid` with `Uuid::new()`; reading and
  querying an existing collection's ObjectIds through `string` and
  `ObjectId(?)`. A document whose ObjectId `_id` was read into a
  `string` is written back with a string `_id`, so `replace` on it
  matches nothing and gives `false`: such a collection is updated with
  `update("{ _id: ObjectId(?) }", ..)`.
- **Signing in.** A URL whose user lives in `admin` adds
  `?authSource=admin` (section 2.1).
- **Versions.** The package depends on `varyk-std` with a requirement
  of at least the release that has section 6.1 (`"0.8.1"`); a program
  and the package resolve to one `varyk-std`. The README keeps a table
  of which `varyk-mongo` works with which `varyk`, and says programs
  using it need Rust 1.88 or later.

## 8. Package layout

```text
varyk-mongo/
  Cargo.toml
  src/lib.vr          pub mod db; mod read; mod tests; mod text; mod write; pub use ..
  src/db.rs           the facade: connect, Database, Collection, Find, Tx, errors
  src/text.rs         the query-text parser (section 4)
  src/write.rs        the serializer (section 3.1)
  src/read.rs         the deserializer (section 3.2)
  src/tests.vr        the Varyk #[test]s of section 9
  demo/users/         the program of section 1, with
                      mongo = { package = "varyk-mongo", path = "../.." }
  docs/specs/         this file
  README.md, CONTRIBUTING.md, AGENTS.md, CHANGELOG.md, LICENSE-MIT, LICENSE-APACHE
  .github/workflows/  ci.yml, release-please.yml, commit-messages.yml, latest-varyk.yml
  release-please-config.json, .release-please-manifest.json
```

Varyk copies into the generated crate only the modules a `mod` line
declares, so `lib.vr` names all four `.rs` files; only `db` is `pub`,
and only `db.rs` declares items a program uses. `db.rs` reaches the
others as `crate::text`, `crate::write`, and `crate::read`.

`Cargo.toml`: `[lib] path = "src/lib.vr"`; `edition = "2024"`;
`rust-version = "1.88"`; `license = "MIT OR Apache-2.0"`; `authors =
["Vlad Mickevic"]`; a `description` naming MongoDB; `repository`;
`homepage = "https://varyk.com"`; `keywords` (`varyk`, `mongodb`,
`mongo`, `database`, `nosql`); `categories = ["database"]`.
`[dependencies]`: `varyk-std = "0.8.1"`; `mongodb = "3.9"` with
`default-features = false` and the features `compat-3-3-0` (the crate
refuses to build without it), `bson-3`, `rustls-tls`, and
`dns-resolver` (for `mongodb+srv`); `tokio = { version = "1", default-features = false,
features = ["sync"] }` for the transaction's async mutex (tokio is
already in the build through the driver). bson is reached as
`mongodb::bson`, and serde as `varyk_std::serde`, so the facade names
no `serde` dependency of its own. No cargo features of its own.

`AGENTS.md` and `CONTRIBUTING.md` follow varyk-sql's: no crash on
absence; no secret in an error (no URL, password, or value in a message
the package writes); query texts stay literal, with no way to build one
from input; conventional commits without `<` or `>` in subjects; docs
move with the code.

## 9. Testing and definition of done

- `varyk test` in the package runs `src/tests.vr` against a real server,
  since MongoDB has no in-memory mode: `MONGO_URL` through `env::parse`
  into a struct with one `Option<string>`, defaulting to
  `mongodb://127.0.0.1:27017/varyk_test`. Each test works in a
  collection named after itself and deletes its documents first, so
  tests run in parallel and a rerun starts clean.
- The tests cover: every call of section 2, with zero and several
  values; `one` with no match and `first` giving `None`; `find` with
  `sort`, `skip`, and `limit`, a negative `limit`, and `limit(0)`;
  `count`; `insert` giving a `Uuid` `_id` as text and a made ObjectId
  (no `_id`, and a `None` one) as 24 hex digits, read back into a
  `string` and found again with `ObjectId(?)`; an `_id` of a refused
  kind; `insert_many`'s ids in order, and a duplicate in the middle
  stopping at the failing one; `upsert` inserting and then updating; `replace`
  without an `_id`; a filter on a user field named `from` taking a
  `?`; a stored string that is not a
  `Uuid` read into a `Uuid` field, and an unknown enum name, each an
  `Error` without the stored text; a `?` under `$expr`, in an
  `$addFields` stage, in a `$lookup`'s `from` and in its `pipeline`'s
  `$match`, in a `$search` stage, under
  `$jsonSchema`, under `$rename`, and in an update given as an array
  each an `Error` naming its position, while `"$secret"` in a filter,
  in a `$match` stage, and under `$set` stays text; `$skip` and `$limit`
  taking a `?`; `aggregate` into a struct and into a
  scalar; the round trip of every type of section 3.1, with the stored
  types checked on the server (`count("{ created_at: { $type: 'date' }
  }")`, `binData` for `Uuid`); `u64` above `i64`'s range; a nested
  struct, a `Vec`, a `HashMap`, an enum; a missing field, a `null` into
  a plain field, a field of a type that does not fit; every text error
  of section 4 (the count, `$where` nested in `$expr`, `?` as a key, a
  bad ObjectId, a filter that is an array, a `?` as `$options`, and a
  `$regex` value of `.*` matching only the text `.*`); `create_index` twice, an
  unknown option, and a duplicate key giving the server's message;
  a transaction committed, dropped (after which a plain `insert` of the
  same `_id` succeeds at once, which it could not while an unaborted
  transaction held the document), failed and then committed, and
  `commit` twice; a call on a committed `Tx`'s collection giving "this
  transaction is finished"; a `?` inside `$push`'s `$each` taken; an
  empty text an `Error`; a call on a failed `Tx`'s collection giving "a call
  in this transaction failed; it will roll back"; a package `Error`
  inside a `Tx` (`one` with no match, an update document with a key
  not starting with `$`) leaving it usable and `commit` succeeding;
  `insert_many` of an empty `Vec` giving an empty `Vec`; a `?` under
  `$text`'s `$search` taken; the temporary-
  conflict message from two `Tx`s updating one document; a `?` in a
  `sort` text and in a `create_index` text refused; a value that is not
  a `Text` under `$regex` an `Error`; a `None` value in a filter, in a `$pull` condition, and in an
  `aggregate`'s `$match` stage an
  `Error`; a negative `skip` an `Error`; a 24-hex-digit `string` `_id` stored as a
  string (`$type: 'string'`); a `Tx`'s `first` under a URL
  with `readPreference=secondaryPreferred` working; and a `None`
  in a `$set` storing null; `connect` with no database, a bad URL, a wrong
  password, an unreachable server (`serverSelectionTimeoutMS=500`), and
  a `mongodb+srv://nonexistent.invalid/app` name that does not resolve,
  each without the URL in its message.
- The standalone-server message (code 20, section 2.4) is checked by
  hand against a standalone `mongo:8.0` during implementation, since CI's
  server is a replica set.
- The local server for the tests is a single-node replica set as in
  section 7; a machine whose port 27017 is taken runs it on another
  port (`mongod --port 27018`, the member named `127.0.0.1:27018`) and
  sets `MONGO_URL` to match.
- Read-only conversions of section 3.2: a whole double (written with
  an update pipeline's `$toDouble`) read into an integer field, a
  double with a fraction into an integer field an `Error`, and a
  Decimal128 (`$toDecimal`) read into a `string`.
- `demo/users` builds, with `MONGO_URL` set, and prints its expected output against the same
  server; it deletes its own documents by email first, so a run that
  stopped half-way does not make every later run a duplicate key.
- CI, in two jobs named `test` (stable) and `msrv` (1.88), the names
  the branch ruleset requires: each starts `mongo:8.0` as a single-node
  replica set with `docker run`, initiates it as in section 7 (GitHub's
  service containers cannot pass `--replSet`), and waits until
  `db.hello().isWritablePrimary` is true, installs
  `varyk` from crates.io, and runs `varyk check` and `varyk test`; `test`
  also runs `rustfmt --edition 2024 --check` on `src/*.rs`, `cargo clippy --all-targets -- -D warnings` in the
  crate `varyk publish --assemble-only` writes, and the demo. The
  `latest-varyk` workflow is varyk-sql's, starting the replica set as
  `ci.yml` does, since every test needs a server. The tests target MongoDB 8.0,
  and the README claims no other server version.
- No `unwrap`, `expect`, or other crash-on-absence call in `src/*.rs`.
- README: install, the example of section 1, configuration, TLS, the
  type table, the query text with its safety rules, transactions and
  the retry loop, the development replica set, testing, the version
  table, and the line on MongoDB, Inc.

Done when CI is green, 0.1.0 is on crates.io, and the roadmap item in
`Varyk-Lang/varyk` is checked.

## 10. Repository and release

As varyk-sql (its spec, section 7): release-please with `release-type:
rust`, one package at the root, `include-component-in-tag: false`,
`bump-minor-pre-major: true`, `bump-patch-for-minor-pre-major: false`,
tag `v0.1.0`; the release job installs `varyk` and runs `varyk publish`
with `CARGO_REGISTRY_TOKEN` from the `release` environment, installing
`varyk` `^0.8.1` like CI; the
commit-messages workflow copied; no `SECURITY.md` of its own (the
organization's policy applies; docs link
https://github.com/Varyk-Lang/varyk-mongo/security/policy); dual
MIT/Apache-2.0 license; published by the organization under
`TRADEMARKS.md`.

The repository starts empty. The maintainer pushes an empty initial
commit to `main`; the spec branch is rebased onto it, and the work
merges through a pull request. Order: varyk's pull request of section
6.1 and the 0.8.1 release; then this package's pull request, its CI
installing `varyk` `^0.8.1` from crates.io; then 0.1.0; then varyk's
pull request of section 6.2.

## 11. Not in this version

A client with several databases; cursors and streaming rows; change
streams; GridFS; bulk writes; `find_one_and_update` and the other
find-and-modify calls; a projection on `find` (a `T` ignores the fields
it does not name); `distinct`; `abort` as a call; a transaction call
that retries; a pattern taken from input; `?` in a sort or an index;
a list beside the text (`{ _id: { $in: ? } }`: values are scalars, so
an `$in` lists its `?`s one by one or the program runs one query per
value); `partialFilterExpression` and other index options; an
`ObjectId('…')` written in the text; collection names chosen at run
time; statement logging; Decimal128, regex, and timestamp values beside
the text; a `?` outside the allowlist of section 4 (in an expression,
in a pipeline stage other than `$match`, `$skip`, and `$limit`, in an
update given as stages, under other update operators, in a sub-pipeline
of `$facet`, `$lookup`, or `$unionWith`, whose `$match`, `$skip`, and
`$limit` a later version can open as it opens the top-level ones), which a later
version can open by sending a `$`-text as `$literal` in expressions and
refusing values where an operator takes a name; MongoDB versions other than 8.0 in CI; client-side
field-level encryption; Atlas Search and vector search helpers.

## 12. Open questions

- A document literal in the language, `doc!{ age: { $gte: min } }`
  with values written inline, as Rust's `doc!` and Node's object
  literals: what type it has, and whether JSON shares it. The package
  would add calls taking it beside the text ones.
- A prefix search from input (`^` then the escaped value), which an
  index can serve, would need a form such as `Prefix(?)` in the text;
  is it needed often enough to add?
- Should values be removed from the server's messages (a duplicate
  key's value, a failed `$convert`'s input), so a log never holds them
  (as varyk-sql's open question)?
- A transaction call that retries on a transient failure needs a
  function passed as an argument, which a facade cannot take yet.
- Should `find` gain a projection once documents are large enough for
  it to matter?

## 13. Decisions

2026-10-09, while brainstorming with the maintainer:

- Name `varyk-mongo` over `varyk-mongodb`: shorter, the roadmap's name,
  and the naming rule unchanged.
- Scope: reading, writing, indexes, aggregation, and transactions;
  change streams, GridFS, and cursors later.
- Native BSON types for `Time`, `Uuid`, and `Bytes`, at the cost of a
  varyk-std change and a varyk release first.
- Shell-syntax text with `?` over strict JSON and a typed builder; a
  document literal in the language later, not first.
- `rust-version = "1.88"` for this package alone, with `mongodb` 3.9;
  moving the whole project's floor is a separate decision.
- `connect` gives the URL's database; `insert` gives the `_id` as a
  `string`.
- From the design's own review: Varyk passes arguments without `&`
  (`users.insert(ada)`), checked with a stub facade under `varyk check`,
  as are the `find(..).sort(..).limit(..).all()` chain and
  `insert_many` of a `Vec` of structs; `create_index` options are a
  fixed list so a typo is an `Error`; the transaction rule counts every
  driver failure, not only the server's; the development replica set
  names `127.0.0.1` as its member; `find` has no projection.
- A value under `$regex` (and, before the allowlist superseded it, `regex`
  in the `$regex…` aggregation operators, which no `?` now reaches) is
  escaped by the package, and a value as `$options` is
  refused: the first design left escaping to the README, which broke
  the rule that the safe way is the default, and Varyk has no escape
  function a program could call. A pattern from input is not offered.
- Staying on `?` rather than string templates (`${min}` inside the
  text), on an independent evaluation the maintainer accepted: `?`
  with values beside the text cannot be misused, a template's safety
  would depend on its destination's type (C#'s Entity Framework had to
  split `FromSql` for this), and the two-queries problem templates
  would solve is varyk-sql's to fix (varyk-sql 0.4, `$1` on every
  database).
- From the first review round, with the advisor's rulings: a `$`-text
  value in an expression is sent as `$literal` (wrapped, not refused,
  so the outcome never depends on input), by the two-rule context of
  section 4; `collection(name)` is literal-only; `limit(0)` is an
  `Error`; a `None` `_id` is left out, and `_id` kinds other than
  string, integer, and `Uuid` are refused; the transaction's session
  sits behind tokio's mutex; a standalone server's code 20 has its own
  message (round 3 corrected round 1: `begin` itself succeeds there); an
  unknown commit result has its own message; `insert_many` reports N of
  M; the `authSource` hint; `partialFilterExpression` and the
  `ObjectId('…')` literal dropped.
- From the independent (Fable) review: the places a value may stand
  became an allowlist (query context, update operators' values, `$skip`
  and `$limit`) in place of the earlier `$literal` wrapping and list of
  name-taking parameters, a denylist that a new MongoDB operator would
  have slipped past; `insert_many`'s "N of M" message gave way to the
  driver's; transactions stay, as the maintainer chose scope with them;
  the varyk-side commits stay `fix:` by the maintainer's rule after 0.7
  (a `feat:` before 1.0 forces every package's release).
- From implementation, with the reviewers' rulings: `insert_many`'s
  failure names the failing document ("document N of the list:") before
  the server's message; a key written twice in one document is an
  `Error`; an empty update document `{}` is a text `Error`, so a
  transaction is not failed by a text mistake; a top-level `Option`
  reads `None` for a one-field document holding null; a call stopped
  mid-send fails its transaction; server errors read "{message} (code
  {code})"; the tests run on MongoDB 8.0 (`mongo:8.0`); a replacement
  document whose first key starts with `$` is a package `Error` before
  anything is sent.
