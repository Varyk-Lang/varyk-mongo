# varyk-mongo

The official MongoDB package for [Varyk](https://varyk.com), a language
for backend services that compiles to Rust, over the official `mongodb`
crate.

varyk-mongo 0.1 works with varyk 0.8.1 and later 0.8 releases (see
[Versions](#versions)), and programs that use it need Rust 1.88 or
later. Varyk is experimental and pre-1.0: anything here may change.
varyk-mongo is not affiliated with or endorsed by MongoDB, Inc.

The package covers what an ordinary service does with MongoDB and
nothing more: connect over TLS, read and write typed documents, filter
with MongoDB's own operators, aggregate, index, run transactions, and
test against a real server.

## Install

```sh
cargo install varyk --version '^0.8.1' --locked
varyk init users
cd users
varyk add varyk-mongo --rename mongo
```

`varyk add` passes these arguments to `cargo add`, so the manifest gets
`mongo = { version = "0.1.0", package = "varyk-mongo" }` and code names
the package `mongo::`. The shorter `varyk add mongo` does the same from
the varyk release after varyk-mongo 0.1.0.

The `mongodb` crate needs Rust 1.88 or later; `rustup update` gets it.

## A first program

`src/main.vr`:

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

`.env`:

```sh
MONGO_URL=mongodb://127.0.0.1:27017/app
```

With a server running (see [A server for development](#a-server-for-development)),
`varyk run` prints `true 1 1`: Ada was found, she is the one recent
user, and one document was deleted. The same program is in
[`demo/users`](demo/users).

Each text in quotes is MongoDB's own query syntax, as the shell and
Compass write it, with `?` where a value goes; the values follow the
text (see [The query text](#the-query-text)). `#[rename("_id")]` makes
`id` the document's `_id`, and `Uuid::new()` makes a new id ordered by
time, as an ObjectId is (see [Ids](#ids)). `ada` stays usable after
`users.insert(ada)`: the call reads the document and does not take it.

## Configuration

The URL comes from the environment through `env::parse`, as above: the
field `mongo_url` reads `MONGO_URL`. A variable set in the process
environment wins; otherwise `env::parse` reads `.env` in the current
directory, which `varyk init` lists in `.gitignore`, so a development
password stays out of the repository. In production, set `MONGO_URL` in
the service's environment.

| Server | URL |
|---|---|
| a local server | `mongodb://127.0.0.1:27017/app` |
| a replica set | `mongodb://user:password@db1:27017,db2:27017,db3:27017/app?replicaSet=rs0` |
| Atlas | `mongodb+srv://user:password@cluster0.example.mongodb.net/app` |

The URL names the database after the host (`/app`): a service uses one
database, and `mongo::connect` gives that one. A URL without a database
is an `Error` saying to add one.

| Call | Gives |
|---|---|
| `mongo::connect(url)` | `Result<mongo::Database, Error>`: reads the URL, looks up a `mongodb+srv` name, and sends `ping` |
| `db.collection(name)` | `mongo::Collection`: a handle to the collection `name`; nothing is sent |
| `db.ping()` | `Result<bool, Error>`: sends `ping`; `true` |
| `db.begin()` | `Result<mongo::Tx, Error>`: starts a transaction (see [Transactions](#transactions)) |

`connect` sends `ping`, so a wrong URL, a server that cannot be
reached, or a refused password fails at startup rather than at the
first request. It waits as long as the URL's `serverSelectionTimeoutMS`,
30 seconds unless the URL says otherwise. Each step that can fail has
its own message, and no message contains the URL, so a password cannot
reach a log that way.

`name` is a literal string, like every text of the package, so input
cannot choose which collection is read. A collection is made on its
first write.

`db.clone()` is another handle on the same pool of connections, for a
started task or a server's state (see [A server's state](#a-servers-state)).
`db.collection("users")` is cheap: call it where it is needed.

### Options

Pool size, timeouts, and the rest are options at the end of the URL,
after `?` and joined with `&`. The few a service sets:

| Option | Does |
|---|---|
| `maxPoolSize=20` | at most 20 connections to each server; 10 by default |
| `serverSelectionTimeoutMS=5000` | how long a call waits for a server it can use, `connect` included; 30 000 by default |
| `connectTimeoutMS=5000` | how long opening one connection may take; 10 000 by default |
| `w=majority` | how many servers confirm a write before it counts as done |
| `readPreference=secondaryPreferred` | reads go to a secondary when there is one |
| `authSource=admin` | the database the user was made in (see below) |

### Signing in

MongoDB signs the user in against the URL's database unless the URL
says `authSource=`. A user made in the `admin` database, as Docker's
`MONGO_INITDB_ROOT_USERNAME` makes one, needs `?authSource=admin`:

```sh
MONGO_URL=mongodb://root:password@127.0.0.1:27017/app?authSource=admin
```

The message of a refused password says so too.

## TLS

The driver is built with rustls and the webpki root certificates, so
TLS works in a minimal container with no CA bundle and no OpenSSL.
Atlas URLs (`mongodb+srv://`) turn TLS on by themselves. Elsewhere,
`tls=true` turns it on, and `tlsCAFile=/path/ca.pem` names the file of a
private CA that signed the server's certificate:

```sh
MONGO_URL=mongodb://user:password@db.internal:27017/app?tls=true&tlsCAFile=/etc/ssl/db-ca.pem
```

## Reading

| Call | Gives |
|---|---|
| `c.one(filter, values..)` | `Result<T, Error>`: the first matching document, or an `Error` when none matches |
| `c.first(filter, values..)` | `Result<Option<T>, Error>`: the first matching document, or `None` |
| `c.all(filter, values..)` | `Result<Vec<T>, Error>`: every matching document |
| `c.count(filter, values..)` | `Result<u64, Error>`: the number of matching documents |
| `c.find(filter, values..)` | `mongo::Find`: a query with options, sent by its `one`, `first`, or `all` |
| `f.sort(text)` | `mongo::Find`: the same query, sorted (`"{ created_at: -1 }"`) |
| `f.skip(n)`, `f.limit(n)` | `mongo::Find`: the same query, skipping `n` documents, or giving at most `n`; `n` is an `i64` |
| `f.one()`, `f.first()`, `f.all()` | as on a collection: sends the query |
| `c.aggregate(pipeline, values..)` | `Result<Vec<T>, Error>`: runs the pipeline and reads every document it gives |

```varyk
let user: User = users.one("{ email: ? }", email).await?;
let adults: u64 = users.count("{ age: { $gte: ? } }", 18).await?;
let page: Vec<User> = users.find("{ city: ? }", city)
    .sort("{ name: 1 }").skip(20).limit(10).all().await?;
```

`T` comes from where the result goes, as above. `find`, `sort`, `skip`,
and `limit` send nothing and cannot fail: a mistake in their text or
values is the `Error` of the `one`, `first`, or `all` that sends the
query. A negative `skip` or `limit` is an `Error`, and so is `limit(0)`,
which MongoDB would read as no limit. `one` and `first` ask the server
for one document.

`one` or `first`: a handler that answers "not found" uses `first` and
matches on `None`; `one` is for a document that must exist. `all` and
`aggregate` read every document into memory, so a query that can match
many says `limit` (or `$limit`) itself; there are no cursors in this
version.

### Aggregation

A pipeline is an array of stages in one text:

```varyk
struct CityCount {
    #[rename("_id")]
    city: string,
    users: i64,
}

let cities: Vec<CityCount> = users.aggregate("[{ $match: { age: { $gte: ? } } }, { $group: { _id: '$city', users: { $sum: 1 } } }, { $sort: { users: -1 } }, { $limit: 10 }]", 18).await?;
let total: Vec<i64> = users.aggregate("[{ $group: { _id: null, total: { $sum: '$age' } } }]").await?;
```

A value may stand in the pipeline's top-level `$match` stages and as the
number of a top-level `$skip` or `$limit` stage, and nowhere else (see
[The query text](#the-query-text)).

## Writing

| Call | Gives |
|---|---|
| `c.insert(doc)` | `Result<string, Error>`: inserts `doc` and gives its `_id` as text |
| `c.insert_many(docs)` | `Result<Vec<string>, Error>`: inserts a `Vec` of documents in order and gives their `_id`s in the same order |
| `c.update(filter, update, values..)` | `Result<u64, Error>`: updates the first matching document; the number matched, 0 or 1 |
| `c.update_many(filter, update, values..)` | `Result<u64, Error>`: updates every matching document; the number matched |
| `c.upsert(filter, update, values..)` | `Result<bool, Error>`: updates the first matching document, or inserts one built from the filter and the update when none matches; `true` when it inserted |
| `c.replace(doc)` | `Result<bool, Error>`: replaces the document with `doc`'s `_id` by `doc`; `true` when one matched |
| `c.delete(filter, values..)` | `Result<u64, Error>`: deletes the first matching document; 0 or 1 |
| `c.delete_many(filter, values..)` | `Result<u64, Error>`: deletes every matching document; the number deleted |
| `c.create_index(keys, options)` | `Result<bool, Error>`: creates the index; `true` (see [Indexes](#indexes)) |

```varyk
let one: u64 = users.update("{ _id: ? }", "{ $set: { name: ? }, $inc: { age: 1 } }", id, "Ada L.").await?;
let added: bool = users.upsert("{ email: ? }", "{ $set: { name: ? }, $setOnInsert: { created_at: ? } }", email, name, Time::now()).await?;
let gone: u64 = users.delete_many("{ city: ? }", "Paris").await?;
```

A document is a struct, or a `HashMap<string, V>` whose keys become its
fields; `insert_many` takes a `Vec` of them. The calls read the document
and do not take it, so it stays usable after the call. The values of an
update call fill the filter's `?`s first, then the update's.

- An update is a document of update operators (`"{ $set: { name: ? } }"`)
  or an array of pipeline stages. Every top-level key of the document
  starts with `$`; to write a whole document, use `replace`.
- `insert` gives the `_id` as text: a `Uuid` with hyphens, a `string` as
  it is, an integer in decimal, and an ObjectId the driver made, because
  the document had no `_id` or a `None` one, as 24 hex digits.
- `insert_many` of an empty `Vec` gives an empty `Vec` and sends nothing.
  It stops at the first document that fails: the documents before it
  stay inserted (outside a transaction), and the `Error` is the server's
  message after "document N of the list:", N the failing document's
  place, counting from 1.
- `replace` finds the document by `doc`'s `_id`; a `doc` without an
  `_id`, or with a `None` one, is an `Error`, and so is a map whose
  first key starts with `$`: MongoDB reads that as an update. Nothing is
  sent, so a transaction stays usable.

## Indexes

`create_index` takes the index's keys and its options, two texts with
no `?`:

```varyk
users.create_index("{ email: 1 }", "{ unique: true }").await?;
users.create_index("{ city: 1, age: -1 }", "{}").await?;
sessions.create_index("{ created_at: 1 }", "{ expireAfterSeconds: 86400 }").await?;
```

The options are `unique`, `name`, `sparse`, and `expireAfterSeconds`,
which makes a TTL index on a `Time` field: MongoDB deletes each document
that many seconds after its time. `"{}"` is no options, and any other
key is an `Error` naming it, so a typo is not ignored.

Creating an index that exists with the same options does nothing, so a
service creates its indexes at startup, before it serves, as above. An
index with the same keys and other options is the server's `Error`.

## The query text

A filter, an update, a pipeline, a sort, and an index's keys and options
are each a text in the syntax of the shell, written as a literal string:
a query built from input does not compile. A `?` stands where a value
goes, and the values follow the text, one per `?`, from left to right
across all the texts of the call. This is the package's guard against
injection: a value is always data, so input cannot become an operator
(`{ "$ne": null }`) or a part of the query.

- objects `{ key: value, … }` and arrays `[ value, … ]`, a trailing
  comma allowed;
- a key bare (letters, digits, `_`, `$`, and `.`, as in `$gte` and
  `address.city`) or in quotes;
- a string in `'single'` or `"double"` quotes, with JSON's escapes; a
  `?` inside quotes is text;
- a whole number (int32, or int64 when it does not fit), a number with
  `.` or an exponent (a double), `true`, `false`, `null`;
- `?`, a value;
- `ObjectId(?)`, an ObjectId made from a value, a `string` of 24 hex
  digits (see [Ids](#ids)).

A filter, a sort, and an index's keys and options are a document, a
pipeline is an array of documents, and an update either. The value
beside the text is a `bool`, a `string`, an integer up to `i64`, a
float, a `Time`, a `Uuid`, a `Bytes`, or an `Option` of one.

The rules that keep input out of the query, each an `Error` naming the
place in the text before anything is sent:

- The number of `?`s across the call's texts must be the number of
  values; the message names both.
- `?` is never a key: `{ ?: 1 }` is an `Error`, and so is a key written
  twice in one document, whose second value would hide the first.
- A value stands only where MongoDB reads plain data:
  - anywhere in a filter, and in a top-level `$match` stage of a
    pipeline, except under `$expr` and `$jsonSchema`;
  - as a value under the update operators `$set`, `$setOnInsert`,
    `$inc`, `$mul`, `$min`, `$max`, `$push`, and `$addToSet`, at any
    depth (`{ $set: { profile: { name: ? } } }`), where `$each` is the
    only `$` key allowed above a `?`; and in the condition of `$pull`;
  - as the number of a top-level `$skip` or `$limit` stage.

  Anywhere else a `?` is an `Error`, "a value can stand only in a
  filter, a `$match` stage, a value of `$set` or a like update operator,
  or `$skip` and `$limit`": in other stages, in sub-pipelines, in a sort
  or an index, in an update written as stages, and under other update
  operators. There MongoDB reads a string as a field path
  (`"$passwordHash"`), a variable, or a collection name, so a value from
  input could read another field or another collection. A pipeline that
  needs a value puts it in its `$match` stage.
- A `None` value cannot be matched: in a filter, a `$match` stage, or a
  `$pull` condition it is an `Error`, since `{ reset_token: null }`
  matches every document without a reset token. Write `null` in the text
  to match a missing or null field. Under `$set` and the other update
  operators a `None` stores null.
- `$where`, `$function`, and `$accumulator`, which run JavaScript on the
  server, are an `Error` as a key anywhere in a text.
- A value is never a pattern. A `?` under `$regex` takes a `string`, and
  the package escapes every character with a meaning in a pattern, so
  the value matches its own text and nothing else:
  `users.all("{ name: { $regex: ?, $options: 'i' } }", search)` finds
  every name that holds `search`, in any case. A `?` as `$options` is an
  `Error`. A pattern of the program's own is written in the text
  (`{ sku: { $regex: '^A-[0-9]+$' } }`).
- `$text`'s `$search` takes a value, which MongoDB reads as its search
  syntax: a `"` makes a phrase and a `-` leaves a word out, which
  changes only which documents match.

Every text is a literal, so a mistake in one shows on the first run of
its call, normally in a test.

## Documents and types

A document is written from a struct, and read into one, by field name.
`#[rename("..")]`, `#[skip]`, and `#[default(..)]` work as they do for
JSON.

| Varyk | Stored as |
|---|---|
| `bool`, `string` | boolean, string |
| `f32`, `f64` | double |
| `i8`, `i16`, `i32`, `u8`, `u16` | int32 |
| `i64`, `u32` | int64 |
| `u64`, `usize` | int64; above `i64`'s largest, an `Error` naming the field |
| `Time` | date, in milliseconds |
| `Uuid` | binary subtype 4, which Compass shows as `UUID("…")` |
| `Bytes` | binary |
| `Option` | null for `None`, the value for `Some` |
| `Vec<T>` | array |
| a struct, a `HashMap<string, V>` | embedded document |
| an enum whose variants carry no data | its name, or its `#[rename]` |

These are real BSON types, so TTL indexes, date operators, Compass, and
other drivers see dates, ids, and binary data as such.

When a document is read, fields the struct has no field for are
ignored; a field with no value in the document is an `Error` naming it,
unless it is an `Option` (then `None`) or has `#[default]`. A null goes
into an `Option` as `None` and into any other field as an `Error`
naming the field. A number reads into an integer field when it is
whole and in the field's range, and into a float field. A string reads
into a `Time` or a `Uuid` when it is one; an ObjectId and a Decimal128
read into a `string` as their text. A value the field does not take is
an `Error` naming the field, never the value.

When `T` is a number, `bool`, `string`, `Time`, `Uuid`, or `Bytes`, the
document has exactly one field other than `_id`, or `_id` alone, as an
aggregation's result often has: `[{ $group: { _id: '$city' } }]` reads
into a `Vec<string>`.

### Dates

BSON dates hold milliseconds, so a `Time`'s microseconds are dropped
when it is stored, rounding toward the past: two `Time`s that differ
only in microseconds are equal once stored. A `Time` beside a text is
rounded the same way, so a `Time` read back from a document matches its
own value.

### Ids

A new collection uses `#[rename("_id")] id: Uuid` and `Uuid::new()`, as
the first program does. The `_id` may be a `Uuid`, a `string`, or an
integer; any other type is an `Error`. A string of 24 hex digits is
stored as a string, never as an ObjectId.

An existing collection whose `_id`s are ObjectIds reads them into a
`string` as 24 hex digits, and queries them with `ObjectId(?)`:

```varyk
struct Item {
    #[rename("_id")]
    id: Option<string>,
    name: string,
}

let item: Option<Item> = items.first("{ _id: ObjectId(?) }", id).await?;
let changed = items.update("{ _id: ObjectId(?) }", "{ $set: { name: ? } }", id, "Lamp").await?;
```

An `Item` inserted with `id: None` gets an ObjectId the driver makes,
which `insert` gives back as text. An `Item` read this way and written back
has a `string` `_id`, not an ObjectId, so `replace` matches nothing and
gives `false`: update such a collection with `update` and
`ObjectId(?)`, as above.

## Transactions

```varyk
struct Order {
    #[rename("_id")]
    id: Uuid,
    item: string,
    count: i64,
}

async fn place(db: mongo::Database, order: Order) -> Result<bool, Error> {
    let mut tx = db.begin().await?;
    let orders = tx.collection("orders");
    let stock = tx.collection("stock");
    orders.insert(order).await?;
    let taken = stock.update("{ _id: ?, left: { $gte: ? } }", "{ $inc: { left: ? } }", order.item, order.count, -order.count).await?;
    if taken == 0 {
        return Err(Error::new("out of stock"));   // drops tx: rolls back the insert
    }
    tx.commit().await
}
```

| Call | Gives |
|---|---|
| `db.begin()` | `Result<mongo::Tx, Error>`: starts a transaction, reading from the primary whatever the URL's `readPreference` |
| `tx.collection(name)` | `mongo::Collection`: a collection whose calls all run inside the transaction |
| `tx.commit()` | `Result<bool, Error>`: commits; `true` |

A transaction's collections take every call of a collection.
`commit` changes the `Tx`, so it is `let mut`. A `Tx` dropped without
`commit` rolls back, so an early `return` or a `?` undoes it; there is
no `abort` call. A `Tx` still open when the program exits is ended by
the server after 60 seconds by default (its
`transactionLifetimeLimitSeconds`). Make indexes at startup, outside a
transaction: MongoDB refuses `create_index` inside one on a collection
that exists.

When a call inside the transaction fails in MongoDB, its `Error` is the
server's or the driver's message followed by its fate. For a duplicate
key, say, that is "; the transaction will roll back"; for a write
conflict or a lost connection, which MongoDB calls temporary, it is the
sentence described below under "Running it again". From
then on every call on its collections is an `Error`, "a call in this
transaction failed; it will roll back", without reaching the server,
and `commit` rolls it back and is an `Error`, "the transaction was
rolled back: a call in it failed". A call stopped while it waits on
MongoDB, as `Task::all` stops the other tasks after an `Err`, fails the
transaction the same way, since its write may or may not have applied.
An `Error` of the package's own, in a text, a value, or reading into
`T`, leaves the transaction as it was. After any `commit` the `Tx` is
finished, and a second `commit`, or a call on one of its collections,
is an `Error`, "this transaction is finished".

### Running it again

A write conflict between two transactions, or an election, makes a
failure MongoDB calls temporary: running the whole transaction again
usually works. Such a message ends "the transaction hit a temporary
conflict; run it again", and only the program can run it again, as
here, up to three times:

```varyk
async fn place_order(db: mongo::Database, order: Order) -> Result<bool, Error> {
    for _ in 0..2 {
        match place(db, order).await {
            Ok(done) => {
                return Ok(done);
            }
            Err(e) => {
                if !e.message().contains("run it again") {
                    return Err(e);
                }
            }
        }
    }
    place(db, order).await
}
```

When the driver's own retry of `commit` cannot tell whether the commit
applied, the `Error` is "it is not known whether the transaction
committed". It never says to run it again: the commit may have applied.

Transactions need a replica set or a sharded cluster; Atlas is one. On
a single server that is not a replica set, the first call inside a
transaction is an `Error`, "transactions need a replica set or a
sharded cluster". For development, run a replica set of one server, as
below.

## A server for development

A replica set of one server, in Docker:

```sh
docker run -d --name mongo -p 27017:27017 mongo:8.0 --replSet rs0 --bind_ip_all
docker exec mongo mongosh --quiet --eval "rs.initiate({ _id: 'rs0', members: [{ _id: 0, host: '127.0.0.1:27017' }] })"
```

The member's host must be one the program can reach, which the
container's own name is not; `127.0.0.1` is. After a few seconds
`docker exec mongo mongosh --quiet --eval "db.hello().isWritablePrimary"`
prints `true`, and `MONGO_URL=mongodb://127.0.0.1:27017/app` reaches
it. When port 27017 is taken, run the server on another port, and name
the member and the URL after it:

```sh
docker run -d --name mongo -p 27018:27018 mongo:8.0 --replSet rs0 --bind_ip_all --port 27018
docker exec mongo mongosh --port 27018 --quiet --eval "rs.initiate({ _id: 'rs0', members: [{ _id: 0, host: '127.0.0.1:27018' }] })"
```

The package is tested against MongoDB 8.0.

## A server's state

`mongo::Database` is cheap to clone, so it is the state of a
[varyk-http](https://github.com/Varyk-Lang/varyk-http) app, and a
readiness check sends `ping`:

```varyk
struct State {
    db: mongo::Database,
}

async fn get_user(id: Uuid, state: Shared<State>) -> Result<Option<User>, Error> {
    state.db.collection("users").first("{ _id: ? }", id).await
}

async fn ready(state: Shared<State>) -> Result<bool, Error> {
    state.db.ping().await
}

fn build_app(state: Shared<State>) -> http::App {
    let mut app = http::App::new(state.clone());
    app.get("/users/{id}", get_user);
    app.get("/ready", ready);
    app
}
```

`main` connects, creates the indexes, and then serves
`build_app(Shared::new(State { db: db }))`.

## Errors

Every failure is an `Error` whose message the package writes, and no
call panics.

- A server error keeps the server's message and code:
  `E11000 duplicate key error collection: app.users index: email_1 dup
  key: { email: "ada@example.com" } (code 11000)`. MongoDB puts the
  duplicate value in that message, so treat these messages as data: log
  them, and give clients a message of your own.
- A connection failure says which step failed and never contains the
  URL. A lost connection or a timeout after connecting gives the
  driver's message, which names hosts, never a password.
- A mistake in a text names its place (`the filter text, at character
  12: ...`), and a value or a field that cannot be read names the
  value's number or the field, never the value.

The driver's command logging stays off, since it would log every
command with its values, and the package logs nothing of its own.

## Testing

MongoDB has no in-memory mode, so tests run against a server, such as
the one under [A server for development](#a-server-for-development):

```varyk
async fn add_ada() -> Result<u64, Error> {
    let config: Config = env::parse()?;
    let db = mongo::connect(config.mongo_url).await?;
    let users = db.collection("adds_a_user");
    users.delete_many("{}").await?;
    let ada = User { id: Uuid::new(), email: "ada@example.com", name: "Ada", created_at: Time::now() };
    users.insert(ada).await?;
    users.count("{ email: ? }", "ada@example.com").await
}

#[test]
async fn adds_a_user() {
    let added: Result<u64, Error> = add_ada().await;
    match added {
        Ok(n) => assert_eq(n, 1),
        Err(e) => assert_eq(e.message(), "no error"),
    }
}
```

```sh
MONGO_URL=mongodb://127.0.0.1:27017/app_test varyk test
```

A test returns nothing, so the work goes in a function that returns a
`Result`, and the test opens it with `match` and `assert_eq`. Tests run
in parallel, so each works in a collection of its own, here named after
the test, and deletes its documents first, so a rerun starts clean. A
database of its own for the tests (`app_test`) keeps them away from the
program's data.

## Not in this version

A client with several databases; cursors; change streams; GridFS; bulk
writes; `find_one_and_update` and the other find-and-modify calls; a
projection; `distinct`; a transaction call that retries by itself; a
pattern taken from input; a list beside the text (`{ _id: { $in: ? } }`:
write one `?` for each element); collection names chosen at run time;
index options other than the four above; Decimal128, regex, and
timestamp values beside the text; client-side field-level encryption;
Atlas Search helpers.

## Versions

A program and varyk-mongo must resolve to one `varyk-std`, or the
generated Rust has two `Error` types, so each minor varyk release is
followed by a varyk-mongo release. varyk-mongo needs varyk-std 0.8.1 or
later, which hands `Time`, `Uuid`, and `Bytes` to the package as typed
values.

| varyk-mongo | varyk | Rust |
|---|---|---|
| 0.1 | 0.8.1 and later 0.8 releases | 1.88 or later |

## Security

Report a vulnerability as described in the
[security policy](https://github.com/Varyk-Lang/varyk-mongo/security/policy)
shared by every Varyk-Lang repository.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md); rules for AI coding agents are
in [AGENTS.md](AGENTS.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option. The name "Varyk" is a
trademark and is not covered by those licenses; see Varyk's
[TRADEMARKS.md](https://github.com/Varyk-Lang/varyk/blob/main/TRADEMARKS.md).
MongoDB is a trademark of MongoDB, Inc.; varyk-mongo is not affiliated
with or endorsed by MongoDB, Inc.
