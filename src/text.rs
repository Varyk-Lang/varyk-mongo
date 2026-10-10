// The query-text parser: Mongo's shell syntax, with `?` where a value goes.
//
// A text is parsed straight into BSON, never through extended JSON, whose
// reader turns `$regularExpression`, `$date`, `$oid`, and `$binary` keys
// into real BSON types; here they are ordinary keys. Each `?` takes the
// next of the call's values, which becomes one BSON value and is never
// read as text, and only where the allowlist of the spec's section 4 lets
// a value stand. Every message names the kind of text and a character
// position, or a value's number, and never holds a value.

use mongodb::bson::oid::ObjectId;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Document};
use varyk_std::{Error, Value};

/// How deep documents and arrays may nest in a text.
const MAX_DEPTH: usize = 64;

/// The update operators whose fields take values.
const UPDATE_OPERATORS: [&str; 8] = [
    "$set",
    "$setOnInsert",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$push",
    "$addToSet",
];

/// The keys that run JavaScript on the server, refused anywhere.
const JAVASCRIPT: [&str; 3] = ["$where", "$function", "$accumulator"];

const NOT_HERE: &str = "a value can stand only in a filter, a `$match` stage, a value of `$set` or a like update operator, or `$skip` and `$limit`";
const NONE_MATCH: &str =
    "a `None` value cannot be matched; write `null` in the text to match a missing or null field";
const NOT_A_PATTERN: &str = "the value of `$regex` must be a `string`; it is matched as plain text";
const OPTIONS: &str =
    "the options of `$regex` are part of the query; write them in the text, as `$options: 'i'`";
const OBJECT_ID: &str = "`ObjectId(?)` takes a string of 24 hex digits";
const OBJECT_ID_FORM: &str = "`ObjectId` takes a `?`, as `ObjectId(?)`";
const STAGE: &str = "each stage of a pipeline is a document, as `{ $match: { name: ? } }`";
const UPDATE_TOP: &str = "each top-level key of an update starts with `$`, as `$set`; `replace` is the call for a whole document";
const UPDATE_EMPTY: &str = "an update names at least one operator, as `$set`";
const NUMBER: &str = "a number goes here, as `-1` or `2.5`";

/// What a text is for, which decides its shape and where a `?` may stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Filter,
    Update,
    Pipeline,
    Sort,
    IndexKeys,
    IndexOptions,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Filter => "filter",
            Kind::Update => "update",
            Kind::Pipeline => "pipeline",
            Kind::Sort => "sort",
            Kind::IndexKeys => "index keys",
            Kind::IndexOptions => "index options",
        }
    }

    fn shape(self) -> &'static str {
        match self {
            Kind::Filter => "a filter is a document, as `{ name: ? }`",
            Kind::Update => {
                "an update is a document of operators, as `{ $set: { name: ? } }`, or an array of stages"
            }
            Kind::Pipeline => "a pipeline is an array of stages, as `[{ $match: { name: ? } }]`",
            Kind::Sort => "a sort is a document, as `{ created_at: -1 }`",
            Kind::IndexKeys => "index keys are a document, as `{ email: 1 }`",
            Kind::IndexOptions => "index options are a document, as `{ unique: true }`",
        }
    }
}

/// A parsed text: a document, or an array of stages. A filter, sort,
/// index keys, and index options are always a document, a pipeline always
/// stages, and an update either.
#[derive(Debug, PartialEq)]
pub enum Query {
    Document(Document),
    Stages(Vec<Document>),
}

impl Query {
    /// The document, for a kind that is always one.
    pub fn into_document(self) -> Result<Document, Error> {
        match self {
            Query::Document(document) => Ok(document),
            Query::Stages(_) => Err(Error::new("the text is not a document".to_string())),
        }
    }

    /// The stages, for a pipeline.
    pub fn into_stages(self) -> Result<Vec<Document>, Error> {
        match self {
            Query::Stages(stages) => Ok(stages),
            Query::Document(_) => Err(Error::new("the text is not an array of stages".to_string())),
        }
    }
}

/// Parses `text`, a text of `kind`, filling its `?`s in order from
/// `values`, starting at `values[first]`. `values` are all of the call's
/// values, so a value's error names its number across the call. Gives the
/// BSON and the number of `?`s it took; the caller checks the count of
/// every text of the call against `values` first, with `count`.
pub fn parse(
    kind: Kind,
    text: &str,
    values: &[Value],
    first: usize,
) -> Result<(Query, usize), Error> {
    let mut parser = Parser::new(kind, text, Some(values), first);
    let query = parser.text()?;
    Ok((query, parser.taken))
}

/// The number of `?`s in `text`, a text of `kind`, which counts
/// `ObjectId(?)` as one. The text is checked as `parse` checks it, short
/// of the values.
pub fn count(kind: Kind, text: &str) -> Result<usize, Error> {
    let mut parser = Parser::new(kind, text, None, 0);
    parser.text()?;
    Ok(parser.taken)
}

/// The error of a call whose texts have `found` `?`s and which gives
/// `given` values.
pub fn count_error(found: usize, given: usize) -> Error {
    let take = if found == 1 { "value" } else { "values" };
    let were = if given == 1 {
        "value was"
    } else {
        "values were"
    };
    Error::new(format!(
        "this call's texts take {found} {take}, one for each `?`, but {given} {were} given"
    ))
}

/// Where a value is being read, which decides whether a `?` may stand
/// there and what its children are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// No `?` may stand here or below.
    Closed,
    /// Query context: a filter, a top-level `$match` stage, a `$pull`
    /// condition. A `None` value is refused.
    Query,
    /// The value of `$regex` in query context: a `?` is escaped text.
    Regex,
    /// The value of `$options` in query context: no `?`.
    Options,
    /// Under a field of an update operator; `each` lets `$each` stand
    /// above a `?` (under `$push` and `$addToSet`).
    Data { each: bool },
    /// The value of a top-level `$skip` or `$limit` stage.
    Count,
    /// The top-level array of a pipeline, or of an update given as stages.
    Stages { update: bool },
    /// A top-level stage of a pipeline.
    Stage,
    /// The top-level document of an update.
    Operators,
    /// The document of `$set` or a like operator.
    Fields { each: bool },
    /// The document of `$pull`.
    Pull,
}

impl Place {
    /// The place of the value under `key` in a document at this place.
    fn member(self, key: &str) -> Place {
        let operator = key.starts_with('$');
        match self {
            Place::Query => match key {
                "$expr" | "$jsonSchema" => Place::Closed,
                "$regex" => Place::Regex,
                "$options" => Place::Options,
                _ => Place::Query,
            },
            Place::Data { each } if !operator || (each && key == "$each") => Place::Data { each },
            Place::Fields { each } if !operator => Place::Data { each },
            Place::Pull if !operator => Place::Query,
            Place::Stage => match key {
                "$match" => Place::Query,
                "$skip" | "$limit" => Place::Count,
                _ => Place::Closed,
            },
            Place::Operators => match key {
                "$pull" => Place::Pull,
                "$push" | "$addToSet" => Place::Fields { each: true },
                _ if UPDATE_OPERATORS.contains(&key) => Place::Fields { each: false },
                _ => Place::Closed,
            },
            _ => Place::Closed,
        }
    }

    /// The place of an element of an array at this place.
    fn element(self) -> Place {
        match self {
            Place::Query | Place::Data { .. } => self,
            Place::Stages { update: false } => Place::Stage,
            _ => Place::Closed,
        }
    }
}

struct Parser<'a> {
    kind: Kind,
    chars: Vec<char>,
    /// The index in `chars` of the next character to read.
    at: usize,
    /// The call's values, or `None` when only counting.
    values: Option<&'a [Value]>,
    /// The index in `values` of the next value to take.
    next: usize,
    taken: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(kind: Kind, text: &str, values: Option<&'a [Value]>, first: usize) -> Parser<'a> {
        Parser {
            kind,
            chars: text.chars().collect(),
            at: 0,
            values,
            next: first,
            taken: 0,
            depth: 0,
        }
    }

    /// The whole text, of the kind's shape.
    fn text(&mut self) -> Result<Query, Error> {
        self.skip_space();
        let start = self.at;
        let place = match (self.kind, self.peek()) {
            (_, None) => {
                return Err(Error::new(format!(
                    "the {} text is empty",
                    self.kind.name()
                )));
            }
            (Kind::Filter, Some('{')) => Place::Query,
            (Kind::Sort | Kind::IndexKeys | Kind::IndexOptions, Some('{')) => Place::Closed,
            (Kind::Update, Some('{')) => Place::Operators,
            (Kind::Update, Some('[')) => Place::Stages { update: true },
            (Kind::Pipeline, Some('[')) => Place::Stages { update: false },
            _ => return Err(self.text_error(self.at, self.kind.shape())),
        };
        let value = self.value(place)?;
        self.skip_space();
        if self.peek().is_some() {
            return Err(self.text_error(self.at, "the text goes on after its end"));
        }
        match value {
            // The driver refuses an empty update document, and inside a
            // transaction its refusal would fail the transaction.
            Bson::Document(document) if self.kind == Kind::Update && document.is_empty() => {
                Err(self.text_error(start, UPDATE_EMPTY))
            }
            Bson::Document(document) => Ok(Query::Document(document)),
            Bson::Array(items) => {
                let mut stages = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Bson::Document(stage) => stages.push(stage),
                        _ => return Err(self.text_error(0, STAGE)),
                    }
                }
                Ok(Query::Stages(stages))
            }
            _ => Err(self.text_error(0, self.kind.shape())),
        }
    }

    fn value(&mut self, place: Place) -> Result<Bson, Error> {
        self.skip_space();
        let start = self.at;
        match self.peek() {
            None => Err(self.end_error()),
            Some('{') => self.object(place),
            Some('[') => self.array(place),
            Some(quote @ ('"' | '\'')) => Ok(Bson::String(self.string(quote)?)),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some('?') => {
                self.at += 1;
                self.fill(place, start, false)
            }
            Some(c) if is_name(c) => {
                let word = self.name();
                match word.as_str() {
                    "true" => Ok(Bson::Boolean(true)),
                    "false" => Ok(Bson::Boolean(false)),
                    "null" => Ok(Bson::Null),
                    "ObjectId" => {
                        self.object_id_form(start)?;
                        self.fill(place, start, true)
                    }
                    _ => Err(self.text_error(
                        start,
                        &format!(
                            "`{word}` is not a value; a value is a number, a quoted string, `true`, `false`, `null`, `?`, or `ObjectId(?)`"
                        ),
                    )),
                }
            }
            Some(_) => Err(self.text_error(start, "a value goes here")),
        }
    }

    fn object(&mut self, place: Place) -> Result<Bson, Error> {
        self.enter()?;
        let mut document = Document::new();
        loop {
            self.skip_space();
            if self.peek() == Some('}') {
                self.at += 1;
                break;
            }
            let key_at = self.at;
            let key = self.key()?;
            if JAVASCRIPT.contains(&key.as_str()) {
                return Err(self.text_error(
                    key_at,
                    &format!(
                        "`{key}` runs JavaScript on the server, which a query text cannot use"
                    ),
                ));
            }
            if place == Place::Operators && !key.starts_with('$') {
                return Err(self.text_error(key_at, UPDATE_TOP));
            }
            if document.contains_key(&key) {
                return Err(self.text_error(
                    key_at,
                    &format!("the key `{key}` is written twice in one document"),
                ));
            }
            self.skip_space();
            if self.peek() != Some(':') {
                return Err(self.text_error(self.at, "a `:` goes after a key"));
            }
            self.at += 1;
            let value = self.value(place.member(&key))?;
            document.insert(key, value);
            self.skip_space();
            match self.peek() {
                Some(',') => self.at += 1,
                Some('}') => {
                    self.at += 1;
                    break;
                }
                None => return Err(self.end_error()),
                Some(_) => return Err(self.text_error(self.at, "a `,` or a `}` goes here")),
            }
        }
        self.depth -= 1;
        Ok(Bson::Document(document))
    }

    fn array(&mut self, place: Place) -> Result<Bson, Error> {
        self.enter()?;
        let stages = matches!(place, Place::Stages { .. });
        let mut items = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                Some(']') => {
                    self.at += 1;
                    break;
                }
                Some('{') => {}
                None => return Err(self.end_error()),
                Some(_) if stages => return Err(self.text_error(self.at, STAGE)),
                Some(_) => {}
            }
            items.push(self.value(place.element())?);
            self.skip_space();
            match self.peek() {
                Some(',') => self.at += 1,
                Some(']') => {
                    self.at += 1;
                    break;
                }
                None => return Err(self.end_error()),
                Some(_) => return Err(self.text_error(self.at, "a `,` or a `]` goes here")),
            }
        }
        self.depth -= 1;
        Ok(Bson::Array(items))
    }

    /// Steps into the document or array whose bracket is at `at`.
    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.text_error(
                self.at,
                &format!("the text nests deeper than {MAX_DEPTH} levels"),
            ));
        }
        self.at += 1;
        Ok(())
    }

    fn key(&mut self) -> Result<String, Error> {
        match self.peek() {
            Some(quote @ ('"' | '\'')) => self.string(quote),
            Some('?') => Err(self.text_error(
                self.at,
                "a `?` cannot be a key; a value stands only where a value goes",
            )),
            Some(c) if is_name(c) || c.is_ascii_digit() => Ok(self.name()),
            None => Err(self.end_error()),
            Some(_) => Err(self.text_error(self.at, "a key goes here, a name or a quoted string")),
        }
    }

    /// A bare name: letters, digits, `_`, `$`, and `.`.
    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if !(is_name(c) || c.is_ascii_digit()) {
                break;
            }
            name.push(c);
            self.at += 1;
        }
        name
    }

    /// A string in `quote`s, with JSON's escapes and `\'`.
    fn string(&mut self, quote: char) -> Result<String, Error> {
        let start = self.at;
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.text_error(start, "the string is not closed"));
            };
            let escape_at = self.at;
            self.at += 1;
            if c == quote {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.peek() else {
                return Err(self.text_error(start, "the string is not closed"));
            };
            self.at += 1;
            match e {
                '"' | '\'' | '\\' | '/' => out.push(e),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => out.push(self.unicode(escape_at)?),
                _ => {
                    return Err(self.text_error(
                        escape_at,
                        &format!(
                            "`\\{e}` is not an escape; a backslash comes before one of \" ' \\ / b f n r t u"
                        ),
                    ));
                }
            }
        }
    }

    /// The character of a `\u` escape whose backslash is at `escape_at`,
    /// with the `u` read; a surrogate pair is two escapes.
    fn unicode(&mut self, escape_at: usize) -> Result<char, Error> {
        let wrong = |parser: &Parser| {
            parser.text_error(
                escape_at,
                "`\\u` takes four hex digits that name a character, or two such escapes for a surrogate pair",
            )
        };
        let Some(high) = self.hex4() else {
            return Err(wrong(self));
        };
        let code = if (0xD800..0xDC00).contains(&high) {
            if self.peek() != Some('\\') {
                return Err(wrong(self));
            }
            self.at += 1;
            if self.peek() != Some('u') {
                return Err(wrong(self));
            }
            self.at += 1;
            match self.hex4() {
                Some(low) if (0xDC00..0xE000).contains(&low) => {
                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                }
                _ => return Err(wrong(self)),
            }
        } else {
            high
        };
        char::from_u32(code).ok_or_else(|| wrong(self))
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut code = 0;
        for _ in 0..4 {
            let digit = self.peek()?.to_digit(16)?;
            code = code * 16 + digit;
            self.at += 1;
        }
        Some(code)
    }

    /// An integer, int32 when it fits and int64 otherwise, or a double
    /// when it has a `.` or an exponent.
    fn number(&mut self) -> Result<Bson, Error> {
        let start = self.at;
        let mut double = false;
        if self.peek() == Some('-') {
            self.at += 1;
        }
        if !self.digits() {
            return Err(self.text_error(start, NUMBER));
        }
        if self.peek() == Some('.') {
            self.at += 1;
            double = true;
            if !self.digits() {
                return Err(self.text_error(start, NUMBER));
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.at += 1;
            double = true;
            if matches!(self.peek(), Some('+' | '-')) {
                self.at += 1;
            }
            if !self.digits() {
                return Err(self.text_error(start, NUMBER));
            }
        }
        let written: String = self
            .chars
            .get(start..self.at)
            .map(|chars| chars.iter().collect())
            .unwrap_or_default();
        if double {
            match written.parse::<f64>() {
                Ok(number) if number.is_finite() => Ok(Bson::Double(number)),
                _ => Err(self.text_error(start, "the number is too large")),
            }
        } else {
            match written.parse::<i64>() {
                Ok(number) => Ok(match i32::try_from(number) {
                    Ok(small) => Bson::Int32(small),
                    Err(_) => Bson::Int64(number),
                }),
                Err(_) => Err(self.text_error(
                    start,
                    "the number is too large; an integer holds at most 9223372036854775807",
                )),
            }
        }
    }

    /// Reads ASCII digits; whether there was one.
    fn digits(&mut self) -> bool {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.at > start
    }

    /// The `(?)` after `ObjectId`, whose `O` is at `start`.
    fn object_id_form(&mut self, start: usize) -> Result<(), Error> {
        for wanted in ['(', '?', ')'] {
            self.skip_space();
            if self.peek() != Some(wanted) {
                return Err(self.text_error(start, OBJECT_ID_FORM));
            }
            self.at += 1;
        }
        Ok(())
    }

    /// The value for a `?` (or `ObjectId(?)` when `object_id`) at `at`,
    /// which stands at `place`.
    fn fill(&mut self, place: Place, at: usize, object_id: bool) -> Result<Bson, Error> {
        match place {
            Place::Query | Place::Data { .. } | Place::Count => {}
            Place::Regex if !object_id => {}
            Place::Regex => return Err(self.text_error(at, NOT_A_PATTERN)),
            Place::Options => return Err(self.text_error(at, OPTIONS)),
            _ => return Err(self.text_error(at, NOT_HERE)),
        }
        let index = self.next;
        self.next += 1;
        self.taken += 1;
        let Some(values) = self.values else {
            return Ok(Bson::Null);
        };
        let Some(value) = values.get(index) else {
            return Err(self.text_error(at, "there are fewer values than `?`s"));
        };
        let number = index + 1;
        if object_id {
            return match value {
                Value::Text(hex) if is_object_id(hex) => ObjectId::parse_str(hex)
                    .map(Bson::ObjectId)
                    .map_err(|_| self.value_error(at, number, OBJECT_ID)),
                _ => Err(self.value_error(at, number, OBJECT_ID)),
            };
        }
        match (place, value) {
            (Place::Regex, Value::Text(text)) => Ok(Bson::String(escape(text))),
            (Place::Regex, _) => Err(self.value_error(at, number, NOT_A_PATTERN)),
            (Place::Query, Value::Null) => Err(self.value_error(at, number, NONE_MATCH)),
            _ => Ok(bson_of(value)),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn end_error(&self) -> Error {
        self.text_error(
            self.at,
            "the text ends before the document or array is closed",
        )
    }

    /// An error of the text at the character whose index is `at`.
    fn text_error(&self, at: usize, reason: &str) -> Error {
        Error::new(format!(
            "the {} text, at character {}: {reason}",
            self.kind.name(),
            at + 1
        ))
    }

    /// An error of the value numbered `number` (from 1, across the call),
    /// whose `?` is at `at`. It never holds the value.
    fn value_error(&self, at: usize, number: usize, reason: &str) -> Error {
        Error::new(format!(
            "value {number}, at character {} of the {} text: {reason}",
            at + 1,
            self.kind.name()
        ))
    }
}

/// Whether `c` may start a bare name or word.
fn is_name(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '_' | '$' | '.')
}

fn is_object_id(text: &str) -> bool {
    text.len() == 24 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A value as BSON, by the spec's section 3.3.
fn bson_of(value: &Value) -> Bson {
    match value {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int(n) => Bson::Int64(*n),
        Value::Float(f) => Bson::Double(*f),
        Value::Text(text) => Bson::String(text.clone()),
        // BSON dates hold milliseconds; the microseconds are dropped,
        // rounding toward the past.
        Value::Time(time) => Bson::DateTime(DateTime::from_millis(
            time.to_unix_micros().div_euclid(1000),
        )),
        Value::Uuid(uuid) => Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uuid.as_bytes().to_vec(),
        }),
        Value::Bytes(bytes) => Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: bytes.as_ref().to_vec(),
        }),
    }
}

/// `text` as a pattern that matches that text and nothing else: a
/// backslash before every character with a meaning in a pattern, and
/// before whitespace, which the `x` option would ignore (with the two
/// marks, U+200E and U+200F, that it ignores too). A backslash before any
/// character that is not a letter or a digit makes it literal.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if "\\^$.|?*+()[]{}-#".contains(c)
            || c.is_whitespace()
            || matches!(c, '\u{200e}' | '\u{200f}')
        {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Kind, Query, count, count_error, parse};
    use mongodb::bson::oid::ObjectId;
    use mongodb::bson::spec::BinarySubtype;
    use mongodb::bson::{Binary, Bson, DateTime, Document, doc};
    use varyk_std::{Bytes, Time, Uuid, Value};

    const NOT_HERE: &str = "a value can stand only in a filter, a `$match` stage, a value of `$set` or a like update operator, or `$skip` and `$limit`";
    const NONE_MATCH: &str = "a `None` value cannot be matched; write `null` in the text to match a missing or null field";

    /// The document a text of `kind` gives with `values`, or `None`.
    fn document(kind: Kind, text: &str, values: &[Value]) -> Option<Document> {
        match parse(kind, text, values, 0) {
            Ok((Query::Document(d), taken)) if taken == count_of(kind, text) => Some(d),
            _ => None,
        }
    }

    /// The stages a text of `kind` gives with `values`, or `None`.
    fn stages(kind: Kind, text: &str, values: &[Value]) -> Option<Vec<Document>> {
        match parse(kind, text, values, 0) {
            Ok((Query::Stages(s), _)) => Some(s),
            _ => None,
        }
    }

    fn count_of(kind: Kind, text: &str) -> usize {
        count(kind, text).unwrap_or(usize::MAX)
    }

    /// The message of a failed parse, or "no error".
    fn message(kind: Kind, text: &str, values: &[Value]) -> String {
        match parse(kind, text, values, 0) {
            Ok(_) => "no error".to_string(),
            Err(e) => e.message().to_string(),
        }
    }

    fn filter(text: &str, values: &[Value]) -> Option<Document> {
        document(Kind::Filter, text, values)
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    // The syntax.

    #[test]
    fn keys_bare_and_quoted() {
        assert_eq!(
            filter(
                r#"{ name: 1, 'two words': 2, "$gte": 3, address.city: 4, _id: 5, $and: 6 }"#,
                &[]
            ),
            Some(doc! {
                "name": 1, "two words": 2, "$gte": 3, "address.city": 4, "_id": 5, "$and": 6
            })
        );
    }

    #[test]
    fn strings_in_single_and_double_quotes_with_escapes() {
        assert_eq!(
            filter(
                r#"{ a: 'it\'s', b: "say \"hi\"", c: 'tab\there\nline', d: "\\ \/ \b \f \r", e: 'é😀', f: "it's", g: 'a "b"' }"#,
                &[]
            ),
            Some(doc! {
                "a": "it's", "b": "say \"hi\"", "c": "tab\there\nline",
                "d": "\\ / \u{8} \u{c} \r", "e": "\u{e9}\u{1f600}", "f": "it's", "g": "a \"b\""
            })
        );
    }

    #[test]
    fn a_bad_escape_or_an_open_string_is_an_error() {
        let m = message(Kind::Filter, r"{ a: '\q' }", &[]);
        assert_eq!(
            m,
            "the filter text, at character 7: `\\q` is not an escape; a backslash comes before one of \" ' \\ / b f n r t u"
        );
        let m = message(Kind::Filter, r"{ a: '\ud83d' }", &[]);
        assert!(m.starts_with("the filter text, at character 7: "), "{m}");
        let m = message(Kind::Filter, "{ a: 'open }", &[]);
        assert_eq!(
            m,
            "the filter text, at character 6: the string is not closed"
        );
    }

    #[test]
    fn a_question_mark_in_quotes_is_text() {
        assert_eq!(count(Kind::Filter, "{ a: '?', '?': \"?\" }").ok(), Some(0));
        assert_eq!(
            filter("{ a: '?', '?': \"?\" }", &[]),
            Some(doc! { "a": "?", "?": "?" })
        );
    }

    #[test]
    fn trailing_commas_and_whitespace() {
        assert_eq!(
            filter("\n\t{ a: [1, 2, ], b: { c: true, }, }\r\n", &[]),
            Some(doc! { "a": [1, 2], "b": { "c": true } })
        );
        assert_eq!(filter("{}", &[]), Some(Document::new()));
        assert_eq!(filter("{ a: [] }", &[]), Some(doc! { "a": [] }));
    }

    #[test]
    fn a_comma_alone_or_a_missing_comma_is_an_error() {
        assert_eq!(
            message(Kind::Filter, "{ , }", &[]),
            "the filter text, at character 3: a key goes here, a name or a quoted string"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: 1 b: 2 }", &[]),
            "the filter text, at character 8: a `,` or a `}` goes here"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: [1 2] }", &[]),
            "the filter text, at character 9: a `,` or a `]` goes here"
        );
        assert_eq!(
            message(Kind::Filter, "{ a 1 }", &[]),
            "the filter text, at character 5: a `:` goes after a key"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: }", &[]),
            "the filter text, at character 6: a value goes here"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: 1", &[]),
            "the filter text, at character 7: the text ends before the document or array is closed"
        );
    }

    #[test]
    fn true_false_null() {
        assert_eq!(
            filter("{ a: true, b: false, c: null }", &[]),
            Some(doc! { "a": true, "b": false, "c": Bson::Null })
        );
        assert_eq!(
            message(Kind::Filter, "{ a: maybe }", &[]),
            "the filter text, at character 6: `maybe` is not a value; a value is a number, a quoted string, `true`, `false`, `null`, `?`, or `ObjectId(?)`"
        );
    }

    #[test]
    fn numbers_int32_int64_and_double() {
        assert_eq!(
            filter(
                "{ a: 1, b: -2147483648, c: 2147483648, d: -9223372036854775808, e: 1.5, f: -2e3, g: 1E-2, h: 0 }",
                &[]
            ),
            Some(doc! {
                "a": Bson::Int32(1),
                "b": Bson::Int32(i32::MIN),
                "c": Bson::Int64(2147483648),
                "d": Bson::Int64(i64::MIN),
                "e": 1.5,
                "f": -2000.0,
                "g": 0.01,
                "h": Bson::Int32(0),
            })
        );
    }

    #[test]
    fn numbers_beyond_int64_or_a_double_are_an_error() {
        assert_eq!(
            message(Kind::Filter, "{ a: 9223372036854775808 }", &[]),
            "the filter text, at character 6: the number is too large; an integer holds at most 9223372036854775807"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: 1e999 }", &[]),
            "the filter text, at character 6: the number is too large"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: - }", &[]),
            "the filter text, at character 6: a number goes here, as `-1` or `2.5`"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: 1. }", &[]),
            "the filter text, at character 6: a number goes here, as `-1` or `2.5`"
        );
    }

    #[test]
    fn text_after_the_end_is_an_error() {
        assert_eq!(
            message(Kind::Filter, "{} {}", &[]),
            "the filter text, at character 4: the text goes on after its end"
        );
    }

    #[test]
    fn nesting_has_a_limit() {
        let deep = format!("{{ a: {}1{} }}", "[".repeat(70), "]".repeat(70));
        let m = message(Kind::Filter, &deep, &[]);
        assert!(m.contains("nests deeper than 64 levels"), "{m}");
        let fine = format!("{{ a: {}1{} }}", "[".repeat(60), "]".repeat(60));
        assert_eq!(message(Kind::Filter, &fine, &[]), "no error");
    }

    #[test]
    fn a_key_twice_is_an_error() {
        assert_eq!(
            message(Kind::Filter, "{ a: 1, 'a': 2 }", &[]),
            "the filter text, at character 9: the key `a` is written twice in one document"
        );
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        assert_eq!(
            message(Kind::Filter, "{ 'é': 1 2 }", &[]),
            "the filter text, at character 10: a `,` or a `}` goes here"
        );
    }

    // The values.

    #[test]
    fn values_fill_the_question_marks_in_order() {
        let time = match Time::from_unix_micros(1_700_000_000_123_999) {
            Ok(t) => t,
            Err(_) => Time::now(),
        };
        let uuid = Uuid::from_bytes([7; 16]);
        let bytes = Bytes::from_text("hi");
        let values = [
            Value::Bool(true),
            Value::Int(5),
            Value::Float(1.5),
            text("Ada"),
            Value::Time(time),
            Value::Uuid(uuid),
            Value::Bytes(bytes),
        ];
        assert_eq!(
            filter("{ a: ?, b: ?, c: ?, d: ?, e: ?, f: ?, g: ? }", &values),
            Some(doc! {
                "a": true,
                "b": Bson::Int64(5),
                "c": 1.5,
                "d": "Ada",
                "e": DateTime::from_millis(1_700_000_000_123),
                "f": Binary { subtype: BinarySubtype::Uuid, bytes: vec![7; 16] },
                "g": Binary { subtype: BinarySubtype::Generic, bytes: b"hi".to_vec() },
            })
        );
    }

    #[test]
    fn a_time_before_1970_rounds_toward_the_past() {
        let time = match Time::from_unix_micros(-1_500) {
            Ok(t) => t,
            Err(_) => Time::now(),
        };
        assert_eq!(
            filter("{ a: ? }", &[Value::Time(time)]),
            Some(doc! { "a": DateTime::from_millis(-2) })
        );
    }

    #[test]
    fn a_value_is_never_parsed_as_text() {
        assert_eq!(
            filter("{ a: ? }", &[text("{ $gt: '' }")]),
            Some(doc! { "a": "{ $gt: '' }" })
        );
        assert_eq!(
            filter("{ a: ? }", &[text("$secret")]),
            Some(doc! { "a": "$secret" })
        );
        assert_eq!(
            filter("{ a: '$secret' }", &[]),
            Some(doc! { "a": "$secret" })
        );
    }

    #[test]
    fn the_first_value_and_the_number_taken() {
        let values = [Value::Int(1), Value::Int(2), Value::Int(3)];
        match parse(Kind::Filter, "{ a: ?, b: ? }", &values, 1) {
            Ok((Query::Document(d), taken)) => {
                assert_eq!(d, doc! { "a": Bson::Int64(2), "b": Bson::Int64(3) });
                assert_eq!(taken, 2);
            }
            other => assert!(matches!(other, Ok((Query::Document(_), _))), "{other:?}"),
        }
        assert_eq!(
            match parse(Kind::Filter, "{ a: ObjectId(?) }", &values, 1) {
                Ok(_) => "no error".to_string(),
                Err(e) => e.message().to_string(),
            },
            "value 2, at character 6 of the filter text: `ObjectId(?)` takes a string of 24 hex digits"
        );
    }

    #[test]
    fn more_question_marks_than_values_is_an_error() {
        assert_eq!(
            message(Kind::Filter, "{ a: ?, b: ? }", &[Value::Int(1)]),
            "the filter text, at character 12: there are fewer values than `?`s"
        );
    }

    #[test]
    fn counting_counts_every_question_mark_and_object_id_once() {
        assert_eq!(count(Kind::Filter, "{}").ok(), Some(0));
        assert_eq!(
            count(
                Kind::Filter,
                "{ a: ?, b: [?, ObjectId(?)], c: { $in: [?] } }"
            )
            .ok(),
            Some(4)
        );
        assert_eq!(
            count(
                Kind::Update,
                "{ $set: { a: ? }, $push: { b: { $each: [?, ?] } } }"
            )
            .ok(),
            Some(3)
        );
    }

    #[test]
    fn counting_checks_the_text_too() {
        assert!(count(Kind::Filter, "{ ?: 1 }").is_err());
        assert!(count(Kind::Sort, "{ a: ? }").is_err());
        assert!(count(Kind::Filter, "").is_err());
    }

    #[test]
    fn the_count_error_names_both_counts() {
        assert_eq!(
            count_error(1, 2).message(),
            "this call's texts take 1 value, one for each `?`, but 2 values were given"
        );
        assert_eq!(
            count_error(2, 1).message(),
            "this call's texts take 2 values, one for each `?`, but 1 value was given"
        );
        assert_eq!(
            count_error(1, 0).message(),
            "this call's texts take 1 value, one for each `?`, but 0 values were given"
        );
    }

    // `ObjectId(?)`.

    #[test]
    fn object_id_takes_24_hex_digits() {
        let hex = "64b7f0c2a1b2c3d4e5f60718";
        let id = match ObjectId::parse_str(hex) {
            Ok(id) => id,
            Err(_) => ObjectId::new(),
        };
        assert_eq!(
            filter("{ _id: ObjectId( ? ) }", &[text(hex)]),
            Some(doc! { "_id": id })
        );
        assert_eq!(count(Kind::Filter, "{ _id: ObjectId(?) }").ok(), Some(1));
    }

    #[test]
    fn object_id_of_anything_else_is_an_error_without_the_value() {
        for value in [
            text("not-hex-secret-value-xyz"),
            text("64b7f0c2a1b2c3d4e5f6071"),
            text("64b7f0c2a1b2c3d4e5f607189"),
            text("64b7f0c2a1b2c3d4e5f6071é"),
            Value::Int(5),
            Value::Null,
        ] {
            let m = message(
                Kind::Filter,
                "{ _id: ObjectId(?) }",
                std::slice::from_ref(&value),
            );
            assert_eq!(
                m,
                "value 1, at character 8 of the filter text: `ObjectId(?)` takes a string of 24 hex digits"
            );
        }
    }

    #[test]
    fn object_id_written_otherwise_is_an_error() {
        let wanted = "the filter text, at character 6: `ObjectId` takes a `?`, as `ObjectId(?)`";
        assert_eq!(
            message(
                Kind::Filter,
                "{ a: ObjectId('64b7f0c2a1b2c3d4e5f60718') }",
                &[]
            ),
            wanted
        );
        assert_eq!(message(Kind::Filter, "{ a: ObjectId }", &[]), wanted);
        assert_eq!(
            message(Kind::Filter, "{ a: ObjectId(? }", &[text("x")]),
            wanted
        );
    }

    // The rules that keep input out of the query.

    #[test]
    fn a_question_mark_is_never_a_key() {
        assert_eq!(
            message(Kind::Filter, "{ a: 1, ?: 1 }", &[Value::Int(1)]),
            "the filter text, at character 9: a `?` cannot be a key; a value stands only where a value goes"
        );
    }

    #[test]
    fn an_empty_text_is_an_error() {
        for (kind, name) in [
            (Kind::Filter, "filter"),
            (Kind::Update, "update"),
            (Kind::Pipeline, "pipeline"),
            (Kind::Sort, "sort"),
            (Kind::IndexKeys, "index keys"),
            (Kind::IndexOptions, "index options"),
        ] {
            assert_eq!(message(kind, "", &[]), format!("the {name} text is empty"));
            assert_eq!(
                message(kind, " \n ", &[]),
                format!("the {name} text is empty")
            );
        }
    }

    #[test]
    fn each_kind_has_its_shape() {
        assert_eq!(
            message(Kind::Filter, "[{ a: 1 }]", &[]),
            "the filter text, at character 1: a filter is a document, as `{ name: ? }`"
        );
        assert_eq!(
            message(Kind::Sort, "[]", &[]),
            "the sort text, at character 1: a sort is a document, as `{ created_at: -1 }`"
        );
        assert_eq!(
            message(Kind::IndexKeys, "1", &[]),
            "the index keys text, at character 1: index keys are a document, as `{ email: 1 }`"
        );
        assert_eq!(
            message(Kind::IndexOptions, "'x'", &[]),
            "the index options text, at character 1: index options are a document, as `{ unique: true }`"
        );
        assert_eq!(
            message(Kind::Pipeline, "{ $match: {} }", &[]),
            "the pipeline text, at character 1: a pipeline is an array of stages, as `[{ $match: { name: ? } }]`"
        );
        assert_eq!(
            message(Kind::Pipeline, "[{ $match: {} }, 1]", &[]),
            "the pipeline text, at character 18: each stage of a pipeline is a document, as `{ $match: { name: ? } }`"
        );
        assert_eq!(
            message(Kind::Update, "?", &[Value::Int(1)]),
            "the update text, at character 1: an update is a document of operators, as `{ $set: { name: ? } }`, or an array of stages"
        );
        assert_eq!(
            message(Kind::Update, "[1]", &[]),
            "the update text, at character 2: each stage of a pipeline is a document, as `{ $match: { name: ? } }`"
        );
        assert_eq!(stages(Kind::Pipeline, "[]", &[]), Some(Vec::new()));
        assert_eq!(
            stages(Kind::Update, "[{ $set: { a: 1 } }]", &[]),
            Some(vec![doc! { "$set": { "a": 1 } }])
        );
        assert_eq!(
            document(Kind::Sort, "{ created_at: -1 }", &[]),
            Some(doc! { "created_at": -1 })
        );
    }

    #[test]
    fn an_update_document_has_only_operators_at_its_top() {
        assert_eq!(
            message(Kind::Update, "{ $set: { a: 1 }, name: 'Ada' }", &[]),
            "the update text, at character 19: each top-level key of an update starts with `$`, as `$set`; `replace` is the call for a whole document"
        );
        assert_eq!(
            document(Kind::Update, "{ $set: { a: { b: 1 } } }", &[]),
            Some(doc! { "$set": { "a": { "b": 1 } } })
        );
    }

    #[test]
    fn an_empty_update_document_is_an_error() {
        for t in ["{}", "  { }"] {
            assert_eq!(
                message(Kind::Update, t, &[]),
                format!(
                    "the update text, at character {}: an update names at least one operator, as `$set`",
                    t.find('{').map_or(0, |at| at + 1)
                ),
                "{t}"
            );
        }
        assert_eq!(count(Kind::Update, "{}").ok(), None);
        assert_eq!(document(Kind::Filter, "{}", &[]), Some(Document::new()));
        assert_eq!(
            document(Kind::IndexOptions, "{}", &[]),
            Some(Document::new())
        );
    }

    #[test]
    fn query_context_takes_values_anywhere() {
        assert_eq!(
            filter(
                "{ age: { $gte: ?, $lt: ? }, $or: [{ a: ? }, { b: { $in: [?, ?] } }], c: { $elemMatch: { d: ? } }, $text: { $search: ? } }",
                &[
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::Int(4),
                    Value::Int(5),
                    Value::Int(6),
                    text("\"a phrase\" -not"),
                ]
            ),
            Some(doc! {
                "age": { "$gte": 1_i64, "$lt": 2_i64 },
                "$or": [{ "a": 3_i64 }, { "b": { "$in": [4_i64, 5_i64] } }],
                "c": { "$elemMatch": { "d": 6_i64 } },
                "$text": { "$search": "\"a phrase\" -not" },
            })
        );
    }

    #[test]
    fn expr_and_json_schema_are_closed() {
        assert_eq!(
            message(
                Kind::Filter,
                "{ $expr: { $eq: ['$a', ?] } }",
                &[Value::Int(1)]
            ),
            format!("the filter text, at character 24: {NOT_HERE}")
        );
        assert_eq!(
            message(
                Kind::Filter,
                "{ $jsonSchema: { properties: { a: { pattern: ? } } } }",
                &[text("x")]
            ),
            format!("the filter text, at character 46: {NOT_HERE}")
        );
        assert_eq!(
            message(
                Kind::Pipeline,
                "[{ $match: { $expr: ? } }]",
                &[Value::Int(1)]
            ),
            format!("the pipeline text, at character 21: {NOT_HERE}")
        );
    }

    #[test]
    fn a_pipeline_takes_values_in_match_skip_and_limit_only() {
        assert_eq!(
            stages(
                Kind::Pipeline,
                "[{ $match: { a: ? } }, { $skip: ? }, { $limit: ? }]",
                &[text("x"), Value::Int(1), Value::Int(2)]
            ),
            Some(vec![
                doc! { "$match": { "a": "x" } },
                doc! { "$skip": 1_i64 },
                doc! { "$limit": 2_i64 },
            ])
        );
        assert_eq!(
            stages(
                Kind::Pipeline,
                "[{ $match: { a: '$secret' } }, { $project: { b: '$secret' } }]",
                &[]
            ),
            Some(vec![
                doc! { "$match": { "a": "$secret" } },
                doc! { "$project": { "b": "$secret" } },
            ])
        );
        for (t, at) in [
            ("[{ $addFields: { a: ? } }]", 21),
            (
                "[{ $lookup: { from: ?, localField: 'a', foreignField: 'b', as: 'c' } }]",
                21,
            ),
            (
                "[{ $lookup: { from: 'b', as: 'c', pipeline: [{ $match: { a: ? } }] } }]",
                61,
            ),
            ("[{ $facet: { a: [{ $match: { a: ? } }] } }]", 33),
            (
                "[{ $unionWith: { coll: 'b', pipeline: [{ $match: { a: ? } }] } }]",
                55,
            ),
            ("[{ $search: { text: { query: ?, path: 'a' } } }]", 30),
            ("[{ $group: { _id: ? } }]", 19),
            ("[{ $skip: { a: ? } }]", 16),
        ] {
            assert_eq!(
                message(Kind::Pipeline, t, &[Value::Int(1)]),
                format!("the pipeline text, at character {at}: {NOT_HERE}"),
                "{t}"
            );
        }
    }

    #[test]
    fn update_operators_take_values_at_any_depth() {
        assert_eq!(
            document(
                Kind::Update,
                "{ $set: { profile: { name: ?, tags: [?] } }, $setOnInsert: { a: ? }, $inc: { b: ? }, $mul: { c: ? }, $min: { d: ? }, $max: { e: ? }, $push: { sessions: { $each: [{ token: ?, at: ? }], $slice: -5 } }, $addToSet: { f: { $each: [?] }, g: ? }, $pull: { h: { token: ? } }, $unset: { i: '' } }",
                &[
                    text("Ada"),
                    text("x"),
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                    Value::Int(4),
                    Value::Int(5),
                    text("t"),
                    Value::Int(6),
                    Value::Int(7),
                    Value::Int(8),
                    text("old"),
                ]
            ),
            Some(doc! {
                "$set": { "profile": { "name": "Ada", "tags": ["x"] } },
                "$setOnInsert": { "a": 1_i64 },
                "$inc": { "b": 2_i64 },
                "$mul": { "c": 3_i64 },
                "$min": { "d": 4_i64 },
                "$max": { "e": 5_i64 },
                "$push": { "sessions": { "$each": [{ "token": "t", "at": 6_i64 }], "$slice": -5 } },
                "$addToSet": { "f": { "$each": [7_i64] }, "g": 8_i64 },
                "$pull": { "h": { "token": "old" } },
                "$unset": { "i": "" },
            })
        );
    }

    #[test]
    fn other_update_places_are_closed() {
        for (t, at) in [
            ("{ $rename: { a: ? } }", 17),
            ("{ $unset: { a: ? } }", 16),
            ("{ $currentDate: { a: ? } }", 22),
            ("{ $set: ? }", 9),
            ("{ $set: { a: { $literal: ? } } }", 26),
            ("{ $set: { $a: ? } }", 15),
            ("{ $set: { a: { $each: [?] } } }", 24),
            ("{ $push: { a: { $each: [1], $slice: ? } } }", 37),
            ("{ $push: { a: { $sort: { b: ? } } } }", 29),
            ("{ $pull: { $expr: ? } }", 19),
            ("{ $pull: { a: { $expr: ? } } }", 24),
        ] {
            assert_eq!(
                message(Kind::Update, t, &[Value::Int(1)]),
                format!("the update text, at character {at}: {NOT_HERE}"),
                "{t}"
            );
        }
        assert_eq!(
            message(Kind::Update, "[{ $set: { a: ? } }]", &[Value::Int(1)]),
            format!("the update text, at character 15: {NOT_HERE}")
        );
        assert_eq!(
            document(Kind::Update, "{ $set: { a: '$secret' } }", &[]),
            Some(doc! { "$set": { "a": "$secret" } })
        );
    }

    #[test]
    fn sort_and_index_texts_take_no_value() {
        for kind in [Kind::Sort, Kind::IndexKeys, Kind::IndexOptions] {
            let m = message(kind, "{ a: ? }", &[Value::Int(1)]);
            assert!(m.ends_with(&format!("at character 6: {NOT_HERE}")), "{m}");
        }
    }

    #[test]
    fn none_cannot_be_matched_in_query_context() {
        assert_eq!(
            message(Kind::Filter, "{ a: 1, reset_token: ? }", &[Value::Null]),
            format!("value 1, at character 22 of the filter text: {NONE_MATCH}")
        );
        assert_eq!(
            message(
                Kind::Filter,
                "{ a: { $in: [?, ?] } }",
                &[Value::Int(1), Value::Null]
            ),
            format!("value 2, at character 17 of the filter text: {NONE_MATCH}")
        );
        assert_eq!(
            message(Kind::Pipeline, "[{ $match: { a: ? } }]", &[Value::Null]),
            format!("value 1, at character 17 of the pipeline text: {NONE_MATCH}")
        );
        assert_eq!(
            message(
                Kind::Update,
                "{ $pull: { s: { token: ? } } }",
                &[Value::Null]
            ),
            format!("value 1, at character 24 of the update text: {NONE_MATCH}")
        );
        assert_eq!(
            message(Kind::Update, "{ $pull: { s: ? } }", &[Value::Null]),
            format!("value 1, at character 15 of the update text: {NONE_MATCH}")
        );
        assert_eq!(filter("{ a: null }", &[]), Some(doc! { "a": Bson::Null }));
    }

    #[test]
    fn none_is_null_as_an_update_value() {
        assert_eq!(
            document(
                Kind::Update,
                "{ $set: { a: ?, b: { c: ? } }, $push: { d: ? } }",
                &[Value::Null, Value::Null, Value::Null]
            ),
            Some(
                doc! { "$set": { "a": Bson::Null, "b": { "c": Bson::Null } }, "$push": { "d": Bson::Null } }
            )
        );
    }

    #[test]
    fn javascript_keys_are_an_error_anywhere() {
        for (kind, t, at) in [
            (Kind::Filter, "{ $where: 'true' }", 3),
            (Kind::Filter, "{ '$where': 'true' }", 3),
            (
                Kind::Filter,
                "{ $expr: { $function: { body: 'x', args: [], lang: 'js' } } }",
                12,
            ),
            (Kind::Filter, "{ $expr: { $and: [{ \"$where\": 1 }] } }", 21),
            (
                Kind::Pipeline,
                "[{ $group: { _id: 1, a: { $accumulator: {} } } }]",
                27,
            ),
            (Kind::Update, "{ $set: { a: { $where: 1 } } }", 16),
            (Kind::Sort, "{ $function: 1 }", 3),
        ] {
            let m = message(kind, t, &[]);
            assert!(m.contains(&format!(", at character {at}: `$")), "{t}: {m}");
            assert!(
                m.ends_with("runs JavaScript on the server, which a query text cannot use"),
                "{t}: {m}"
            );
        }
        assert_eq!(
            message(Kind::Filter, "{ $where: 'true' }", &[]),
            "the filter text, at character 3: `$where` runs JavaScript on the server, which a query text cannot use"
        );
        assert_eq!(
            filter("{ a: '$where', b: '$function' }", &[]),
            Some(doc! { "a": "$where", "b": "$function" })
        );
    }

    #[test]
    fn a_regex_value_is_escaped_and_matches_only_its_text() {
        assert_eq!(
            filter("{ name: { $regex: ?, $options: 'i' } }", &[text(".*")]),
            Some(doc! { "name": { "$regex": "\\.\\*", "$options": "i" } })
        );
        assert_eq!(
            filter(
                "{ a: { $regex: ? } }",
                &[text("\\^$.|?*+()[]{}-# \t\nab9\u{a0}\u{200e}")]
            ),
            Some(doc! { "a": {
                "$regex": "\\\\\\^\\$\\.\\|\\?\\*\\+\\(\\)\\[\\]\\{\\}\\-\\#\\ \\\t\\\nab9\\\u{a0}\\\u{200e}"
            } })
        );
        assert_eq!(
            filter("{ sku: { $regex: '^A-[0-9]+$' } }", &[]),
            Some(doc! { "sku": { "$regex": "^A-[0-9]+$" } })
        );
    }

    #[test]
    fn a_regex_value_must_be_text() {
        for value in [Value::Int(1), Value::Null, Value::Bool(true)] {
            assert_eq!(
                message(
                    Kind::Filter,
                    "{ a: { $regex: ? } }",
                    std::slice::from_ref(&value)
                ),
                "value 1, at character 16 of the filter text: the value of `$regex` must be a `string`; it is matched as plain text"
            );
        }
        assert_eq!(
            message(Kind::Filter, "{ a: { $regex: ObjectId(?) } }", &[text("x")]),
            "the filter text, at character 16: the value of `$regex` must be a `string`; it is matched as plain text"
        );
        assert_eq!(
            message(Kind::Filter, "{ a: { $regex: [?] } }", &[text("x")]),
            format!("the filter text, at character 17: {NOT_HERE}")
        );
    }

    #[test]
    fn a_value_cannot_be_the_options() {
        assert_eq!(
            message(
                Kind::Filter,
                "{ a: { $regex: 'x', $options: ? } }",
                &[text("i")]
            ),
            "the filter text, at character 31: the options of `$regex` are part of the query; write them in the text, as `$options: 'i'`"
        );
    }

    #[test]
    fn regular_expression_is_an_ordinary_key() {
        let parsed = filter(
            "{ a: { $regularExpression: { pattern: ?, options: '' } } }",
            &[text(".*")],
        );
        assert_eq!(
            parsed,
            Some(doc! { "a": { "$regularExpression": { "pattern": ".*", "options": "" } } })
        );
        let is_document = matches!(
            parsed.as_ref().and_then(|d| d.get("a")),
            Some(Bson::Document(_))
        );
        assert!(is_document);
        assert_eq!(
            filter("{ a: { $date: 1, $oid: 'x', $binary: 'y' } }", &[]),
            Some(doc! { "a": { "$date": 1, "$oid": "x", "$binary": "y" } })
        );
    }

    #[test]
    fn messages_never_hold_a_value() {
        for (kind, t) in [
            (Kind::Filter, "{ _id: ObjectId(?) }"),
            (Kind::Filter, "{ a: { $regex: ? } }"),
            (Kind::Filter, "{ $expr: ? }"),
            (Kind::Filter, "{ a: { $options: ? } }"),
            (Kind::Update, "{ $rename: { a: ? } }"),
            (Kind::Sort, "{ a: ? }"),
        ] {
            for value in [text("hunter2-secret"), Value::Int(424242)] {
                let m = message(kind, t, std::slice::from_ref(&value));
                if m == "no error" {
                    continue;
                }
                assert!(!m.contains("hunter2") && !m.contains("424242"), "{t}: {m}");
            }
        }
    }
}
