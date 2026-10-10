// The serializer: a Varyk value into a BSON document (spec 3.1). It
// reports itself as not human-readable, so varyk-std hands `Time` and
// `Uuid` over under the names of `varyk_std::serde_names` and `Bytes`
// through `serialize_bytes` (spec 6.1), and each keeps its BSON type: a
// date, binary subtype 4, binary subtype 0. Every message names a field,
// a document's place in a list, or a type, and never holds a value.

use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Document};
use std::fmt;
use varyk_std::serde::ser::{self, Impossible, Serialize};
use varyk_std::{Error, Uuid, serde_names};

/// `value`, a struct or a `HashMap<string, V>`, as a document whose `_id`
/// is checked: a `None` one left out, a `string`, an integer, or a `Uuid`
/// kept, anything else an `Error` naming its type.
pub fn document<T: Serialize + ?Sized>(value: &T) -> Result<Document, Error> {
    value
        .serialize(Writer)
        .and_then(checked)
        .map_err(|e| e.into_error(None))
}

/// `values`, a `Vec` of structs or maps, as documents in the same order,
/// each as `document` writes it; an `Error` names the document by its
/// place, counting from 1.
pub fn documents<T: Serialize + ?Sized>(values: &T) -> Result<Vec<Document>, Error> {
    let items = match values.serialize(Writer) {
        Ok(Bson::Array(items)) => items,
        Ok(other) => {
            return Err(Error::new(format!(
                "`insert_many` takes a `Vec` of structs or maps; this value is {}",
                kind(&other)
            )));
        }
        Err(mut e) => {
            // The outermost step of the path is the document's place.
            let place = match e.path.pop() {
                Some(Step::Index(place)) => Some(place),
                Some(other) => {
                    e.path.push(other);
                    None
                }
                None => None,
            };
            return Err(e.into_error(place));
        }
    };
    items
        .into_iter()
        .enumerate()
        .map(|(place, item)| checked(item).map_err(|e| e.into_error(Some(place))))
        .collect()
}

/// The text `insert` gives for an `_id`: a `string` as it is, an integer
/// in decimal, a `Uuid` hyphenated, an ObjectId as 24 hex digits.
pub fn id_text(id: &Bson) -> Result<String, Error> {
    match id {
        Bson::String(text) => Ok(text.clone()),
        Bson::Int32(n) => Ok(n.to_string()),
        Bson::Int64(n) => Ok(n.to_string()),
        Bson::ObjectId(oid) => Ok(oid.to_hex()),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes,
        }) => match <[u8; 16]>::try_from(bytes.as_slice()) {
            Ok(bytes) => Ok(Uuid::from_bytes(bytes).to_string()),
            Err(_) => Err(not_an_id_text()),
        },
        _ => Err(not_an_id_text()),
    }
}

fn not_an_id_text() -> Error {
    Error::new(
        "the `_id` of the inserted document is not a `string`, an integer, a `Uuid`, or an ObjectId"
            .to_string(),
    )
}

/// The written value as a document, its `_id` checked (spec 3.1). Only
/// the top level's `_id` is the document's; an embedded one is a field.
fn checked(written: Bson) -> Result<Document, WriteError> {
    let mut doc = match written {
        Bson::Document(doc) => doc,
        other => return Err(WriteError::new(Problem::NotADocument(kind(&other)))),
    };
    match doc.get("_id") {
        None | Some(Bson::String(_) | Bson::Int32(_) | Bson::Int64(_)) => {}
        Some(Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            ..
        })) => {}
        Some(Bson::Null) => {
            doc.remove("_id");
        }
        Some(other) => return Err(WriteError::new(Problem::Id(kind(other)))),
    }
    Ok(doc)
}

/// What a written value is, in Varyk's words.
fn kind(value: &Bson) -> &'static str {
    match value {
        Bson::Boolean(_) => "a `bool`",
        Bson::String(_) => "a `string`",
        Bson::Int32(_) | Bson::Int64(_) => "an integer",
        Bson::Double(_) => "a float",
        Bson::DateTime(_) => "a `Time`",
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            ..
        }) => "a `Uuid`",
        Bson::Binary(_) => "a `Bytes`",
        Bson::Null => "`None`",
        Bson::Array(_) => "a `Vec`",
        Bson::Document(_) => "a struct or a map",
        _ => "a value of another kind",
    }
}

/// One step of the path to a field: a struct's field, a map's entry,
/// whose key is data and so is not named, or a place in a list.
#[derive(Debug)]
enum Step {
    Field(&'static str),
    Entry,
    Index(usize),
}

#[derive(Debug)]
enum Problem {
    /// A `u64` or `usize` above `i64`'s largest.
    TooLarge,
    /// A map whose keys are not strings.
    Key,
    /// An enum variant that carries data.
    Data,
    /// The value's own `Serialize` failed; its text is dropped.
    Refused,
    /// The top level is not a struct or a map; what it is.
    NotADocument(&'static str),
    /// The `_id` is of a kind an `_id` cannot be; what it is.
    Id(&'static str),
}

/// A failure while writing, with the path to the field it is about,
/// innermost step first.
#[derive(Debug)]
struct WriteError {
    problem: Problem,
    path: Vec<Step>,
}

impl WriteError {
    fn new(problem: Problem) -> WriteError {
        WriteError {
            problem,
            path: Vec::new(),
        }
    }

    /// This error, inside `step`.
    fn inside(mut self, step: Step) -> WriteError {
        self.path.push(step);
        self
    }

    /// The `Error` of this failure, in the document at `place` of a list
    /// when there is one.
    fn into_error(self, place: Option<usize>) -> Error {
        let path: Vec<String> = self
            .path
            .iter()
            .rev()
            .map(|step| match step {
                Step::Field(name) => (*name).to_string(),
                Step::Entry => "<key>".to_string(),
                Step::Index(index) => index.to_string(),
            })
            .collect();
        let document = match place {
            Some(place) => format!(" of document {} of the list", place + 1),
            None => String::new(),
        };
        let subject = if path.is_empty() {
            match place {
                Some(place) => format!("document {} of the list", place + 1),
                None => "the value".to_string(),
            }
        } else {
            format!("the field `{}`{document}", path.join("."))
        };
        Error::new(match self.problem {
            Problem::TooLarge => format!(
                "{subject} is above the largest `i64`, which a document cannot hold; BSON has no unsigned integer"
            ),
            Problem::Key => format!("{subject} is a map whose keys are not strings"),
            Problem::Data => format!(
                "{subject} is an enum variant that carries data, which a document cannot hold"
            ),
            Problem::Refused => format!("{subject} cannot be written to a document"),
            Problem::NotADocument(what) => format!(
                "a document is written from a struct or a `HashMap<string, V>`; {} is {what}",
                match place {
                    Some(place) => format!("document {} of the list", place + 1),
                    None => "this value".to_string(),
                }
            ),
            Problem::Id(what) => format!(
                "the `_id`{document} must be a `string`, an integer, or a `Uuid`; this one is {what}"
            ),
        })
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a value cannot be written to a document")
    }
}

impl std::error::Error for WriteError {}

impl ser::Error for WriteError {
    fn custom<M: fmt::Display>(_message: M) -> Self {
        WriteError::new(Problem::Refused)
    }
}

/// Writes one value as BSON.
struct Writer;

impl ser::Serializer for Writer {
    type Ok = Bson;
    type Error = WriteError;
    type SerializeSeq = List;
    type SerializeTuple = List;
    type SerializeTupleStruct = List;
    type SerializeTupleVariant = Impossible<Bson, WriteError>;
    type SerializeMap = Map;
    type SerializeStruct = Struct;
    type SerializeStructVariant = Impossible<Bson, WriteError>;

    /// Not human-readable, so `Time`, `Uuid`, and `Bytes` come typed.
    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<Bson, WriteError> {
        Ok(Bson::Boolean(v))
    }

    fn serialize_i8(self, v: i8) -> Result<Bson, WriteError> {
        Ok(Bson::Int32(i32::from(v)))
    }

    fn serialize_i16(self, v: i16) -> Result<Bson, WriteError> {
        Ok(Bson::Int32(i32::from(v)))
    }

    fn serialize_i32(self, v: i32) -> Result<Bson, WriteError> {
        Ok(Bson::Int32(v))
    }

    fn serialize_i64(self, v: i64) -> Result<Bson, WriteError> {
        Ok(Bson::Int64(v))
    }

    fn serialize_u8(self, v: u8) -> Result<Bson, WriteError> {
        Ok(Bson::Int32(i32::from(v)))
    }

    fn serialize_u16(self, v: u16) -> Result<Bson, WriteError> {
        Ok(Bson::Int32(i32::from(v)))
    }

    fn serialize_u32(self, v: u32) -> Result<Bson, WriteError> {
        Ok(Bson::Int64(i64::from(v)))
    }

    /// A `u64` or a `usize`: BSON has no unsigned integer.
    fn serialize_u64(self, v: u64) -> Result<Bson, WriteError> {
        match i64::try_from(v) {
            Ok(v) => Ok(Bson::Int64(v)),
            Err(_) => Err(WriteError::new(Problem::TooLarge)),
        }
    }

    fn serialize_f32(self, v: f32) -> Result<Bson, WriteError> {
        Ok(Bson::Double(f64::from(v)))
    }

    fn serialize_f64(self, v: f64) -> Result<Bson, WriteError> {
        Ok(Bson::Double(v))
    }

    fn serialize_char(self, v: char) -> Result<Bson, WriteError> {
        Ok(Bson::String(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<Bson, WriteError> {
        Ok(Bson::String(v.to_string()))
    }

    /// A `Bytes`, and a `Uuid`'s 16 bytes inside its name.
    fn serialize_bytes(self, v: &[u8]) -> Result<Bson, WriteError> {
        Ok(Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: v.to_vec(),
        }))
    }

    fn serialize_none(self) -> Result<Bson, WriteError> {
        Ok(Bson::Null)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Bson, WriteError> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Bson, WriteError> {
        Ok(Bson::Null)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Bson, WriteError> {
        Ok(Bson::Null)
    }

    /// A variant that carries no data: its name, or its `#[rename]`.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Bson, WriteError> {
        Ok(Bson::String(variant.to_string()))
    }

    /// A `Time` (its `i64` microseconds, as a date in milliseconds,
    /// rounding toward the past) and a `Uuid` (its 16 bytes, as binary
    /// subtype 4) by their names; any other newtype is its inside.
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Bson, WriteError> {
        let inside = value.serialize(Writer)?;
        if name == serde_names::TIME {
            match inside {
                Bson::Int64(micros) => Ok(Bson::DateTime(DateTime::from_millis(
                    micros.div_euclid(1000),
                ))),
                _ => Err(WriteError::new(Problem::Refused)),
            }
        } else if name == serde_names::UUID {
            match inside {
                Bson::Binary(Binary { bytes, .. }) => Ok(Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes,
                })),
                _ => Err(WriteError::new(Problem::Refused)),
            }
        } else {
            Ok(inside)
        }
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Bson, WriteError> {
        Err(WriteError::new(Problem::Data))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<List, WriteError> {
        Ok(List {
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<List, WriteError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<List, WriteError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Impossible<Bson, WriteError>, WriteError> {
        Err(WriteError::new(Problem::Data))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Map, WriteError> {
        Ok(Map {
            doc: Document::new(),
            key: None,
        })
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Struct, WriteError> {
        Ok(Struct {
            doc: Document::new(),
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Impossible<Bson, WriteError>, WriteError> {
        Err(WriteError::new(Problem::Data))
    }
}

/// A `Vec`, as an array.
struct List {
    items: Vec<Bson>,
}

impl ser::SerializeSeq for List {
    type Ok = Bson;
    type Error = WriteError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WriteError> {
        let index = self.items.len();
        let item = value
            .serialize(Writer)
            .map_err(|e| e.inside(Step::Index(index)))?;
        self.items.push(item);
        Ok(())
    }

    fn end(self) -> Result<Bson, WriteError> {
        Ok(Bson::Array(self.items))
    }
}

impl ser::SerializeTuple for List {
    type Ok = Bson;
    type Error = WriteError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WriteError> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Bson, WriteError> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for List {
    type Ok = Bson;
    type Error = WriteError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WriteError> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Bson, WriteError> {
        ser::SerializeSeq::end(self)
    }
}

/// A `HashMap<string, V>`, as an embedded document.
struct Map {
    doc: Document,
    /// The key whose value comes next.
    key: Option<String>,
}

impl ser::SerializeMap for Map {
    type Ok = Bson;
    type Error = WriteError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), WriteError> {
        match key.serialize(Writer) {
            Ok(Bson::String(key)) => {
                self.key = Some(key);
                Ok(())
            }
            _ => Err(WriteError::new(Problem::Key)),
        }
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), WriteError> {
        let Some(key) = self.key.take() else {
            return Err(WriteError::new(Problem::Refused));
        };
        let value = value.serialize(Writer).map_err(|e| e.inside(Step::Entry))?;
        self.doc.insert(key, value);
        Ok(())
    }

    fn end(self) -> Result<Bson, WriteError> {
        Ok(Bson::Document(self.doc))
    }
}

/// A struct, as an embedded document: its fields in order, a skipped
/// one left out.
struct Struct {
    doc: Document,
}

impl ser::SerializeStruct for Struct {
    type Ok = Bson;
    type Error = WriteError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), WriteError> {
        let value = value
            .serialize(Writer)
            .map_err(|e| e.inside(Step::Field(key)))?;
        self.doc.insert(key, value);
        Ok(())
    }

    fn end(self) -> Result<Bson, WriteError> {
        Ok(Bson::Document(self.doc))
    }
}

#[cfg(test)]
mod tests {
    use super::{document, documents, id_text};
    use mongodb::bson::oid::ObjectId;
    use mongodb::bson::spec::BinarySubtype;
    use mongodb::bson::{Binary, Bson, DateTime, Document, doc};
    use std::collections::HashMap;
    use varyk_std::serde::Serialize;
    use varyk_std::{Bytes, Time, Uuid};

    /// The message of a failed result, or "no error".
    fn message_of<T>(result: Result<T, varyk_std::Error>) -> String {
        match result {
            Ok(_) => "no error".to_string(),
            Err(e) => e.message().to_string(),
        }
    }

    /// The document `value` writes, or one holding the error's message
    /// under `error`.
    fn written<T: Serialize + ?Sized>(value: &T) -> Document {
        match document(value) {
            Ok(d) => d,
            Err(e) => doc! { "error": e.message() },
        }
    }

    fn uuid() -> Uuid {
        Uuid::from_bytes([
            1, 137, 10, 93, 172, 150, 119, 75, 188, 206, 179, 2, 9, 154, 128, 87,
        ])
    }

    fn time(micros: i64) -> Time {
        match Time::from_unix_micros(micros) {
            Ok(t) => t,
            Err(_) => Time::now(),
        }
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Plain {
        flag: bool,
        name: String,
    }

    #[test]
    fn bool_and_string_are_boolean_and_string() {
        let value = Plain {
            flag: true,
            name: "Ada".to_string(),
        };
        assert_eq!(written(&value), doc! { "flag": true, "name": "Ada" });
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Numbers {
        a: i8,
        b: i16,
        c: i32,
        d: u8,
        e: u16,
        f: i64,
        g: u32,
        h: u64,
        i: usize,
        j: f32,
        k: f64,
    }

    #[test]
    fn integers_take_the_width_of_the_table_and_floats_are_doubles() {
        let value = Numbers {
            a: i8::MIN,
            b: i16::MAX,
            c: i32::MIN,
            d: u8::MAX,
            e: u16::MAX,
            f: i64::MIN,
            g: u32::MAX,
            h: i64::MAX as u64,
            i: 7,
            j: 0.5,
            k: -2.25,
        };
        let d = written(&value);
        assert_eq!(d.get("a"), Some(&Bson::Int32(-128)));
        assert_eq!(d.get("b"), Some(&Bson::Int32(32767)));
        assert_eq!(d.get("c"), Some(&Bson::Int32(i32::MIN)));
        assert_eq!(d.get("d"), Some(&Bson::Int32(255)));
        assert_eq!(d.get("e"), Some(&Bson::Int32(65535)));
        assert_eq!(d.get("f"), Some(&Bson::Int64(i64::MIN)));
        assert_eq!(d.get("g"), Some(&Bson::Int64(4_294_967_295)));
        assert_eq!(d.get("h"), Some(&Bson::Int64(i64::MAX)));
        assert_eq!(d.get("i"), Some(&Bson::Int64(7)));
        assert_eq!(d.get("j"), Some(&Bson::Double(0.5)));
        assert_eq!(d.get("k"), Some(&Bson::Double(-2.25)));
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Big {
        count: u64,
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Size {
        count: usize,
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Outer {
        inner: Big,
        list: Vec<u64>,
        map: HashMap<String, u64>,
    }

    #[test]
    fn a_u64_above_the_largest_i64_is_an_error_naming_the_field() {
        let message = message_of(document(&Big { count: u64::MAX }));
        assert!(message.contains("`count`"), "{message}");
        assert!(message.contains("`i64`"), "{message}");
        assert!(!message.contains("18446744073709551615"), "{message}");
        let message = message_of(document(&Big {
            count: i64::MAX as u64 + 1,
        }));
        assert!(message.contains("`count`"), "{message}");
        if usize::BITS == 64 {
            let message = message_of(document(&Size { count: usize::MAX }));
            assert!(message.contains("`count`"), "{message}");
        }
    }

    #[test]
    fn a_nested_field_is_named_by_its_path() {
        let deep = Outer {
            inner: Big { count: u64::MAX },
            list: vec![],
            map: HashMap::new(),
        };
        let message = message_of(document(&deep));
        assert!(message.contains("`inner.count`"), "{message}");
        let listed = Outer {
            inner: Big { count: 1 },
            list: vec![1, u64::MAX],
            map: HashMap::new(),
        };
        let message = message_of(document(&listed));
        assert!(message.contains("`list.1`"), "{message}");
        // A map's key is data, so it is not named.
        let mapped = Outer {
            inner: Big { count: 1 },
            list: vec![],
            map: HashMap::from([("ada@example.com".to_string(), u64::MAX)]),
        };
        let message = message_of(document(&mapped));
        assert!(message.contains("`map.<key>`"), "{message}");
        assert!(!message.contains("ada@example.com"), "{message}");
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Typed {
        at: Time,
        id: Uuid,
        data: Bytes,
    }

    #[test]
    fn time_uuid_and_bytes_keep_their_bson_types() {
        let value = Typed {
            at: time(1_700_000_000_123_456),
            id: uuid(),
            data: Bytes::from_text("hi"),
        };
        assert_eq!(
            written(&value),
            doc! {
                "at": DateTime::from_millis(1_700_000_000_123),
                "id": Binary { subtype: BinarySubtype::Uuid, bytes: uuid().as_bytes().to_vec() },
                "data": Binary { subtype: BinarySubtype::Generic, bytes: b"hi".to_vec() },
            }
        );
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct At {
        at: Time,
    }

    #[test]
    fn a_time_drops_its_microseconds_rounding_toward_the_past() {
        for (micros, millis) in [
            (0, 0),
            (999, 0),
            (1_000, 1),
            (-1, -1),
            (-1_000, -1),
            (-1_001, -2),
        ] {
            assert_eq!(
                written(&At { at: time(micros) }),
                doc! { "at": DateTime::from_millis(millis) },
                "{micros}"
            );
        }
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    enum Role {
        Admin,
        #[serde(rename = "member")]
        Member,
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Shapes {
        none: Option<String>,
        some: Option<i32>,
        list: Vec<String>,
        inner: Plain,
        map: HashMap<String, i64>,
        admin: Role,
        member: Role,
        #[serde(rename = "userName")]
        user_name: String,
        #[serde(skip)]
        secret: Option<String>,
    }

    #[test]
    fn options_lists_structs_maps_and_enums_take_their_bson_shapes() {
        let value = Shapes {
            none: None,
            some: Some(3),
            list: vec!["a".to_string(), "b".to_string()],
            inner: Plain {
                flag: false,
                name: "x".to_string(),
            },
            map: HashMap::from([("k".to_string(), 1)]),
            admin: Role::Admin,
            member: Role::Member,
            user_name: "ann".to_string(),
            secret: Some("hunter2".to_string()),
        };
        assert_eq!(
            written(&value),
            doc! {
                "none": Bson::Null,
                "some": 3,
                "list": ["a", "b"],
                "inner": { "flag": false, "name": "x" },
                "map": { "k": 1_i64 },
                "admin": "Admin",
                "member": "member",
                "userName": "ann",
            }
        );
        // The skipped field held a value, and the document has no trace of it.
        assert_eq!(value.secret.as_deref(), Some("hunter2"));
    }

    #[test]
    fn a_map_at_the_top_is_a_document() {
        let value = HashMap::from([("n".to_string(), 1_i64)]);
        assert_eq!(written(&value), doc! { "n": 1_i64 });
    }

    #[test]
    fn anything_but_a_struct_or_a_map_at_the_top_is_an_error() {
        for message in [
            message_of(document(&5_i64)),
            message_of(document("ada@example.com")),
            message_of(document(&vec![1, 2])),
            message_of(document(&None::<Plain>)),
            message_of(document(&uuid())),
        ] {
            assert!(message.contains("struct or a `HashMap"), "{message}");
            assert!(!message.contains("ada@example.com"), "{message}");
        }
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct WithId<T> {
        #[serde(rename = "_id")]
        id: T,
        name: String,
    }

    fn with_id<T>(id: T) -> WithId<T> {
        WithId {
            id,
            name: "Ada".to_string(),
        }
    }

    #[test]
    fn a_none_id_is_left_out() {
        assert_eq!(written(&with_id(None::<String>)), doc! { "name": "Ada" });
    }

    #[test]
    fn a_string_an_integer_or_a_uuid_id_is_kept() {
        assert_eq!(
            written(&with_id("ada")),
            doc! { "_id": "ada", "name": "Ada" }
        );
        assert_eq!(
            written(&with_id(Some("ada"))),
            doc! { "_id": "ada", "name": "Ada" }
        );
        assert_eq!(written(&with_id(7_u8)), doc! { "_id": 7, "name": "Ada" });
        assert_eq!(
            written(&with_id(7_i64)),
            doc! { "_id": 7_i64, "name": "Ada" }
        );
        assert_eq!(
            written(&with_id(uuid())),
            doc! {
                "_id": Binary { subtype: BinarySubtype::Uuid, bytes: uuid().as_bytes().to_vec() },
                "name": "Ada",
            }
        );
    }

    #[test]
    fn a_24_hex_string_id_stays_a_string() {
        let hex = "65a1b2c3d4e5f60718293a4b";
        assert_eq!(written(&with_id(hex)), doc! { "_id": hex, "name": "Ada" });
    }

    #[test]
    fn an_id_of_another_kind_is_an_error_naming_its_type() {
        for (message, kind) in [
            (message_of(document(&with_id(true))), "`bool`"),
            (message_of(document(&with_id(1.5_f64))), "float"),
            (message_of(document(&with_id(time(0)))), "`Time`"),
            (
                message_of(document(&with_id(Bytes::from_text("secret")))),
                "`Bytes`",
            ),
            (
                message_of(document(&with_id(Plain {
                    flag: true,
                    name: "secret".to_string(),
                }))),
                "struct",
            ),
            (message_of(document(&with_id(vec![1]))), "`Vec`"),
            (
                message_of(document(&with_id(HashMap::from([("a", 1)])))),
                "map",
            ),
        ] {
            assert!(message.contains("`_id`"), "{message}");
            assert!(message.contains(kind), "{message}");
            assert!(!message.contains("secret"), "{message}");
        }
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Nested {
        inner: WithId<Option<String>>,
    }

    #[test]
    fn only_the_top_level_id_is_checked() {
        let value = Nested {
            inner: with_id(None),
        };
        assert_eq!(
            written(&value),
            doc! { "inner": { "_id": Bson::Null, "name": "Ada" } }
        );
    }

    #[test]
    fn documents_writes_a_list_in_order() {
        let values = vec![with_id("a"), with_id("b")];
        assert_eq!(
            documents(&values).ok(),
            Some(vec![
                doc! { "_id": "a", "name": "Ada" },
                doc! { "_id": "b", "name": "Ada" },
            ])
        );
        assert_eq!(documents(&Vec::<Plain>::new()).ok(), Some(vec![]));
    }

    #[test]
    fn documents_names_the_document_that_fails() {
        let values = vec![Big { count: 1 }, Big { count: u64::MAX }];
        let message = message_of(documents(&values));
        assert!(message.contains("`count`"), "{message}");
        assert!(message.contains("document 2"), "{message}");
        let ids = vec![with_id(1.5_f64)];
        let message = message_of(documents(&ids));
        assert!(message.contains("`_id`"), "{message}");
        assert!(message.contains("document 1"), "{message}");
        let message = message_of(documents(&vec![1, 2]));
        assert!(message.contains("document 1"), "{message}");
        assert!(message.contains("struct or a `HashMap"), "{message}");
    }

    #[test]
    fn documents_of_something_not_a_list_is_an_error() {
        let message = message_of(documents(&Plain {
            flag: true,
            name: "x".to_string(),
        }));
        assert!(message.contains("`Vec`"), "{message}");
    }

    #[test]
    fn id_text_gives_each_kind_of_id_as_text() {
        let oid = ObjectId::from_bytes([1; 12]);
        assert_eq!(id_text(&Bson::ObjectId(oid)).ok(), Some(oid.to_hex()));
        assert_eq!(
            id_text(&Bson::String("ada".to_string())).ok(),
            Some("ada".to_string())
        );
        assert_eq!(id_text(&Bson::Int32(-7)).ok(), Some("-7".to_string()));
        assert_eq!(
            id_text(&Bson::Int64(1 << 40)).ok(),
            Some("1099511627776".to_string())
        );
        let id = Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uuid().as_bytes().to_vec(),
        });
        assert_eq!(
            id_text(&id).ok(),
            Some("01890a5d-ac96-774b-bcce-b302099a8057".to_string())
        );
        assert!(id_text(&Bson::Boolean(true)).is_err());
    }
}
