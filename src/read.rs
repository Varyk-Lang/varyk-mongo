// The deserializer: a BSON document into the type a Varyk program names
// (spec 3.2), by field name, with varyk-sql's rules for a missing field
// and a null. A message names the field and the type it was read into
// and never holds a value: the text a field's own deserializer gives to
// `custom` (a `Time` or a `Uuid` puts the text it read there) and the
// values serde passes to `invalid_type`, `invalid_value`, and
// `unknown_variant` are dropped (spec 5).

use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, Document};
use std::fmt;
use varyk_std::serde::de::{
    self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};
use varyk_std::{Error, Time, Uuid};

/// What a `Time`, a `Uuid`, and a `Bytes` ask for: each reads a string.
const TEXT_TYPES: &str = "a `Time`, a `Uuid`, or `Bytes`";

/// `doc` read as a `T`: a struct or a map by field name, any other type
/// from the document's one field other than `_id`, or from `_id` alone.
pub fn from_document<T: DeserializeOwned>(doc: Document) -> Result<T, Error> {
    T::deserialize(Whole { doc }).map_err(|e| Error::new(e.message()))
}

/// One step of the path to a field: a struct's field, a map's entry,
/// whose key is data and so is not named, a place in a list, or the one
/// field a scalar reads, whose key may be data too (a map's) and so is
/// not named either.
#[derive(Debug)]
enum Step {
    Field(String),
    Entry,
    Index(usize),
    Only,
}

#[derive(Debug)]
enum Problem {
    /// A field's own deserializer refused what it was handed; its text is
    /// dropped. The method that handed it names the type, `Into`.
    Refused,
    /// The value cannot be read into this type.
    Into(&'static str),
    /// The value is this BSON type, which cannot be read into that one.
    Holds(&'static str, &'static str),
    /// A null, outside an `Option`.
    Null,
    /// A number out of the range of this type.
    Range(&'static str),
    /// A double with a fractional part, read into this integer type.
    Fraction(&'static str),
    /// A date outside `Time`'s range.
    DateRange,
    /// Binary subtype 4 that is not 16 bytes.
    UuidLength,
    /// Binary subtype 3.
    LegacyUuid,
    /// A string that names none of an enum's variants, which are these.
    Variant(&'static [&'static str]),
    /// A field the type needs that the document lacks.
    Missing(&'static str),
    /// A field the document has twice.
    Duplicate(&'static str),
    /// A document read as one value with this many fields other than `_id`.
    Count(usize),
}

/// A failure while reading, with the path to the field it is about,
/// innermost step first.
#[derive(Debug)]
struct ReadError {
    problem: Problem,
    path: Vec<Step>,
}

impl ReadError {
    fn new(problem: Problem) -> ReadError {
        ReadError {
            problem,
            path: Vec::new(),
        }
    }

    /// This error, inside `step`.
    fn inside(mut self, step: Step) -> ReadError {
        self.path.push(step);
        self
    }

    /// A field's refusal named as a refusal of `into`; any other error
    /// as it is.
    fn refusing(self, into: &'static str) -> ReadError {
        match self.problem {
            Problem::Refused => ReadError {
                problem: Problem::Into(into),
                path: self.path,
            },
            _ => self,
        }
    }

    /// The message, naming the field by its path from the document.
    fn message(&self) -> String {
        let path: Vec<String> = self
            .path
            .iter()
            .rev()
            .map(|step| match step {
                Step::Field(name) => name.clone(),
                Step::Entry => "<key>".to_string(),
                Step::Index(index) => index.to_string(),
                Step::Only => "<field>".to_string(),
            })
            .collect();
        let path = path.join(".");
        // The full path of a field named inside the struct at `path`.
        let within = |name: &str| {
            if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}.{name}")
            }
        };
        let subject = if path.is_empty() {
            "the document".to_string()
        } else if matches!(self.path.as_slice(), [Step::Only]) {
            "the document's one field".to_string()
        } else {
            format!("the field `{path}`")
        };
        match &self.problem {
            Problem::Refused => format!("{subject} cannot be read into its type"),
            Problem::Into(into) => format!("{subject} cannot be read into {into}"),
            Problem::Holds(held, into) => {
                format!("{subject} holds {held}, which cannot be read into {into}")
            }
            Problem::Null => format!("{subject} is null, which only an `Option` field can hold"),
            Problem::Range(into) => format!("{subject} holds a number out of the range of {into}"),
            Problem::Fraction(into) => format!(
                "{subject} holds a number with a fractional part, which cannot be read into {into}"
            ),
            Problem::DateRange => format!(
                "{subject} holds a date outside the range of a `Time`, which runs from the year 0000 to the year 9999"
            ),
            Problem::UuidLength => format!("{subject} holds a UUID that is not 16 bytes long"),
            Problem::LegacyUuid => format!(
                "{subject} holds a legacy UUID (binary subtype 3) of an old driver, whose byte order is not known; store it as subtype 4"
            ),
            Problem::Variant(names) => {
                let names: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
                format!(
                    "{subject} names none of the enum's variants, which are {}",
                    names.join(", ")
                )
            }
            Problem::Missing(name) => format!(
                "the document has no field `{}`, which the type needs; an `Option` field or one with a `#[default]` may be missing",
                within(name)
            ),
            Problem::Duplicate(name) => {
                format!("the document has the field `{}` twice", within(name))
            }
            Problem::Count(0) => "a document read as one value must have one field other than `_id`, or `_id` alone; this one has none".to_string(),
            Problem::Count(count) => format!(
                "a document read as one value must have one field other than `_id`, or `_id` alone; this one has {count}"
            ),
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ReadError {}

impl de::Error for ReadError {
    /// A field's refusal, its text dropped: serde's `invalid_type`,
    /// `invalid_value`, `invalid_length`, and `unknown_field` come here
    /// too, so the value they are given is dropped with it.
    fn custom<M: fmt::Display>(_message: M) -> Self {
        ReadError::new(Problem::Refused)
    }

    /// Names the enum's variants, never the text that named none.
    fn unknown_variant(_variant: &str, expected: &'static [&'static str]) -> Self {
        ReadError::new(Problem::Variant(expected))
    }

    fn missing_field(field: &'static str) -> Self {
        ReadError::new(Problem::Missing(field))
    }

    fn duplicate_field(field: &'static str) -> Self {
        ReadError::new(Problem::Duplicate(field))
    }
}

/// The BSON type of a value, in the words of MongoDB's documentation.
fn kind(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "a double",
        Bson::String(_) => "a string",
        Bson::Array(_) => "an array",
        Bson::Document(_) => "an embedded document",
        Bson::Boolean(_) => "a boolean",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "a regular expression",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "JavaScript code",
        Bson::Int32(_) => "an int32",
        Bson::Int64(_) => "an int64",
        Bson::Timestamp(_) => "a timestamp",
        Bson::Binary(_) => "binary data",
        Bson::ObjectId(_) => "an ObjectId",
        Bson::DateTime(_) => "a date",
        Bson::Symbol(_) => "a symbol",
        Bson::Decimal128(_) => "a Decimal128",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "a max key",
        Bson::MinKey => "a min key",
        Bson::DbPointer(_) => "a DBPointer",
    }
}

/// The error of `value`, which cannot be read into `into`.
fn mismatch(value: &Bson, into: &'static str) -> ReadError {
    match value {
        Bson::Null => ReadError::new(Problem::Null),
        other => ReadError::new(Problem::Holds(kind(other), into)),
    }
}

/// A whole document: a struct or a map reads its fields, and any other
/// type reads its one field other than `_id`, or `_id` alone.
struct Whole {
    doc: Document,
}

impl Whole {
    /// The key of the field a scalar reads, or the count of the fields
    /// other than `_id`.
    fn scalar_key(&self) -> Result<&str, usize> {
        let mut others = self.doc.keys().filter(|key| *key != "_id");
        match (others.next(), others.next()) {
            (Some(key), None) => Ok(key),
            (None, None) if self.doc.contains_key("_id") => Ok("_id"),
            (None, _) => Err(0),
            (Some(_), Some(_)) => Err(self.doc.keys().filter(|key| *key != "_id").count()),
        }
    }

    /// `read` of the field a scalar reads, its errors naming the field
    /// when it is `_id` and calling it the document's one field otherwise.
    fn scalar<T>(
        mut self,
        read: impl FnOnce(Value) -> Result<T, ReadError>,
    ) -> Result<T, ReadError> {
        let key = match self.scalar_key() {
            Ok(key) => key.to_string(),
            Err(count) => return Err(ReadError::new(Problem::Count(count))),
        };
        let value = self.doc.remove(&key).unwrap_or(Bson::Null);
        let step = if key == "_id" {
            Step::Field(key)
        } else {
            Step::Only
        };
        read(Value { value }).map_err(|e| e.inside(step))
    }

    fn fields(self, names: bool) -> Fields {
        Fields {
            entries: self.doc.into_iter(),
            pending: None,
            names,
        }
    }
}

/// Methods a scalar `T` calls: each reads the document's one value.
macro_rules! scalar_methods {
    ($($method:ident),* $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
                self.scalar(|value| value.$method(visitor))
            }
        )*
    };
}

impl<'de> de::Deserializer<'de> for Whole {
    type Error = ReadError;

    scalar_methods!(
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_unit,
        deserialize_seq,
        deserialize_identifier,
    );

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.deserialize_map(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        visitor
            .visit_map(self.fields(true))
            .map_err(|e| e.refusing("a struct"))
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor
            .visit_map(self.fields(false))
            .map_err(|e| e.refusing("a map"))
    }

    /// `None` when the field a scalar reads is null, as for
    /// `{ _id: null }` read into an `Option<string>`; otherwise the
    /// document is there, and the type inside reads it.
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        let null = match self.scalar_key() {
            Ok(key) => matches!(self.doc.get(key), Some(Bson::Null)),
            Err(_) => false,
        };
        if null {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.scalar(|value| value.deserialize_unit_struct(name, visitor))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.scalar(|value| value.deserialize_enum(name, variants, visitor))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_unit()
    }
}

/// A document's fields, for a struct (`names`: the path names a field)
/// or a map (whose keys are data, so the path does not name them).
struct Fields {
    entries: <Document as IntoIterator>::IntoIter,
    /// The key and value whose value comes next.
    pending: Option<(String, Bson)>,
    names: bool,
}

impl<'de> MapAccess<'de> for Fields {
    type Error = ReadError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, ReadError> {
        match self.entries.next() {
            Some((key, value)) => {
                let read = seed.deserialize(key.as_str().into_deserializer())?;
                self.pending = Some((key, value));
                Ok(Some(read))
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, ReadError> {
        let Some((key, value)) = self.pending.take() else {
            return Err(ReadError::new(Problem::Refused));
        };
        let step = if self.names {
            Step::Field(key)
        } else {
            Step::Entry
        };
        seed.deserialize(Value { value })
            .map_err(|e| e.inside(step))
    }
}

/// An array's items, for a `Vec`.
struct Items {
    items: std::vec::IntoIter<Bson>,
    next: usize,
}

impl<'de> SeqAccess<'de> for Items {
    type Error = ReadError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, ReadError> {
        match self.items.next() {
            Some(value) => {
                let index = self.next;
                self.next += 1;
                seed.deserialize(Value { value })
                    .map(Some)
                    .map_err(|e| e.inside(Step::Index(index)))
            }
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

/// One BSON value, read into one field.
struct Value {
    value: Bson,
}

impl Value {
    /// An integer of type `T`, `into` in messages: an int32 or int64 in
    /// range, or a double with no fractional part in range.
    fn integer<T: TryFrom<i128>>(self, into: &'static str) -> Result<T, ReadError> {
        let range = || ReadError::new(Problem::Range(into));
        match self.value {
            Bson::Int32(n) => T::try_from(i128::from(n)).map_err(|_| range()),
            Bson::Int64(n) => T::try_from(i128::from(n)).map_err(|_| range()),
            Bson::Double(x) if x.is_finite() && x.fract() == 0.0 => {
                // `as` saturates, and a saturated value fails `try_from`.
                T::try_from(x as i128).map_err(|_| range())
            }
            Bson::Double(x) if x.is_finite() => Err(ReadError::new(Problem::Fraction(into))),
            Bson::Double(_) => Err(range()),
            other => Err(mismatch(&other, into)),
        }
    }

    /// A float, `into` in messages: a double, or an integer as Rust's
    /// `as` converts it.
    fn float(self, into: &'static str) -> Result<f64, ReadError> {
        match self.value {
            Bson::Double(x) => Ok(x),
            Bson::Int32(n) => Ok(f64::from(n)),
            Bson::Int64(n) => Ok(n as f64),
            other => Err(mismatch(&other, into)),
        }
    }

    /// A field that asks for a string, `into` in messages: a string; a
    /// date as `Time`'s ISO text; binary subtype 4 as a hyphenated
    /// `Uuid`; other binary as its bytes, which a `string` takes when
    /// they are UTF-8, a `Uuid` when there are 16, and a `Bytes` always.
    /// An ObjectId as 24 hex digits and a Decimal128 as its text only
    /// when `string`, for a `string` field: hex digits and a decimal can
    /// be valid base64, which a `Bytes` would silently take.
    fn text<'de, V: Visitor<'de>>(
        self,
        visitor: V,
        into: &'static str,
        string: bool,
    ) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::String(text) => visitor.visit_string(text),
            Bson::DateTime(date) => {
                let time = date
                    .timestamp_millis()
                    .checked_mul(1000)
                    .and_then(|micros| Time::from_unix_micros(micros).ok());
                match time {
                    Some(time) => visitor.visit_string(time.to_iso()),
                    None => Err(ReadError::new(Problem::DateRange)),
                }
            }
            Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes,
            }) => match <[u8; 16]>::try_from(bytes.as_slice()) {
                Ok(bytes) => visitor.visit_string(Uuid::from_bytes(bytes).to_string()),
                Err(_) => Err(ReadError::new(Problem::UuidLength)),
            },
            Bson::Binary(Binary {
                subtype: BinarySubtype::UuidOld,
                ..
            }) => Err(ReadError::new(Problem::LegacyUuid)),
            Bson::Binary(Binary { bytes, .. }) => visitor.visit_byte_buf(bytes),
            Bson::ObjectId(oid) if string => visitor.visit_string(oid.to_hex()),
            Bson::Decimal128(number) if string => visitor.visit_string(number.to_string()),
            other => Err(mismatch(&other, into)),
        }
        .map_err(|e| e.refusing(into))
    }
}

impl<'de> de::Deserializer<'de> for Value {
    type Error = ReadError;

    /// Asked for by no type a Varyk program reads; each BSON type as the
    /// methods below read it.
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Double(x) => visitor.visit_f64(x),
            Bson::Int32(n) => visitor.visit_i32(n),
            Bson::Int64(n) => visitor.visit_i64(n),
            Bson::Boolean(b) => visitor.visit_bool(b),
            Bson::Null => visitor.visit_unit(),
            Bson::Array(_) => self.deserialize_seq(visitor),
            Bson::Document(_) => self.deserialize_map(visitor),
            _ => self.text(visitor, "a value", true),
        }
        .map_err(|e| e.refusing("its type"))
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Boolean(b) => visitor.visit_bool(b),
            other => Err(mismatch(&other, "a `bool`")),
        }
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_i8(self.integer("an `i8`")?)
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_i16(self.integer("an `i16`")?)
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_i32(self.integer("an `i32`")?)
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_i64(self.integer("an `i64`")?)
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_u8(self.integer("a `u8`")?)
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_u16(self.integer("a `u16`")?)
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_u32(self.integer("a `u32`")?)
    }

    /// A `u64`, and a `usize`, which serde reads as one.
    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_u64(self.integer("a `u64`")?)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_f32(self.float("an `f32`")? as f32)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_f64(self.float("an `f64`")?)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.text(visitor, "a character", false)
    }

    /// A `Time`, a `Uuid`, or a `Bytes` asks for this.
    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.text(visitor, TEXT_TYPES, false)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.text(visitor, "a `string`", true)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.text(visitor, "bytes", false)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.text(visitor, "bytes", false)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Null => visitor.visit_unit(),
            other => Err(mismatch(&other, "nothing")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Array(items) => visitor
                .visit_seq(Items {
                    items: items.into_iter(),
                    next: 0,
                })
                .map_err(|e| e.refusing("a `Vec`")),
            other => Err(mismatch(&other, "a `Vec`")),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Document(doc) => visitor
                .visit_map(Whole { doc }.fields(false))
                .map_err(|e| e.refusing("a map")),
            other => Err(mismatch(&other, "a map")),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::Document(doc) => visitor
                .visit_map(Whole { doc }.fields(true))
                .map_err(|e| e.refusing("a struct")),
            other => Err(mismatch(&other, "a struct")),
        }
    }

    /// An enum whose variants carry no data, from a string naming one.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ReadError> {
        match self.value {
            Bson::String(name) => visitor
                .visit_enum(IntoDeserializer::<ReadError>::into_deserializer(name))
                .map_err(|e| e.refusing("an enum")),
            other => Err(mismatch(&other, "an enum")),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        self.deserialize_string(visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ReadError> {
        visitor.visit_unit()
    }
}

#[cfg(test)]
mod tests {
    use super::from_document;
    use mongodb::bson::oid::ObjectId;
    use mongodb::bson::raw::CString;
    use mongodb::bson::spec::BinarySubtype;
    use mongodb::bson::{
        Binary, Bson, DateTime, Decimal128, Document, JavaScriptCodeWithScope, Regex, Timestamp,
        doc,
    };
    use std::collections::HashMap;
    use std::fmt::Debug;
    use varyk_std::serde::de::DeserializeOwned;
    use varyk_std::serde::{Deserialize, Serialize};
    use varyk_std::{Bytes, Time, Uuid};

    /// The message of a failed result, or "no error".
    fn message_of<T>(result: Result<T, varyk_std::Error>) -> String {
        match result {
            Ok(_) => "no error".to_string(),
            Err(e) => e.message().to_string(),
        }
    }

    /// `doc` read as a `T`, or `None`.
    fn read<T: DeserializeOwned>(doc: Document) -> Option<T> {
        from_document(doc).ok()
    }

    /// The field `v` of `{ v: value }` read as a `T`.
    fn field<T: DeserializeOwned>(value: Bson) -> Result<T, varyk_std::Error> {
        from_document::<One<T>>(doc! { "v": value }).map(|one| one.v)
    }

    /// The message of reading `{ v: value }` into a `T`, which must
    /// name the field and not hold `secret`.
    fn refused<T: DeserializeOwned + Debug>(value: Bson, secret: &str) -> String {
        let message = message_of(field::<T>(value));
        assert!(message.contains("`v`"), "{message}");
        assert!(secret.is_empty() || !message.contains(secret), "{message}");
        message
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    struct One<T> {
        v: T,
    }

    fn uuid() -> Uuid {
        Uuid::from_bytes([
            1, 137, 10, 93, 172, 150, 119, 75, 188, 206, 179, 2, 9, 154, 128, 87,
        ])
    }

    const UUID_TEXT: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

    fn time(micros: i64) -> Time {
        match Time::from_unix_micros(micros) {
            Ok(t) => t,
            Err(_) => Time::now(),
        }
    }

    fn binary(subtype: BinarySubtype, bytes: &[u8]) -> Bson {
        Bson::Binary(Binary {
            subtype,
            bytes: bytes.to_vec(),
        })
    }

    #[test]
    fn int32_and_int64_read_into_an_integer_in_its_range() {
        assert_eq!(field::<i8>(Bson::Int32(-128)).ok(), Some(-128));
        assert_eq!(field::<i16>(Bson::Int64(300)).ok(), Some(300));
        assert_eq!(field::<i32>(Bson::Int64(-5)).ok(), Some(-5));
        assert_eq!(field::<i64>(Bson::Int32(7)).ok(), Some(7));
        assert_eq!(field::<i64>(Bson::Int64(i64::MIN)).ok(), Some(i64::MIN));
        assert_eq!(field::<u8>(Bson::Int32(255)).ok(), Some(255));
        assert_eq!(field::<u16>(Bson::Int32(65535)).ok(), Some(65535));
        assert_eq!(
            field::<u32>(Bson::Int64(4_294_967_295)).ok(),
            Some(u32::MAX)
        );
        assert_eq!(
            field::<u64>(Bson::Int64(i64::MAX)).ok(),
            Some(9_223_372_036_854_775_807)
        );
        assert_eq!(field::<usize>(Bson::Int32(3)).ok(), Some(3));
    }

    #[test]
    fn an_integer_out_of_its_range_is_an_error_naming_the_field() {
        let message = refused::<u8>(Bson::Int32(300), "300");
        assert!(message.contains("range"), "{message}");
        assert!(message.contains("`u8`"), "{message}");
        refused::<i8>(Bson::Int64(-129), "129");
        refused::<u64>(Bson::Int32(-1), "-1");
        refused::<u32>(Bson::Int64(1 << 40), "1099511627776");
        refused::<i32>(Bson::Int64(1 << 40), "1099511627776");
    }

    #[test]
    fn an_integer_reads_into_a_float_as_as_converts() {
        assert_eq!(field::<f64>(Bson::Int32(3)).ok(), Some(3.0));
        assert_eq!(field::<f32>(Bson::Int64(-4)).ok(), Some(-4.0));
        assert_eq!(
            field::<f64>(Bson::Int64(i64::MAX)).ok(),
            Some(i64::MAX as f64)
        );
    }

    #[test]
    fn a_double_reads_into_a_float_and_into_an_integer_only_when_whole() {
        assert_eq!(field::<f64>(Bson::Double(2.5)).ok(), Some(2.5));
        assert_eq!(field::<f32>(Bson::Double(0.5)).ok(), Some(0.5));
        assert_eq!(field::<i64>(Bson::Double(3.0)).ok(), Some(3));
        assert_eq!(field::<u8>(Bson::Double(255.0)).ok(), Some(255));
        let message = refused::<i64>(Bson::Double(2.5), "2.5");
        assert!(message.contains("fractional"), "{message}");
        let message = refused::<u8>(Bson::Double(256.0), "256");
        assert!(message.contains("range"), "{message}");
        refused::<i64>(Bson::Double(1e30), "");
        refused::<i64>(Bson::Double(f64::NAN), "");
        refused::<i64>(Bson::Double(f64::INFINITY), "");
    }

    #[test]
    fn a_boolean_reads_into_a_bool() {
        assert_eq!(field::<bool>(Bson::Boolean(true)).ok(), Some(true));
        let message = refused::<bool>(Bson::String("secret".to_string()), "secret");
        assert!(message.contains("`bool`"), "{message}");
        refused::<bool>(Bson::Int32(1), "");
    }

    #[test]
    fn a_string_reads_into_a_string_time_uuid_or_bytes() {
        assert_eq!(
            field::<String>(Bson::String("Ada".to_string())).ok(),
            Some("Ada".to_string())
        );
        assert_eq!(
            field::<Time>(Bson::String("2026-10-07T12:00:00.5Z".to_string())).ok(),
            Some(time(1_791_374_400_500_000))
        );
        assert_eq!(
            field::<Uuid>(Bson::String(UUID_TEXT.to_string())).ok(),
            Some(uuid())
        );
        assert_eq!(
            field::<Bytes>(Bson::String("aGk=".to_string())).ok(),
            Some(Bytes::from_text("hi"))
        );
    }

    #[test]
    fn a_string_that_is_not_the_type_is_an_error_without_the_text() {
        let message = refused::<Time>(Bson::String("secret-o-clock".to_string()), "secret");
        assert!(message.contains("`Time`"), "{message}");
        refused::<Uuid>(Bson::String("secret-id".to_string()), "secret");
        refused::<Bytes>(Bson::String("secret!!".to_string()), "secret");
        let message = refused::<i64>(Bson::String("secret".to_string()), "secret");
        assert!(message.contains("`i64`"), "{message}");
    }

    #[test]
    fn a_date_reads_into_a_time_and_into_a_string_as_its_iso_text() {
        let date = Bson::DateTime(DateTime::from_millis(1_791_374_400_500));
        assert_eq!(
            field::<Time>(date.clone()).ok(),
            Some(time(1_791_374_400_500_000))
        );
        assert_eq!(
            field::<String>(date).ok(),
            Some("2026-10-07T12:00:00.5Z".to_string())
        );
        assert_eq!(
            field::<String>(Bson::DateTime(DateTime::from_millis(-1))).ok(),
            Some(time(-1_000).to_iso())
        );
    }

    #[test]
    fn a_date_outside_the_range_of_a_time_is_an_error_naming_the_field() {
        for millis in [i64::MAX, i64::MIN, 253_402_300_800_000, -62_167_219_200_001] {
            let message = refused::<Time>(Bson::DateTime(DateTime::from_millis(millis)), "");
            assert!(message.contains("date"), "{message}");
            assert!(!message.contains(&millis.to_string()), "{message}");
        }
    }

    #[test]
    fn binary_subtype_4_reads_into_a_uuid_and_into_a_string_hyphenated() {
        let id = binary(BinarySubtype::Uuid, uuid().as_bytes());
        assert_eq!(field::<Uuid>(id.clone()).ok(), Some(uuid()));
        assert_eq!(field::<String>(id).ok(), Some(UUID_TEXT.to_string()));
        refused::<Uuid>(binary(BinarySubtype::Uuid, &[1; 15]), "");
    }

    #[test]
    fn binary_subtype_3_is_an_error_naming_the_field() {
        let old = binary(BinarySubtype::UuidOld, uuid().as_bytes());
        let message = refused::<Uuid>(old.clone(), "");
        assert!(message.contains("subtype 3"), "{message}");
        refused::<String>(old.clone(), "");
        refused::<Bytes>(old, "");
    }

    #[test]
    fn other_binary_reads_into_bytes_or_a_16_byte_uuid() {
        assert_eq!(
            field::<Bytes>(binary(BinarySubtype::Generic, b"\x00\xff")).ok(),
            Some(Bytes::from_base64("AP8=").ok()).flatten()
        );
        assert_eq!(
            field::<Bytes>(binary(BinarySubtype::UserDefined(0x80), b"hi")).ok(),
            Some(Bytes::from_text("hi"))
        );
        assert_eq!(
            field::<Uuid>(binary(BinarySubtype::Generic, uuid().as_bytes())).ok(),
            Some(uuid())
        );
        refused::<Uuid>(binary(BinarySubtype::Generic, &[1; 4]), "");
    }

    #[test]
    fn other_binary_reads_into_a_string_only_when_it_is_utf8() {
        assert_eq!(
            field::<String>(binary(BinarySubtype::Generic, b"secret")).ok(),
            Some("secret".to_string())
        );
        let message = refused::<String>(binary(BinarySubtype::Generic, b"secret\xff"), "secret");
        assert!(message.contains("`string`"), "{message}");
    }

    #[test]
    fn an_object_id_reads_into_a_string_as_hex() {
        let oid = ObjectId::from_bytes([0xab; 12]);
        assert_eq!(
            field::<String>(Bson::ObjectId(oid)).ok(),
            Some("abababababababababababab".to_string())
        );
        refused::<i64>(Bson::ObjectId(oid), "abab");
    }

    #[test]
    fn an_object_id_or_a_decimal128_never_reads_into_bytes_uuid_or_time() {
        let oid = Bson::ObjectId(ObjectId::from_bytes([0xab; 12]));
        let parsed = "1234".parse::<Decimal128>();
        assert!(parsed.is_ok());
        let mut values = vec![oid];
        if let Ok(number) = parsed {
            values.push(Bson::Decimal128(number));
        }
        for value in values {
            for message in [
                refused::<Bytes>(value.clone(), "abab"),
                refused::<Uuid>(value.clone(), "abab"),
                refused::<Time>(value, "abab"),
            ] {
                assert!(message.contains("`Bytes`"), "{message}");
                assert!(!message.contains("1234"), "{message}");
            }
        }
    }

    #[test]
    fn a_decimal128_reads_into_a_string_as_its_text() {
        let parsed = "12.50".parse::<Decimal128>();
        assert!(parsed.is_ok());
        if let Ok(number) = parsed {
            assert_eq!(
                field::<String>(Bson::Decimal128(number)).ok(),
                Some("12.50".to_string())
            );
            refused::<f64>(Bson::Decimal128(number), "12.5");
        }
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Address {
        city: String,
        zip: u32,
    }

    #[test]
    fn an_embedded_document_reads_into_a_struct_or_a_map() {
        let inner = Bson::Document(doc! { "city": "Vilnius", "zip": 1000 });
        assert_eq!(
            field::<Address>(inner.clone()).ok(),
            Some(Address {
                city: "Vilnius".to_string(),
                zip: 1000
            })
        );
        let scores = Bson::Document(doc! { "a": 1, "b": 2_i64 });
        assert_eq!(
            field::<HashMap<String, i64>>(scores).ok(),
            Some(HashMap::from([("a".to_string(), 1), ("b".to_string(), 2)]))
        );
        refused::<Address>(Bson::Array(vec![]), "");
        refused::<HashMap<String, i64>>(Bson::String("secret".to_string()), "secret");
    }

    #[test]
    fn an_array_reads_into_a_vec() {
        let tags = Bson::Array(vec![
            Bson::String("a".to_string()),
            Bson::String("b".to_string()),
        ]);
        assert_eq!(
            field::<Vec<String>>(tags).ok(),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        refused::<Vec<String>>(Bson::String("secret".to_string()), "secret");
    }

    #[test]
    fn a_nested_field_is_named_by_its_path() {
        let message = message_of(field::<Address>(Bson::Document(
            doc! { "city": "Vilnius", "zip": "secret" },
        )));
        assert!(message.contains("`v.zip`"), "{message}");
        assert!(!message.contains("secret"), "{message}");
        let message = message_of(field::<Vec<Address>>(Bson::Array(vec![
            Bson::Document(doc! { "city": "a", "zip": 1 }),
            Bson::Document(doc! { "city": "b", "zip": -1 }),
        ])));
        assert!(message.contains("`v.1.zip`"), "{message}");
        // A map's key is data, so it is not named.
        let message = message_of(field::<HashMap<String, i64>>(Bson::Document(
            doc! { "ada@example.com": "secret" },
        )));
        assert!(message.contains("`v.<key>`"), "{message}");
        assert!(!message.contains("ada@example.com"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn a_bson_type_the_table_does_not_list_is_an_error_naming_the_field() {
        let mut others = vec![
            Bson::JavaScriptCode("secret()".to_string()),
            Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code: "secret()".to_string(),
                scope: doc! {},
            }),
            Bson::Timestamp(Timestamp {
                time: 1,
                increment: 2,
            }),
            Bson::MinKey,
            Bson::MaxKey,
            Bson::Symbol("secret".to_string()),
            Bson::Undefined,
        ];
        if let (Ok(pattern), Ok(options)) = (CString::try_from("secret"), CString::try_from("i")) {
            others.push(Bson::RegularExpression(Regex { pattern, options }));
        }
        assert_eq!(others.len(), 8);
        for value in others {
            refused::<String>(value.clone(), "secret");
            refused::<i64>(value.clone(), "secret");
            refused::<Option<String>>(value, "secret");
        }
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Defaults {
        name: String,
        nickname: Option<String>,
        #[serde(default = "Defaults::varyk_default_age")]
        age: u8,
        #[serde(skip)]
        cache: Option<String>,
    }

    impl Defaults {
        fn varyk_default_age() -> u8 {
            18
        }
    }

    #[test]
    fn a_missing_field_is_none_its_default_or_an_error_naming_it() {
        assert_eq!(
            read::<Defaults>(doc! { "name": "Ada", "extra": "ignored", "cache": "x" }),
            Some(Defaults {
                name: "Ada".to_string(),
                nickname: None,
                age: 18,
                cache: None,
            })
        );
        let message = message_of(from_document::<Defaults>(doc! { "nickname": "secret" }));
        assert!(message.contains("`name`"), "{message}");
        assert!(!message.contains("secret"), "{message}");
        let message = message_of(field::<Address>(Bson::Document(doc! { "city": "x" })));
        assert!(message.contains("`v.zip`"), "{message}");
    }

    #[test]
    fn a_null_is_none_in_an_option_and_an_error_elsewhere() {
        assert_eq!(
            read::<Defaults>(doc! { "name": "Ada", "nickname": Bson::Null }),
            Some(Defaults {
                name: "Ada".to_string(),
                nickname: None,
                age: 18,
                cache: None,
            })
        );
        assert_eq!(field::<Option<i64>>(Bson::Int32(3)).ok(), Some(Some(3)));
        let message = refused::<String>(Bson::Null, "");
        assert!(message.contains("null"), "{message}");
        refused::<u8>(Bson::Null, "");
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    enum Role {
        Admin,
        #[serde(rename = "member")]
        Member,
    }

    #[test]
    fn a_string_reads_into_an_enum_by_its_name() {
        assert_eq!(
            field::<Role>(Bson::String("Admin".to_string())).ok(),
            Some(Role::Admin)
        );
        assert_eq!(
            field::<Role>(Bson::String("member".to_string())).ok(),
            Some(Role::Member)
        );
        let message = refused::<Role>(Bson::String("secret".to_string()), "secret");
        assert!(message.contains("`Admin`"), "{message}");
        assert!(message.contains("`member`"), "{message}");
        refused::<Role>(Bson::Int32(0), "");
    }

    #[test]
    fn a_scalar_reads_from_the_one_field_other_than_the_id() {
        assert_eq!(
            read::<i64>(doc! { "_id": Bson::Null, "total": 12 }),
            Some(12)
        );
        assert_eq!(read::<i64>(doc! { "total": 12_i64 }), Some(12));
        assert_eq!(
            read::<String>(doc! { "_id": "Vilnius" }),
            Some("Vilnius".to_string())
        );
        assert_eq!(
            read::<Vec<i64>>(doc! { "_id": 1, "list": [1, 2] }),
            Some(vec![1, 2])
        );
        assert_eq!(
            read::<Uuid>(doc! { "_id": binary(BinarySubtype::Uuid, uuid().as_bytes()) }),
            Some(uuid())
        );
        assert_eq!(
            read::<Option<String>>(doc! { "_id": Bson::Null }),
            Some(None)
        );
        assert_eq!(
            read::<Option<i64>>(doc! { "_id": 1, "total": 5 }),
            Some(Some(5))
        );
    }

    #[test]
    fn a_scalar_from_another_shape_is_an_error_naming_the_count() {
        let message = message_of(from_document::<i64>(doc! { "a": 1, "b": 2 }));
        assert!(message.contains("one field other than `_id`"), "{message}");
        assert!(message.contains('2'), "{message}");
        let message = message_of(from_document::<i64>(doc! {}));
        assert!(message.contains("one field other than `_id`"), "{message}");
        let message = message_of(from_document::<i64>(doc! { "total": "secret" }));
        assert!(message.contains("the document's one field"), "{message}");
        assert!(!message.contains("total"), "{message}");
        assert!(!message.contains("secret"), "{message}");
        // A data key, as a map's, never reaches the message.
        let message = message_of(from_document::<i64>(
            doc! { "_id": 1, "ada@example.com": "secret" },
        ));
        assert!(message.contains("the document's one field"), "{message}");
        assert!(!message.contains("ada@example.com"), "{message}");
        let message = message_of(from_document::<Vec<Address>>(doc! {
            "ada@example.com": [{ "city": "x", "zip": -1 }],
        }));
        assert!(message.contains("`<field>.0.zip`"), "{message}");
        assert!(!message.contains("ada@example.com"), "{message}");
        let message = message_of(from_document::<i64>(doc! { "_id": "secret" }));
        assert!(message.contains("`_id`"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn an_option_of_a_struct_reads_the_whole_document() {
        assert_eq!(
            read::<Option<Address>>(doc! { "city": "x", "zip": 1 }),
            Some(Some(Address {
                city: "x".to_string(),
                zip: 1
            }))
        );
    }

    #[test]
    fn errors_drop_the_text_given_to_custom_and_the_values_serde_gives() {
        use super::ReadError;
        use varyk_std::serde::de::{Error as _, Unexpected};
        for e in [
            ReadError::custom("secret"),
            ReadError::invalid_type(Unexpected::Str("secret"), &"a number"),
            ReadError::invalid_value(Unexpected::Str("secret"), &"a number"),
            ReadError::invalid_length(7, &"16 bytes"),
            ReadError::unknown_variant("secret", &["Admin"]),
            ReadError::unknown_field("secret", &["name"]),
        ] {
            let message = e.message();
            assert!(!message.contains("secret"), "{message}");
            assert!(!message.contains('7'), "{message}");
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    enum Kind {
        Plain,
        #[serde(rename = "fancy")]
        Fancy,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Inner {
        label: String,
        weight: f64,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(crate = "varyk_std::serde")]
    struct Everything {
        #[serde(rename = "_id")]
        id: Uuid,
        flag: bool,
        name: String,
        a: i8,
        b: i16,
        c: i32,
        d: i64,
        e: u8,
        f: u16,
        g: u32,
        h: u64,
        i: usize,
        j: f32,
        k: f64,
        at: Time,
        data: Bytes,
        none: Option<String>,
        some: Option<i64>,
        tags: Vec<String>,
        inner: Inner,
        list: Vec<Inner>,
        map: HashMap<String, u16>,
        kind: Kind,
        fancy: Kind,
        #[serde(rename = "userName")]
        user_name: String,
        #[serde(skip)]
        cache: Option<String>,
    }

    #[test]
    fn a_struct_with_every_type_round_trips() {
        let value = Everything {
            id: uuid(),
            flag: true,
            name: "Ada".to_string(),
            a: i8::MIN,
            b: i16::MAX,
            c: i32::MIN,
            d: i64::MAX,
            e: u8::MAX,
            f: u16::MAX,
            g: u32::MAX,
            h: i64::MAX as u64,
            i: 42,
            j: 1.5,
            k: -0.25,
            at: time(1_791_374_400_123_000),
            data: Bytes::from_text("hi"),
            none: None,
            some: Some(-3),
            tags: vec!["a".to_string(), "b".to_string()],
            inner: Inner {
                label: "x".to_string(),
                weight: 2.0,
            },
            list: vec![Inner {
                label: "y".to_string(),
                weight: 0.5,
            }],
            map: HashMap::from([("k".to_string(), 9)]),
            kind: Kind::Plain,
            fancy: Kind::Fancy,
            user_name: "ann".to_string(),
            cache: Some("dropped".to_string()),
        };
        let written = crate::write::document(&value);
        assert!(written.is_ok(), "{}", message_of(written.clone()));
        let back = written.and_then(from_document::<Everything>);
        assert_eq!(
            back.ok(),
            Some(Everything {
                cache: None,
                ..value
            })
        );
    }

    #[test]
    fn a_time_round_trips_to_the_millisecond() {
        let value = One {
            v: time(1_791_374_400_123_999),
        };
        let back =
            crate::write::document(&SerOne { v: value.v }).and_then(from_document::<One<Time>>);
        assert_eq!(
            back.ok(),
            Some(One {
                v: time(1_791_374_400_123_000)
            })
        );
    }

    #[derive(Serialize)]
    #[serde(crate = "varyk_std::serde")]
    struct SerOne<T> {
        v: T,
    }
}
