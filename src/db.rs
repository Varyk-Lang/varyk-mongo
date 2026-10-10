// The facade of varyk-mongo: the Rust that Varyk programs reach through
// `mongo::connect`, `mongo::Database`, `mongo::Collection`, `mongo::Find`,
// and `mongo::Tx`. Every driver `Result` is mapped to a `varyk_std::Error`
// whose message the package writes and which names no URL and no
// password, and nothing here panics.

use crate::text::{self, Kind, Query};
use mongodb::bson::{Bson, Document, RawDocumentBuf};
use mongodb::error::{
    ErrorKind, TRANSIENT_TRANSACTION_ERROR, UNKNOWN_TRANSACTION_COMMIT_RESULT, WriteFailure,
};
use mongodb::options::{
    ClientOptions, ConnectionString, FindOptions, IndexOptions, ReadPreference, SelectionCriteria,
    UpdateModifications,
};
use mongodb::{ClientSession, IndexModel, SessionCursor};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};

/// The server's code for a refused sign-in, `AuthenticationFailed`.
const AUTHENTICATION_FAILED: i32 = 18;

/// The server's code for an operation the deployment cannot do,
/// `IllegalOperation`, which a standalone server gives a call inside a
/// transaction. It is a general code, so its message is checked too.
const ILLEGAL_OPERATION: i32 = 20;

/// The ending of a transaction's failure MongoDB labels transient.
const TEMPORARY_CONFLICT: &str = "the transaction hit a temporary conflict; run it again";

/// Sends `$action`, a driver action, inside the transaction `$entered`
/// holds, or on its own outside one.
macro_rules! sent {
    ($entered:expr, $action:expr) => {
        match $entered.session() {
            Some(session) => $action.session(session).await,
            None => $action.await,
        }
    };
}

/// The database the URL names, on a pool of connections to its server.
/// `clone` gives another handle on the same pool.
#[derive(Clone)]
pub struct Database {
    inner: mongodb::Database,
}

/// A collection of a `Database`, or of a `Tx`, whose calls then all run
/// inside its transaction. `clone` gives another handle on the same
/// collection and pool, and transaction.
#[derive(Clone)]
pub struct Collection {
    inner: mongodb::Collection<Document>,
    /// The transaction the calls run in, for a collection a `Tx` gave.
    tx: Option<Arc<Mutex<Shared>>>,
}

/// A transaction: a session with a transaction started on it. Its
/// collections run their calls inside it, and `commit` ends it. A `Tx`
/// dropped without `commit`, with its collections, rolls back: the driver
/// aborts a session's open transaction when the session is dropped.
pub struct Tx {
    db: mongodb::Database,
    shared: Arc<Mutex<Shared>>,
}

/// What a `Tx` and its collections share: a `Collection` cannot reach its
/// `Tx`, so the flags live beside the session. Behind tokio's mutex, held
/// across a call's `.await`, so one call at a time uses the session and a
/// started call can still move between threads.
struct Shared {
    session: ClientSession,
    /// Set while a call is in flight, and kept when that call failed in
    /// the driver or the server or was stopped before it finished, since
    /// its write may or may not have applied: the transaction will roll
    /// back, and no later call is sent.
    failed: bool,
    /// Set by `commit`, whatever its outcome.
    finished: bool,
}

/// Connects to the database `url` names: reads the URL with no network,
/// resolves a `mongodb+srv` name, and sends `ping`, so a wrong URL, an
/// unreachable server, or a refused password fails here rather than at
/// the first call. The wait for the server is the URL's
/// `serverSelectionTimeoutMS`, 30 seconds unless it says otherwise.
pub async fn connect(url: &str) -> Result<Database, varyk_std::Error> {
    let (parsed, name) = read_url(url)?;
    let options = ClientOptions::parse(parsed).await.map_err(resolve_error)?;
    // `with_options` only checks the options again; it sends nothing.
    let client = mongodb::Client::with_options(options).map_err(|_| cannot_be_read())?;
    let db = Database {
        inner: client.database(&name),
    };
    db.ping().await?;
    Ok(db)
}

/// The driver's reading of `url` and the name of the database in its
/// path. No network is used.
fn read_url(url: &str) -> Result<(ConnectionString, String), varyk_std::Error> {
    // The driver's parse errors repeat option values, which may be a
    // password, so none of their text is kept.
    let parsed = ConnectionString::parse(url).map_err(|_| cannot_be_read())?;
    match parsed.default_database.clone() {
        Some(name) if !name.is_empty() => Ok((parsed, name)),
        _ => Err(varyk_std::Error::new(
            "the MongoDB URL names no database; add one after the host, as in `mongodb://localhost:27017/app`"
                .to_string(),
        )),
    }
}

impl Database {
    /// A handle to the collection `name`. Nothing is sent: MongoDB makes
    /// a collection on its first write.
    pub fn collection(&self, name: &'static str) -> Collection {
        Collection {
            inner: self.inner.collection(name),
            tx: None,
        }
    }

    /// Starts a session and a transaction on it, reading from the primary
    /// whatever the URL's `readPreference`, since MongoDB refuses any other
    /// inside a transaction.
    pub async fn begin(&self) -> Result<Tx, varyk_std::Error> {
        let mut session = self
            .inner
            .client()
            .start_session()
            .await
            .map_err(driver_error)?;
        session
            .start_transaction()
            .selection_criteria(SelectionCriteria::ReadPreference(ReadPreference::Primary))
            .await
            .map_err(driver_error)?;
        Ok(Tx {
            db: self.inner.clone(),
            shared: Arc::new(Mutex::new(Shared {
                session,
                failed: false,
                finished: false,
            })),
        })
    }

    /// Sends `ping` to the server: `true` when it answers, for a
    /// readiness check.
    pub async fn ping(&self) -> Result<bool, varyk_std::Error> {
        let mut command = Document::new();
        command.insert("ping", 1);
        self.inner
            .run_command(command)
            .await
            .map_err(driver_error)?;
        Ok(true)
    }
}

impl Collection {
    /// The first document `filter` matches, read as a `T`; an `Error`
    /// when none matches.
    pub async fn one<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<T, varyk_std::Error> {
        self.find(filter, values).one().await
    }

    /// The first document `filter` matches, read as a `T`, or `None`.
    pub async fn first<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<Option<T>, varyk_std::Error> {
        self.find(filter, values).first().await
    }

    /// Every document `filter` matches, each read as a `T`.
    pub async fn all<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<Vec<T>, varyk_std::Error> {
        self.find(filter, values).all().await
    }

    /// The number of documents `filter` matches.
    pub async fn count(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<u64, varyk_std::Error> {
        let filter = filter_of(filter, &values)?;
        let mut entered = self.enter().await?;
        let counted = sent!(entered, self.inner.count_documents(filter));
        entered.checked(counted)
    }

    /// A query for the documents `filter` matches, sent by its `one`,
    /// `first`, or `all`. Nothing is checked or sent here: an error in
    /// the text or the values is the `Error` of the call that sends it.
    pub fn find(&self, filter: &'static str, values: Vec<varyk_std::Value>) -> Find {
        Find {
            collection: self.clone(),
            filter,
            values,
            sort: None,
            skip: None,
            limit: None,
        }
    }

    /// Inserts `doc`, a struct or a `HashMap<string, V>`, and gives its
    /// `_id` as text: the one it has, or the ObjectId the driver made for
    /// a document without one.
    pub async fn insert<T: varyk_std::serde::Serialize + ?Sized>(
        &self,
        doc: &T,
    ) -> Result<String, varyk_std::Error> {
        let doc = crate::write::document(doc)?;
        let mut entered = self.enter().await?;
        let inserted = sent!(entered, self.inner.insert_one(doc));
        crate::write::id_text(&entered.checked(inserted)?.inserted_id)
    }

    /// Inserts `docs`, a `Vec` of structs or maps, in order, and gives
    /// their `_id`s in the same order. An empty `Vec` gives an empty one
    /// without sending. On a failure the documents before the failing one
    /// stay inserted.
    pub async fn insert_many<T: varyk_std::serde::Serialize + ?Sized>(
        &self,
        docs: &T,
    ) -> Result<Vec<String>, varyk_std::Error> {
        let docs = crate::write::documents(docs)?;
        if docs.is_empty() {
            // Nothing is sent, but a finished or failed `Tx` still says so.
            self.open().await?;
            return Ok(Vec::new());
        }
        let sent = docs.len();
        let mut entered = self.enter().await?;
        let inserted = sent!(entered, self.inner.insert_many(docs));
        let inserted = entered.checked(inserted)?;
        (0..sent)
            .map(|place| match inserted.inserted_ids.get(&place) {
                Some(id) => crate::write::id_text(id),
                None => Err(varyk_std::Error::new(
                    "MongoDB did not give the `_id` of every inserted document".to_string(),
                )),
            })
            .collect()
    }

    /// Deletes every document `filter` matches; gives the number deleted.
    pub async fn delete_many(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<u64, varyk_std::Error> {
        let filter = filter_of(filter, &values)?;
        let mut entered = self.enter().await?;
        let deleted = sent!(entered, self.inner.delete_many(filter));
        Ok(entered.checked(deleted)?.deleted_count)
    }

    /// Updates the first document `filter` matches with `update`, a
    /// document of update operators or an array of stages; gives the
    /// number matched, 0 or 1.
    pub async fn update(
        &self,
        filter: &'static str,
        update: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<u64, varyk_std::Error> {
        let (filter, update) = update_of(filter, update, &values)?;
        let mut entered = self.enter().await?;
        let updated = sent!(entered, self.inner.update_one(filter, update));
        Ok(entered.checked(updated)?.matched_count)
    }

    /// Updates every document `filter` matches with `update`; gives the
    /// number matched.
    pub async fn update_many(
        &self,
        filter: &'static str,
        update: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<u64, varyk_std::Error> {
        let (filter, update) = update_of(filter, update, &values)?;
        let mut entered = self.enter().await?;
        let updated = sent!(entered, self.inner.update_many(filter, update));
        Ok(entered.checked(updated)?.matched_count)
    }

    /// Updates the first document `filter` matches with `update`, or,
    /// when none matches, inserts one built from the filter and the
    /// update; `true` when it inserted.
    pub async fn upsert(
        &self,
        filter: &'static str,
        update: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<bool, varyk_std::Error> {
        let (filter, update) = update_of(filter, update, &values)?;
        let mut entered = self.enter().await?;
        let updated = sent!(entered, self.inner.update_one(filter, update).upsert(true));
        Ok(entered.checked(updated)?.upserted_id.is_some())
    }

    /// Replaces the document whose `_id` is `doc`'s with `doc`, a struct
    /// or a map; `true` when one matched. A `doc` without an `_id`, or
    /// with a `None` one, is an `Error`.
    pub async fn replace<T: varyk_std::serde::Serialize + ?Sized>(
        &self,
        doc: &T,
    ) -> Result<bool, varyk_std::Error> {
        let doc = crate::write::document(doc)?;
        // The driver refuses a replacement whose first key starts with
        // `$` (it reads it as an update), and inside a transaction that
        // refusal would fail the transaction though nothing was sent.
        if doc.keys().next().is_some_and(|key| key.starts_with('$')) {
            return Err(varyk_std::Error::new(
                "a replacement document cannot start with a key that begins with `$`, which MongoDB reads as an update; `update` is the call for that"
                    .to_string(),
            ));
        }
        // `write::document` leaves a `None` `_id` out, so both are here.
        let id = match doc.get("_id") {
            Some(id) => id.clone(),
            None => {
                return Err(varyk_std::Error::new(
                    "`replace` finds the document to replace by its `_id`, and this one has no `_id` or a `None` one"
                        .to_string(),
                ));
            }
        };
        let mut filter = Document::new();
        filter.insert("_id", id);
        let mut entered = self.enter().await?;
        let replaced = sent!(entered, self.inner.replace_one(filter, doc));
        Ok(entered.checked(replaced)?.matched_count > 0)
    }

    /// Deletes the first document `filter` matches; gives the number
    /// deleted, 0 or 1.
    pub async fn delete(
        &self,
        filter: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<u64, varyk_std::Error> {
        let filter = filter_of(filter, &values)?;
        let mut entered = self.enter().await?;
        let deleted = sent!(entered, self.inner.delete_one(filter));
        Ok(entered.checked(deleted)?.deleted_count)
    }

    /// Runs `pipeline`, an array of stages, and gives every document it
    /// gives, each read as a `T`.
    pub async fn aggregate<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
        pipeline: &'static str,
        values: Vec<varyk_std::Value>,
    ) -> Result<Vec<T>, varyk_std::Error> {
        counted(&[(Kind::Pipeline, pipeline)], &values)?;
        let stages = text::parse(Kind::Pipeline, pipeline, &values, 0)?
            .0
            .into_stages()?;
        let mut entered = self.enter().await?;
        let aggregate = self.inner.aggregate(stages).with_type::<RawDocumentBuf>();
        let raw = match entered.session() {
            Some(session) => {
                async {
                    let cursor = aggregate.session(&mut *session).await?;
                    drained_in(cursor, session).await
                }
                .await
            }
            None => async { drained(aggregate.await?).await }.await,
        };
        documents_of(entered.checked(raw)?)?
            .into_iter()
            .map(crate::read::from_document)
            .collect()
    }

    /// Creates the index on `keys` (`"{ email: 1 }"`) with `options`
    /// (`"{ unique: true }"`, or `"{}"`); `true`. An index that exists
    /// with the same options is left as it is. Neither text takes a `?`.
    pub async fn create_index(
        &self,
        keys: &'static str,
        options: &'static str,
    ) -> Result<bool, varyk_std::Error> {
        let keys = text::parse(Kind::IndexKeys, keys, &[], 0)?
            .0
            .into_document()?;
        let options = index_options(
            text::parse(Kind::IndexOptions, options, &[], 0)?
                .0
                .into_document()?,
        )?;
        // The keys reach the server through the driver's serde path, which
        // is safe here: the text is a literal that takes no `?`.
        let model = IndexModel::builder().keys(keys).options(options).build();
        let mut entered = self.enter().await?;
        let created = sent!(entered, self.inner.create_index(model));
        entered.checked(created)?;
        Ok(true)
    }

    /// The documents `filter` matches, in the order `options` sort them,
    /// each as the server stored it.
    async fn documents(
        &self,
        filter: Document,
        options: FindOptions,
    ) -> Result<Vec<Document>, varyk_std::Error> {
        // Read raw and converted here, so an embedded document whose keys
        // look like extended JSON (`$regularExpression`) stays a document.
        let raw = self.inner.clone_with_type::<RawDocumentBuf>();
        let find = raw.find(filter).with_options(options);
        let mut entered = self.enter().await?;
        let found = match entered.session() {
            Some(session) => {
                async {
                    let cursor = find.session(&mut *session).await?;
                    drained_in(cursor, session).await
                }
                .await
            }
            None => async { drained(find.await?).await }.await,
        };
        documents_of(entered.checked(found)?)
    }

    /// The transaction's state locked for one call, or nothing for a
    /// collection outside a transaction; an `Error`, with nothing sent,
    /// once its `Tx` is finished or a call in it failed.
    ///
    /// The transaction is marked failed until `Entered::checked` reads the
    /// call's outcome, so a call stopped mid-send (its task stopped, as
    /// `Task::all` stops the others when one fails) fails the transaction
    /// as a failed call does: its write may or may not have reached the
    /// server. Nothing that can fail sits between `enter` and `checked`.
    async fn enter(&self) -> Result<Entered<'_>, varyk_std::Error> {
        let mut shared = self.open().await?;
        if let Some(shared) = shared.as_mut() {
            shared.failed = true;
        }
        Ok(Entered { shared })
    }

    /// The transaction's state locked, or `None` outside a transaction;
    /// an `Error` once its `Tx` is finished or a call in it failed.
    async fn open(&self) -> Result<Option<MutexGuard<'_, Shared>>, varyk_std::Error> {
        let Some(tx) = &self.tx else {
            return Ok(None);
        };
        let shared = tx.lock().await;
        if shared.finished {
            return Err(finished());
        }
        if shared.failed {
            return Err(varyk_std::Error::new(
                "a call in this transaction failed; it will roll back".to_string(),
            ));
        }
        Ok(Some(shared))
    }
}

/// A call's hold on its transaction, from `Collection::enter`: the lock
/// on the state a `Tx` shares, kept until the call is done, or nothing
/// outside a transaction.
struct Entered<'a> {
    shared: Option<MutexGuard<'a, Shared>>,
}

impl Entered<'_> {
    /// The transaction's session, to send the call in.
    fn session(&mut self) -> Option<&mut ClientSession> {
        self.shared.as_mut().map(|shared| &mut shared.session)
    }

    /// The outcome of a driver call, its failure mapped. Inside a
    /// transaction it clears the mark `enter` set when the call succeeded,
    /// and a failure fails the transaction, its message saying the
    /// transaction will roll back.
    fn checked<R>(&mut self, outcome: mongodb::error::Result<R>) -> Result<R, varyk_std::Error> {
        if let Some(shared) = self.shared.as_mut() {
            shared.failed = outcome.is_err();
        }
        outcome.map_err(|e| match self.shared {
            Some(_) => in_transaction_error(&e),
            None => driver_error(e),
        })
    }
}

impl Tx {
    /// A handle to the collection `name` whose calls all run inside this
    /// transaction. Nothing is sent.
    pub fn collection(&self, name: &'static str) -> Collection {
        Collection {
            inner: self.db.collection(name),
            tx: Some(Arc::clone(&self.shared)),
        }
    }

    /// Commits the transaction; `true`. When a call in it failed, it
    /// rolls the transaction back instead and is an `Error`. The `Tx` is
    /// finished after any `commit`.
    pub async fn commit(&mut self) -> Result<bool, varyk_std::Error> {
        let mut shared = self.shared.lock().await;
        if shared.finished {
            return Err(finished());
        }
        shared.finished = true;
        if shared.failed {
            // The server may have aborted the transaction already and
            // answer that there is none, so the abort's own result is
            // not read.
            let _ = shared.session.abort_transaction().await;
            return Err(varyk_std::Error::new(
                "the transaction was rolled back: a call in it failed".to_string(),
            ));
        }
        match shared.session.commit_transaction().await {
            Ok(()) => Ok(true),
            // The driver has retried the commit once already; it may have
            // applied, so the message never says to run it again.
            Err(e) if e.contains_label(UNKNOWN_TRANSACTION_COMMIT_RESULT) => {
                Err(varyk_std::Error::new(
                    "it is not known whether the transaction committed".to_string(),
                ))
            }
            Err(e) if e.contains_label(TRANSIENT_TRANSACTION_ERROR) => Err(varyk_std::Error::new(
                format!("{}; {TEMPORARY_CONFLICT}", driver_message(&e)),
            )),
            Err(e) => Err(driver_error(e)),
        }
    }
}

/// The `Error` of a call on a finished `Tx`'s collection, and of a second
/// `commit`.
fn finished() -> varyk_std::Error {
    varyk_std::Error::new("this transaction is finished".to_string())
}

/// The `Error` of a driver failure inside a transaction, which fails it:
/// the driver's or server's message followed by the transaction's fate.
/// A standalone server refuses every call in a transaction with code 20,
/// "Transaction numbers ...", which the driver rewrites to advice to add
/// `retryWrites=false`, wrong here; either text gets the real reason.
fn in_transaction_error(e: &mongodb::error::Error) -> varyk_std::Error {
    let standalone = matches!(
        e.kind.as_ref(),
        ErrorKind::Command(c) if c.code == ILLEGAL_OPERATION
            && (c.message.starts_with("Transaction numbers")
                || c.message.starts_with("This MongoDB deployment does not support retryable writes"))
    );
    varyk_std::Error::new(if standalone {
        "transactions need a replica set or a sharded cluster".to_string()
    } else if e.contains_label(TRANSIENT_TRANSACTION_ERROR) {
        format!("{}; {TEMPORARY_CONFLICT}", driver_message(e))
    } else {
        format!("{}; the transaction will roll back", driver_message(e))
    })
}

/// Every document `cursor` gives, each as the server sent it.
async fn drained(
    mut cursor: mongodb::Cursor<RawDocumentBuf>,
) -> mongodb::error::Result<Vec<RawDocumentBuf>> {
    let mut docs = Vec::new();
    while cursor.advance().await? {
        docs.push(cursor.current().to_owned());
    }
    Ok(docs)
}

/// Every document `cursor`, a cursor of a transaction's `session`, gives,
/// each as the server sent it.
async fn drained_in(
    mut cursor: SessionCursor<RawDocumentBuf>,
    session: &mut ClientSession,
) -> mongodb::error::Result<Vec<RawDocumentBuf>> {
    let mut docs = Vec::new();
    while cursor.advance(session).await? {
        docs.push(cursor.current().to_owned());
    }
    Ok(docs)
}

/// The documents read raw, converted with bson's raw conversion, which
/// keeps an embedded document whose keys look like extended JSON a
/// document.
fn documents_of(raw: Vec<RawDocumentBuf>) -> Result<Vec<Document>, varyk_std::Error> {
    raw.iter()
        .map(|doc| {
            Document::try_from(doc.as_ref()).map_err(|_| {
                varyk_std::Error::new("MongoDB sent a document that is not valid BSON".to_string())
            })
        })
        .collect()
}

/// A query of a `Collection` with its options: the filter text, its
/// values, and an optional sort, skip, and limit. `sort`, `skip`, and
/// `limit` give a new `Find` and send nothing; `one`, `first`, and `all`
/// send it.
#[derive(Clone)]
pub struct Find {
    collection: Collection,
    filter: &'static str,
    values: Vec<varyk_std::Value>,
    sort: Option<&'static str>,
    skip: Option<i64>,
    limit: Option<i64>,
}

impl Find {
    /// The same query sorted by `text`, as `"{ created_at: -1 }"`.
    pub fn sort(&self, text: &'static str) -> Find {
        let mut find = self.clone();
        find.sort = Some(text);
        find
    }

    /// The same query skipping the first `n` documents.
    pub fn skip(&self, n: i64) -> Find {
        let mut find = self.clone();
        find.skip = Some(n);
        find
    }

    /// The same query giving at most `n` documents.
    pub fn limit(&self, n: i64) -> Find {
        let mut find = self.clone();
        find.limit = Some(n);
        find
    }

    /// The first document the query gives, read as a `T`; an `Error` when
    /// it gives none.
    pub async fn one<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
    ) -> Result<T, varyk_std::Error> {
        match self.first().await? {
            Some(found) => Ok(found),
            None => Err(varyk_std::Error::new(
                "no document matches the filter; to read one that may be missing, use `first`"
                    .to_string(),
            )),
        }
    }

    /// The first document the query gives, read as a `T`, or `None`.
    pub async fn first<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
    ) -> Result<Option<T>, varyk_std::Error> {
        let (filter, mut options) = self.prepared()?;
        options.limit = Some(1);
        match self
            .collection
            .documents(filter, options)
            .await?
            .into_iter()
            .next()
        {
            Some(doc) => crate::read::from_document(doc).map(Some),
            None => Ok(None),
        }
    }

    /// Every document the query gives, each read as a `T`.
    pub async fn all<T: varyk_std::serde::de::DeserializeOwned>(
        &self,
    ) -> Result<Vec<T>, varyk_std::Error> {
        let (filter, options) = self.prepared()?;
        self.collection
            .documents(filter, options)
            .await?
            .into_iter()
            .map(crate::read::from_document)
            .collect()
    }

    /// The parsed filter and the driver's options, every text, value,
    /// and count checked.
    fn prepared(&self) -> Result<(Document, FindOptions), varyk_std::Error> {
        // A sort takes no `?`, so the call's values are all the filter's.
        let sort = match self.sort {
            Some(text) => Some(text::parse(Kind::Sort, text, &[], 0)?.0.into_document()?),
            None => None,
        };
        let filter = filter_of(self.filter, &self.values)?;
        let skip = match self.skip {
            Some(n) => Some(u64::try_from(n).map_err(|_| {
                varyk_std::Error::new(
                    "`skip` takes a number of documents from 0 up; this one is negative"
                        .to_string(),
                )
            })?),
            None => None,
        };
        let limit = match self.limit {
            Some(n) if n < 0 => {
                return Err(varyk_std::Error::new(
                    "`limit` takes a number of documents from 1 up; this one is negative"
                        .to_string(),
                ));
            }
            Some(0) => {
                return Err(varyk_std::Error::new(
                    "`limit` takes a number of documents from 1 up; MongoDB reads a limit of 0 as no limit, so leave `limit` out to read every document"
                        .to_string(),
                ));
            }
            other => other,
        };
        let mut options = FindOptions::default();
        options.sort = sort;
        options.skip = skip;
        options.limit = limit;
        Ok((filter, options))
    }
}

/// Checks that `values` has one value for each `?` across `texts`, each
/// a text of its kind; the texts are checked as they are counted, so a
/// text error comes before a count error.
fn counted(texts: &[(Kind, &str)], values: &[varyk_std::Value]) -> Result<(), varyk_std::Error> {
    let mut found: usize = 0;
    for (kind, text) in texts {
        found = found.saturating_add(text::count(*kind, text)?);
    }
    if found != values.len() {
        return Err(text::count_error(found, values.len()));
    }
    Ok(())
}

/// The filter text with its `?`s filled from `values`, after checking
/// that there are as many values as `?`s.
fn filter_of(filter: &str, values: &[varyk_std::Value]) -> Result<Document, varyk_std::Error> {
    counted(&[(Kind::Filter, filter)], values)?;
    text::parse(Kind::Filter, filter, values, 0)?
        .0
        .into_document()
}

/// The filter and the update of an update call, their `?`s filled in
/// order from `values`, the filter's first, after checking that there are
/// as many values as `?`s across both.
fn update_of(
    filter: &str,
    update: &str,
    values: &[varyk_std::Value],
) -> Result<(Document, UpdateModifications), varyk_std::Error> {
    counted(&[(Kind::Filter, filter), (Kind::Update, update)], values)?;
    let (filter, taken) = text::parse(Kind::Filter, filter, values, 0)?;
    let update = match text::parse(Kind::Update, update, values, taken)?.0 {
        Query::Document(operators) => UpdateModifications::Document(operators),
        Query::Stages(stages) => UpdateModifications::Pipeline(stages),
    };
    Ok((filter.into_document()?, update))
}

/// The driver's index options from the options text, whose keys are a
/// fixed list: `unique`, `name`, `sparse`, and `expireAfterSeconds`.
fn index_options(text: Document) -> Result<IndexOptions, varyk_std::Error> {
    let mut options = IndexOptions::default();
    for (key, value) in text {
        match (key.as_str(), value) {
            ("unique", Bson::Boolean(yes)) => options.unique = Some(yes),
            ("sparse", Bson::Boolean(yes)) => options.sparse = Some(yes),
            ("name", Bson::String(name)) => options.name = Some(name),
            ("expireAfterSeconds", Bson::Int32(seconds)) => {
                options.expire_after = Some(seconds_of(i64::from(seconds))?);
            }
            ("expireAfterSeconds", Bson::Int64(seconds)) => {
                options.expire_after = Some(seconds_of(seconds)?);
            }
            ("unique" | "sparse", _) => {
                return Err(varyk_std::Error::new(format!(
                    "the index option `{key}` takes `true` or `false`"
                )));
            }
            ("name", _) => {
                return Err(varyk_std::Error::new(
                    "the index option `name` takes a quoted string".to_string(),
                ));
            }
            ("expireAfterSeconds", _) => return Err(seconds_error()),
            _ => {
                return Err(varyk_std::Error::new(format!(
                    "`{key}` is not an index option; the options are `unique`, `name`, `sparse`, and `expireAfterSeconds`"
                )));
            }
        }
    }
    Ok(options)
}

/// The time of a TTL index, from its number of seconds.
fn seconds_of(seconds: i64) -> Result<Duration, varyk_std::Error> {
    u64::try_from(seconds)
        .map(Duration::from_secs)
        .map_err(|_| seconds_error())
}

fn seconds_error() -> varyk_std::Error {
    varyk_std::Error::new(
        "the index option `expireAfterSeconds` takes a whole number of seconds from 0 up"
            .to_string(),
    )
}

/// The `Error` for a URL the driver cannot read.
fn cannot_be_read() -> varyk_std::Error {
    varyk_std::Error::new(
        "the MongoDB URL cannot be read; it is written as `mongodb://host:27017/app` or `mongodb+srv://cluster.example.net/app`, with any options after `?`"
            .to_string(),
    )
}

/// Maps a failure of resolving the URL's hosts, where a `mongodb+srv`
/// name is looked up and the options are checked together. The resolver's
/// message names the name looked up, so none of its text is kept.
fn resolve_error(e: mongodb::error::Error) -> varyk_std::Error {
    match e.kind.as_ref() {
        ErrorKind::DnsResolve { .. } | ErrorKind::Io(_) => varyk_std::Error::new(
            "the cluster name of the `mongodb+srv` URL cannot be reached; check the name and the network"
                .to_string(),
        ),
        _ => cannot_be_read(),
    }
}

/// The `varyk_std::Error` of a driver error. A refused sign-in gets the
/// `authSource=admin` hint; a server that cannot be reached and a broken
/// connection keep the driver's message, which names hosts and never a
/// password or a value; a server's refusal, a failed write included,
/// keeps its code and message, with the place of the failing document of
/// an `insert_many`. The driver's own messages that hold no value (a
/// failed transaction, a server too old) are kept; every other message
/// is written here.
fn driver_error(e: mongodb::error::Error) -> varyk_std::Error {
    varyk_std::Error::new(driver_message(&e))
}

/// The message of `driver_error`.
fn driver_message(e: &mongodb::error::Error) -> String {
    match e.kind.as_ref() {
        ErrorKind::Authentication { .. } => refused(),
        ErrorKind::Command(c) if c.code == AUTHENTICATION_FAILED => refused(),
        ErrorKind::ServerSelection { message, .. } => {
            format!("cannot reach the MongoDB server: {message}")
        }
        ErrorKind::Io(err) => format!("the connection to the MongoDB server failed: {err}"),
        ErrorKind::ConnectionPoolCleared { message, .. } => {
            format!("the connection to the MongoDB server failed: {message}")
        }
        ErrorKind::Command(c) => server(&c.message, c.code),
        ErrorKind::Write(WriteFailure::WriteError(w)) => server(&w.message, w.code),
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => server(&w.message, w.code),
        ErrorKind::InsertMany(failure) => {
            match (
                failure
                    .write_errors
                    .as_ref()
                    .and_then(|errors| errors.first()),
                failure.write_concern_error.as_ref(),
            ) {
                (Some(w), _) => format!(
                    "document {} of the list: {}",
                    w.index.saturating_add(1),
                    server(&w.message, w.code)
                ),
                (None, Some(w)) => server(&w.message, w.code),
                (None, None) => failed(),
            }
        }
        ErrorKind::Internal { .. }
        | ErrorKind::IncompatibleServer { .. }
        | ErrorKind::SessionsNotSupported
        | ErrorKind::Transaction { .. }
        | ErrorKind::Shutdown => e.kind.to_string(),
        _ => failed(),
    }
}

/// The message of a server's refusal: its message and its code.
fn server(message: &str, code: i32) -> String {
    format!("{message} (code {code})")
}

/// The message of a driver failure whose own text may hold a value (a
/// document it could not write or read, an argument it refused).
fn failed() -> String {
    "the call to MongoDB failed".to_string()
}

/// The message for a user name or password the server refused.
fn refused() -> String {
    "MongoDB refused the user name or password in the URL; a user made in the `admin` database, as Docker's `MONGO_INITDB_ROOT_USERNAME` makes one, needs `?authSource=admin` after the database"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Collection, Database, Tx, connect, read_url};
    use std::collections::HashMap;

    /// The message of a failed result, or "no error".
    fn message_of<T>(result: Result<T, varyk_std::Error>) -> String {
        match result {
            Ok(_) => "no error".to_string(),
            Err(e) => e.message().to_string(),
        }
    }

    #[test]
    fn read_url_gives_the_database_of_the_path() {
        let read = read_url("mongodb://ada:hunter2@127.0.0.1:27017/app?maxPoolSize=5");
        assert!(matches!(read, Ok((_, ref name)) if name == "app"));
    }

    #[test]
    fn read_url_without_a_database_is_an_error_without_the_url() {
        for url in [
            "mongodb://ada:hunter2@127.0.0.1:27017",
            "mongodb://ada:hunter2@127.0.0.1:27017/",
            "mongodb://ada:hunter2@127.0.0.1:27017/?retryWrites=true",
        ] {
            let message = message_of(read_url(url));
            assert!(message.contains("names no database"), "{url}: {message}");
            assert!(!message.contains("hunter2"), "{url}");
            assert!(!message.contains("127.0.0.1"), "{url}");
        }
    }

    #[test]
    fn read_url_that_cannot_be_read_keeps_no_driver_text() {
        for url in [
            "",
            "postgres://ada:hunter2@127.0.0.1/app",
            "mongodb://ada:hunter2@127.0.0.1:27017/app?maxPoolSize=hunter3",
            "mongodb://ada:hunter2@127.0.0.1:27017/app?wTimeoutMS=hunter3",
            "mongodb://ada:hunter2@[bad/app",
        ] {
            let message = message_of(read_url(url));
            assert!(message.contains("cannot be read"), "{url}: {message}");
            assert!(!message.contains("hunter"), "{url}");
        }
    }

    /// varyk-http runs each handler on a task of its own, so every call a
    /// handler awaits must give a future that can move between threads.
    /// Checked by the compiler; nothing runs.
    #[test]
    fn every_call_gives_a_future_that_can_move_between_threads() {
        fn send<F: Send>(future: F) -> F {
            future
        }
        fn database_calls(db: &Database) {
            drop(send(connect("mongodb://localhost/app")));
            drop(send(db.ping()));
            drop(send(db.begin()));
        }
        fn tx_calls(tx: &mut Tx) {
            let c = tx.collection("a");
            drop(send(c.insert(&1)));
            drop(send(c.first::<i64>("{}", Vec::new())));
            drop(send(c.aggregate::<i64>("[]", Vec::new())));
            drop(send(tx.commit()));
        }
        fn collection_calls(c: &Collection, docs: &[HashMap<String, i64>]) {
            drop(send(c.one::<i64>("{}", Vec::new())));
            drop(send(c.first::<i64>("{}", Vec::new())));
            drop(send(c.all::<i64>("{}", Vec::new())));
            drop(send(c.count("{}", Vec::new())));
            drop(send(c.insert(docs)));
            drop(send(c.insert_many(docs)));
            drop(send(c.delete_many("{}", Vec::new())));
            drop(send(c.update("{}", "{ $set: { a: 1 } }", Vec::new())));
            drop(send(c.update_many("{}", "{ $set: { a: 1 } }", Vec::new())));
            drop(send(c.upsert("{}", "{ $set: { a: 1 } }", Vec::new())));
            drop(send(c.replace(docs)));
            drop(send(c.delete("{}", Vec::new())));
            drop(send(c.aggregate::<i64>("[]", Vec::new())));
            drop(send(c.create_index("{ a: 1 }", "{}")));
            let find = c.find("{}", Vec::new()).sort("{ a: 1 }").skip(1).limit(2);
            drop(send(find.one::<i64>()));
            drop(send(find.first::<i64>()));
            drop(send(find.all::<i64>()));
        }
        let _: fn(&Database) = database_calls;
        let _: fn(&mut Tx) = tx_calls;
        let _: fn(&Collection, &[HashMap<String, i64>]) = collection_calls;
    }
}
