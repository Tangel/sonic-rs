use std::{cell::Cell, mem::ManuallyDrop, result::Result as StdResult, slice};

use ::serde::{
    de::{
        self, Deserialize, DeserializeSeed, EnumAccess, Expected, IntoDeserializer, MapAccess,
        SeqAccess, Unexpected, VariantAccess, Visitor,
    },
    forward_to_deserialize_any,
};

use super::node::ValueRef;
use crate::{
    error::{Error, ErrorCode},
    reader::Reader,
    serde::{number::N, tri},
    value::{node::Value, Object},
};

/// Interpret a `sonic_rs::Value` as an instance of type `T`.
///
/// # Example
///
/// ```
/// use serde::Deserialize;
/// use sonic_rs::json;
///
/// #[derive(Deserialize, Debug)]
/// struct User {
///     string: String,
///     number: i32,
///     array: Vec<String>,
/// }
///
///  // The type of `j` is `sonic_rs::Value`
///  let j = json!({
///      "string": "hello",
///      "number": 123,
///      "array": ["a", "b", "c"],
///  });
///  let u: User = sonic_rs::from_value(&j).unwrap();
///  assert_eq!(u.string, "hello");
///  assert_eq!(u.number, 123);
///  assert_eq!(u.array, vec!["a", "b", "c"]);
/// ```
///
/// # Errors
///
/// This conversion can fail if the structure of the Value does not match the
/// structure expected by `T`, for example if `T` is a struct type but the Value
/// contains something other than a JSON map. It can also fail if the structure
/// is correct but `T`'s implementation of `Deserialize` decides that something
/// is wrong with the data, for example required struct fields are missing from
/// the JSON map or some number is too big to fit in the expected primitive
/// type.
pub fn from_value<'de, T>(value: &'de Value) -> Result<T, Error>
where
    T: Deserialize<'de>,
{
    T::deserialize(value)
}

impl<'de> Deserialize<'de> for Value {
    /// Deserialize this value from a `Deserializer`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sonic_rs::Value;
    ///
    /// let v: Value = sonic_rs::from_str(r#"{"a": 1, "b": 2}"#).unwrap();
    /// assert_eq!(v["a"], 1);
    /// assert_eq!(v["b"], 2);
    /// ```
    fn deserialize<D>(deserializer: D) -> StdResult<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        deserializer.deserialize_newtype_struct(TOKEN, ValueVisitor)
    }
}

pub(crate) const TOKEN: &str = "$sonic_rs::private::Value";

struct NativeValue {
    marker: *const u8,
    value: Value,
}

thread_local! {
    // NativeValueGuard empties/restores the slot on every return or unwind.
    // Only the storage is non-dropping: avoiding TLS destructor registration
    // keeps the handoff cheap and available during other TLS destructors.
    static NATIVE_VALUE: ManuallyDrop<Cell<Option<NativeValue>>> =
        const { ManuallyDrop::new(Cell::new(None)) };
}

struct NativeValueGuard<'a> {
    slot: &'a Cell<Option<NativeValue>>,
    previous: Option<NativeValue>,
}

impl Drop for NativeValueGuard<'_> {
    fn drop(&mut self) {
        // Restore a reentrant caller and drop any value the visitor did not take.
        self.slot.set(self.previous.take());
    }
}

/// Transfer an already parsed DOM without exposing its representation to Serde.
#[inline]
pub(crate) fn visit_native_value<'de, V>(value: Value, visitor: V) -> Result<V::Value, Error>
where
    V: Visitor<'de>,
{
    NATIVE_VALUE
        .try_with(|slot| {
            let mut marker = [0_u8];
            let previous = slot.replace(Some(NativeValue {
                marker: marker.as_mut_ptr(),
                value,
            }));
            let _guard = NativeValueGuard { slot, previous };
            visitor.visit_bytes(&marker)
        })
        .map_err(|_| <Error as de::Error>::custom("native JSON value transfer is unavailable"))?
}

pub(crate) struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a valid json")
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> StdResult<Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_unit<E: de::Error>(self) -> StdResult<Value, E> {
        Ok(Value::new_null())
    }

    fn visit_none<E: de::Error>(self) -> StdResult<Value, E> {
        self.visit_unit()
    }

    fn visit_some<D>(self, deserializer: D) -> StdResult<Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        Value::deserialize(deserializer)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> StdResult<Value, E> {
        Ok(Value::new_bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> StdResult<Value, E> {
        Ok(Value::new_i64(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> StdResult<Value, E> {
        Ok(Value::new_u64(value))
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> StdResult<Value, E> {
        if let Ok(value) = i64::try_from(value) {
            return self.visit_i64(value);
        }
        if let Ok(value) = u64::try_from(value) {
            return self.visit_u64(value);
        }
        #[cfg(feature = "arbitrary_precision")]
        return Ok(Value::new_rawnum_faststr(&value.to_string().into()));
        #[cfg(not(feature = "arbitrary_precision"))]
        Err(E::custom("integer out of range"))
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> StdResult<Value, E> {
        if let Ok(value) = u64::try_from(value) {
            return self.visit_u64(value);
        }
        #[cfg(feature = "arbitrary_precision")]
        return Ok(Value::new_rawnum_faststr(&value.to_string().into()));
        #[cfg(not(feature = "arbitrary_precision"))]
        Err(E::custom("integer out of range"))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> StdResult<Value, E> {
        Value::new_f64(value).ok_or_else(|| E::invalid_value(Unexpected::Float(value), &self))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> StdResult<Value, E> {
        Ok(Value::copy_str(value))
    }

    fn visit_string<E: de::Error>(self, value: String) -> StdResult<Value, E> {
        Ok(Value::new_faststr(value.into()))
    }

    fn visit_seq<A>(self, mut sequence: A) -> StdResult<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        // Size hints are untrusted; grow normally beyond this initial reservation.
        let mut value = Value::new_array_with(sequence.size_hint().unwrap_or_default().min(4096));
        while let Some(element) = sequence.next_element::<Value>()? {
            value.append_value(element);
        }
        Ok(value)
    }

    fn visit_map<A>(self, mut map: A) -> StdResult<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut value = Value::new_object_with(map.size_hint().unwrap_or_default().min(4096));
        while let Some((key, element)) = map.next_entry::<String, Value>()? {
            value.insert(&key, element);
        }
        Ok(value)
    }

    #[inline]
    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> StdResult<Value, E> {
        // Only the actual borrowed marker from the active native call can take
        // its owned value. Contents (including a copied marker) confer no access.
        NATIVE_VALUE
            .try_with(|slot| match slot.take() {
                Some(native) if bytes.len() == 1 && bytes.as_ptr() == native.marker => {
                    Ok(native.value)
                }
                previous => {
                    slot.set(previous);
                    Err(E::invalid_type(Unexpected::Bytes(bytes), &self))
                }
            })
            .map_err(|_| E::custom("native JSON value transfer is unavailable"))?
    }
}

struct SeqRefDeserializer<'de> {
    iter: slice::Iter<'de, Value>,
}

impl<'de> SeqRefDeserializer<'de> {
    fn new(slice: &'de [Value]) -> Self {
        SeqRefDeserializer { iter: slice.iter() }
    }
}

impl<'de> SeqAccess<'de> for SeqRefDeserializer<'de> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some(value) => seed.deserialize(value).map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        match self.iter.size_hint() {
            (lower, Some(upper)) if lower == upper => Some(upper),
            _ => None,
        }
    }
}

struct MapRefDeserializer<'de> {
    iter: <&'de Object as IntoIterator>::IntoIter,
    value: Option<&'de Value>,
}

impl<'de> MapRefDeserializer<'de> {
    fn new(map: &'de Object) -> Self {
        MapRefDeserializer {
            iter: map.into_iter(),
            value: None,
        }
    }
}

impl<'de> MapAccess<'de> for MapRefDeserializer<'de> {
    type Error = Error;

    fn next_key_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((key, value)) => {
                self.value = Some(value);
                let key_de = MapKeyDeserializer { key };
                seed.deserialize(key_de).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<T>(&mut self, seed: T) -> Result<T::Value, Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.value.take() {
            Some(value) => seed.deserialize(value),
            None => Err(serde::de::Error::custom("value is missing")),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        match self.iter.size_hint() {
            (lower, Some(upper)) if lower == upper => Some(upper),
            _ => None,
        }
    }
}

struct MapKeyDeserializer<'de> {
    key: &'de str,
}

macro_rules! deserialize_numeric_key {
    ($method:ident) => {
        deserialize_numeric_key!($method, deserialize_number);
    };

    ($method:ident, $using:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Error>
        where
            V: Visitor<'de>,
        {
            let mut de =
                crate::Deserializer::new(crate::reader::Read::new(self.key.as_bytes(), false));
            match de.parser.read.peek() {
                Some(b'0'..=b'9' | b'-') => {}
                _ => return Err(Error::syntax(ErrorCode::ExpectedNumericKey, b"", 0)),
            }
            let number = tri!(de.$using(visitor));
            Ok(number)
        }
    };
}

impl<'de> serde::Deserializer<'de> for MapKeyDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_borrowed_str(self.key)
    }

    deserialize_numeric_key!(deserialize_i8);
    deserialize_numeric_key!(deserialize_i16);
    deserialize_numeric_key!(deserialize_i32);
    deserialize_numeric_key!(deserialize_i64);
    deserialize_numeric_key!(deserialize_u8);
    deserialize_numeric_key!(deserialize_u16);
    deserialize_numeric_key!(deserialize_u32);
    deserialize_numeric_key!(deserialize_u64);
    // TODO: impl parsing f32
    deserialize_numeric_key!(deserialize_f32);
    deserialize_numeric_key!(deserialize_f64);
    deserialize_numeric_key!(deserialize_i128, deserialize_i128);
    deserialize_numeric_key!(deserialize_u128, deserialize_u128);

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        if self.key == "true" {
            visitor.visit_bool(true)
        } else if self.key == "false" {
            visitor.visit_bool(false)
        } else {
            Err(serde::de::Error::invalid_type(
                Unexpected::Str(self.key),
                &visitor,
            ))
        }
    }

    #[inline]
    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        // Map keys cannot be null.
        visitor.visit_some(self)
    }

    #[inline]
    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.key
            .into_deserializer()
            .deserialize_enum(name, variants, visitor)
    }

    forward_to_deserialize_any! {
        char str string bytes byte_buf unit unit_struct seq tuple tuple_struct
        map struct identifier ignored_any
    }
}

fn visit_array_ref<'de, V>(array: &'de [Value], visitor: V) -> Result<V::Value, Error>
where
    V: Visitor<'de>,
{
    let len = array.len();
    let mut deserializer = SeqRefDeserializer::new(array);
    let seq = tri!(visitor.visit_seq(&mut deserializer));
    let remaining = deserializer.iter.len();
    if remaining == 0 {
        Ok(seq)
    } else {
        Err(serde::de::Error::invalid_length(
            len,
            &"fewer elements in array",
        ))
    }
}

fn visit_object_ref<'de, V>(object: &'de Object, visitor: V) -> Result<V::Value, Error>
where
    V: Visitor<'de>,
{
    let len = object.len();
    let mut deserializer = MapRefDeserializer::new(object);
    let map = tri!(visitor.visit_map(&mut deserializer));
    let remaining = deserializer.iter.len();
    if remaining == 0 {
        Ok(map)
    } else {
        Err(serde::de::Error::invalid_length(
            len,
            &"fewer elements in map",
        ))
    }
}

macro_rules! deserialize_number {
    ($method:ident) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Error>
        where
            V: Visitor<'de>,
        {
            match self.as_ref() {
                ValueRef::Number(n) => n.deserialize_any(visitor),
                _ => Err(self.invalid_type(&visitor)),
            }
        }
    };
}

impl Value {
    #[cold]
    fn invalid_type<E>(&self, exp: &dyn Expected) -> E
    where
        E: serde::de::Error,
    {
        serde::de::Error::invalid_type(self.unexpected(), exp)
    }

    #[cold]
    fn unexpected(&self) -> Unexpected<'_> {
        self.as_ref().unexpected()
    }
}

impl<'a> ValueRef<'a> {
    #[cold]
    fn unexpected(&self) -> Unexpected<'a> {
        match self {
            ValueRef::Null => Unexpected::Unit,
            ValueRef::Bool(b) => Unexpected::Bool(*b),
            ValueRef::Number(n) => match n.n {
                N::PosInt(u) => Unexpected::Unsigned(u),
                N::NegInt(i) => Unexpected::Signed(i),
                N::Float(f) => Unexpected::Float(f),
            },
            ValueRef::String(s) => Unexpected::Str(s),
            ValueRef::Array(_) => Unexpected::Seq,
            ValueRef::Object(_) => Unexpected::Map,
        }
    }
}

struct EnumRefDeserializer<'de> {
    variant: &'de str,
    value: Option<&'de Value>,
}

impl<'de> EnumAccess<'de> for EnumRefDeserializer<'de> {
    type Error = Error;
    type Variant = VariantRefDeserializer<'de>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Error>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = self.variant.into_deserializer();
        let visitor = VariantRefDeserializer { value: self.value };
        seed.deserialize(variant).map(|v| (v, visitor))
    }
}

struct VariantRefDeserializer<'de> {
    value: Option<&'de Value>,
}

impl<'de> VariantAccess<'de> for VariantRefDeserializer<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        match self.value {
            Some(value) => Deserialize::deserialize(value),
            None => Ok(()),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.value {
            Some(value) => seed.deserialize(value),
            None => Err(serde::de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"newtype variant",
            )),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.value.map(|v| v.as_ref()) {
            Some(ValueRef::Array(v)) => {
                if v.is_empty() {
                    visitor.visit_unit()
                } else {
                    visit_array_ref(v, visitor)
                }
            }
            Some(other) => Err(serde::de::Error::invalid_type(
                other.unexpected(),
                &"tuple variant",
            )),
            None => Err(serde::de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"tuple variant",
            )),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.value.map(|v| v.as_ref()) {
            Some(ValueRef::Object(v)) => visit_object_ref(v, visitor),
            // a struct variant may be written as a sequence, as when deserializing from text
            Some(ValueRef::Array(v)) => visit_array_ref(v, visitor),
            Some(other) => Err(serde::de::Error::invalid_type(
                other.unexpected(),
                &"struct variant",
            )),
            None => Err(serde::de::Error::invalid_type(
                Unexpected::UnitVariant,
                &"struct variant",
            )),
        }
    }
}

impl<'de> IntoDeserializer<'de, Error> for &'de Value {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self::Deserializer {
        self
    }
}

impl<'de> serde::Deserializer<'de> for &'de Value {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Null => visitor.visit_unit(),
            ValueRef::Bool(v) => visitor.visit_bool(v),
            ValueRef::Number(n) => n.deserialize_any(visitor),
            ValueRef::String(v) => visitor.visit_borrowed_str(v),
            ValueRef::Array(v) => visit_array_ref(v, visitor),
            ValueRef::Object(v) => visit_object_ref(v, visitor),
        }
    }

    deserialize_number!(deserialize_i8);
    deserialize_number!(deserialize_i16);
    deserialize_number!(deserialize_i32);
    deserialize_number!(deserialize_i64);
    deserialize_number!(deserialize_i128);
    deserialize_number!(deserialize_u8);
    deserialize_number!(deserialize_u16);
    deserialize_number!(deserialize_u32);
    deserialize_number!(deserialize_u64);
    deserialize_number!(deserialize_u128);
    deserialize_number!(deserialize_f32);
    deserialize_number!(deserialize_f64);

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        let (variant, value) = match self.as_ref() {
            ValueRef::Object(value) => {
                let mut iter = value.into_iter();
                let (variant, value) = match iter.next() {
                    Some(v) => v,
                    None => {
                        return Err(serde::de::Error::invalid_value(
                            Unexpected::Map,
                            &"map with a single key",
                        ));
                    }
                };
                // enums are encoded in json as maps with a single key:value pair
                if iter.next().is_some() {
                    return Err(serde::de::Error::invalid_value(
                        Unexpected::Map,
                        &"map with a single key",
                    ));
                }
                (variant, Some(value))
            }
            ValueRef::String(variant) => (variant, None),
            other => {
                return Err(serde::de::Error::invalid_type(
                    other.unexpected(),
                    &"string or map",
                ));
            }
        };

        visitor.visit_enum(EnumRefDeserializer { variant, value })
    }

    #[inline]
    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        if name == TOKEN {
            return visit_native_value(self.clone(), visitor);
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Bool(v) => visitor.visit_bool(v),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::String(v) => visitor.visit_borrowed_str(v),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::String(v) => visitor.visit_borrowed_str(v),
            ValueRef::Array(v) => visit_array_ref(v, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Null => visitor.visit_unit(),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_unit_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Array(v) => visit_array_ref(v, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Object(v) => visit_object_ref(v, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        match self.as_ref() {
            ValueRef::Array(v) => visit_array_ref(v, visitor),
            ValueRef::Object(v) => visit_object_ref(v, visitor),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }
}

#[cfg(test)]
mod test {
    #[cfg(not(target_family = "wasm"))]
    use std::{
        cell::RefCell,
        sync::{
            atomic::{AtomicBool, Ordering},
            Barrier,
        },
        thread,
    };
    use std::{
        fmt,
        mem::size_of,
        panic::{catch_unwind, AssertUnwindSafe},
        sync::Arc,
    };

    use serde::{
        de::{
            self,
            value::{
                BoolDeserializer, BorrowedBytesDeserializer, BytesDeserializer,
                Error as ValueError, F64Deserializer, I128Deserializer, MapDeserializer,
                SeqDeserializer, StrDeserializer, U128Deserializer,
            },
            DeserializeSeed, MapAccess, SeqAccess, Visitor,
        },
        Deserialize,
    };

    use super::{visit_native_value, Value, ValueVisitor};
    use crate::{from_str, from_value, json, to_string, Array, JsonValueTrait, Object};

    #[test]
    fn test_value_as_deserializer() {
        let original = json!({"items": [null, true, -1, 1.25, "温度"], "empty": {}});
        let actual: Value = from_value(&original).unwrap();
        assert_eq!(actual, original);
        drop(original);
        assert_eq!(actual["items"][4].as_str(), Some("温度"));
    }

    #[test]
    fn test_value_from_foreign_json() {
        for expected in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(i64::MIN),
            serde_json::json!(u64::MAX),
            serde_json::json!(1.25),
            serde_json::json!("温度\n café"),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"nested": [{"value": [null, false, 42]}]}),
        ] {
            let actual: Value = serde_json::from_value(expected.clone()).unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), expected);
        }
    }

    #[test]
    fn test_direct_value_deserializers() {
        let boolean = Value::deserialize(BoolDeserializer::<ValueError>::new(true)).unwrap();
        assert_eq!(boolean.as_bool(), Some(true));
        let string = Value::deserialize(StrDeserializer::<ValueError>::new("borrowed")).unwrap();
        assert_eq!(string.as_str(), Some("borrowed"));
        let array = Value::deserialize(SeqDeserializer::<_, ValueError>::new(
            [true, false].into_iter(),
        ))
        .unwrap();
        assert_eq!(array, json!([true, false]));
    }

    #[test]
    fn test_generic_array_and_object() {
        let array: Array = serde_json::from_value(serde_json::json!([1, {"a": true}])).unwrap();
        assert_eq!(
            serde_json::to_value(array).unwrap(),
            serde_json::json!([1, {"a": true}])
        );
        let object: Object = serde_json::from_value(serde_json::json!({"a": [1, 2]})).unwrap();
        assert_eq!(
            serde_json::to_value(object).unwrap(),
            serde_json::json!({"a": [1, 2]})
        );
        assert!(serde_json::from_value::<Array>(serde_json::json!({})).is_err());
        assert!(serde_json::from_value::<Object>(serde_json::json!([])).is_err());
    }

    #[test]
    fn test_generic_nested_value_field() {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Value,
            optional: Option<Value>,
        }

        let value: Envelope =
            serde_json::from_str(r#"{"payload":{"items":[1,null,true]},"optional":"owned"}"#)
                .unwrap();
        assert_eq!(value.payload, json!({"items": [1, null, true]}));
        assert_eq!(value.optional.unwrap().as_str(), Some("owned"));
        let native: Envelope = from_str(r#"{"payload":{"a":[1,2]},"optional":null}"#).unwrap();
        assert_eq!(native.payload, json!({"a": [1, 2]}));
        assert!(native.optional.is_none());
    }

    #[test]
    fn test_direct_bytes_return_errors_without_panicking() {
        for bytes in [
            &[][..],
            &[0_u8][..],
            &[0_u8; size_of::<Value>()][..],
            &[0xff_u8; size_of::<Value>()][..],
            &[0_u8; 17][..],
        ] {
            let result =
                catch_unwind(|| Value::deserialize(BytesDeserializer::<ValueError>::new(bytes)));
            assert!(result.is_ok(), "ordinary bytes must not panic");
            assert!(result.unwrap().is_err());
            assert!(
                Value::deserialize(BorrowedBytesDeserializer::<ValueError>::new(bytes)).is_err()
            );
        }
    }

    #[test]
    fn test_generic_numeric_boundaries() {
        let minimum =
            Value::deserialize(I128Deserializer::<ValueError>::new(i64::MIN as i128)).unwrap();
        assert_eq!(minimum.as_i64(), Some(i64::MIN));
        let maximum =
            Value::deserialize(U128Deserializer::<ValueError>::new(u64::MAX as u128)).unwrap();
        assert_eq!(maximum.as_u64(), Some(u64::MAX));
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Value::deserialize(F64Deserializer::<ValueError>::new(number)).is_err());
        }
    }

    #[test]
    fn test_nested_bytes_and_non_string_keys_are_rejected() {
        let bytes = [0xff_u8; size_of::<Value>()];
        let sequence = SeqDeserializer::<_, ValueError>::new(
            [BytesDeserializer::<ValueError>::new(&bytes)].into_iter(),
        );
        assert!(Value::deserialize(sequence).is_err());
        let map = MapDeserializer::<_, ValueError>::new(
            [("payload", BytesDeserializer::<ValueError>::new(&bytes))].into_iter(),
        );
        assert!(Value::deserialize(map).is_err());
        let map = MapDeserializer::<_, ValueError>::new([(true, 1_u64)].into_iter());
        assert!(Value::deserialize(map).is_err());
    }

    #[test]
    fn test_generic_128_bit_range() {
        for number in [i128::MIN, i128::MAX] {
            let result = Value::deserialize(I128Deserializer::<ValueError>::new(number));
            if cfg!(feature = "arbitrary_precision") {
                assert_eq!(to_string(&result.unwrap()).unwrap(), number.to_string());
            } else {
                assert!(result.is_err());
            }
        }
        let result = Value::deserialize(U128Deserializer::<ValueError>::new(u128::MAX));
        if cfg!(feature = "arbitrary_precision") {
            assert_eq!(to_string(&result.unwrap()).unwrap(), u128::MAX.to_string());
        } else {
            assert!(result.is_err());
        }
    }

    #[test]
    #[cfg(feature = "arbitrary_precision")]
    fn test_native_value_preserves_raw_numbers() {
        for json in [
            "1.2345678901234567890123456789",
            "1234567890123456789012345678901234567890",
        ] {
            let original: Value = from_str(json).unwrap();
            let actual: Value = from_value(&original).unwrap();
            assert_eq!(to_string(&actual).unwrap(), json);
        }
    }

    #[test]
    fn test_untrusted_container_size_hints_are_bounded() {
        struct Empty;

        impl<'de> SeqAccess<'de> for Empty {
            type Error = ValueError;

            fn next_element_seed<T>(&mut self, _seed: T) -> Result<Option<T::Value>, ValueError>
            where
                T: DeserializeSeed<'de>,
            {
                Ok(None)
            }

            fn size_hint(&self) -> Option<usize> {
                Some(usize::MAX)
            }
        }

        impl<'de> MapAccess<'de> for Empty {
            type Error = ValueError;

            fn next_key_seed<K>(&mut self, _seed: K) -> Result<Option<K::Value>, ValueError>
            where
                K: DeserializeSeed<'de>,
            {
                Ok(None)
            }

            fn next_value_seed<V>(&mut self, _seed: V) -> Result<V::Value, ValueError>
            where
                V: DeserializeSeed<'de>,
            {
                Err(de::Error::custom("empty map has no value"))
            }

            fn size_hint(&self) -> Option<usize> {
                Some(usize::MAX)
            }
        }

        assert_eq!(ValueVisitor.visit_seq(Empty).unwrap(), json!([]));
        assert_eq!(ValueVisitor.visit_map(Empty).unwrap(), json!({}));
    }

    struct RejectVisitor {
        panic: bool,
    }

    impl<'de> Visitor<'de> for RejectVisitor {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a rejected transfer")
        }

        fn visit_bytes<E: de::Error>(self, _bytes: &[u8]) -> Result<(), E> {
            assert!(!self.panic, "visitor panic");
            Err(E::custom("visitor rejection"))
        }
    }

    #[test]
    fn test_native_transfer_cleans_up_on_error_and_panic() {
        let owner = Arc::new(vec![json!({"owned": "value"})]);
        for panic in [false, true] {
            // Abort-only targets cannot exercise a caught unwind.
            if panic && !cfg!(panic = "unwind") {
                continue;
            }
            let result = catch_unwind(AssertUnwindSafe(|| {
                visit_native_value(Value::from(owner.clone()), RejectVisitor { panic })
            }));
            if panic {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(
                Arc::strong_count(&owner),
                1,
                "unclaimed DOM must be dropped"
            );
        }
        assert_eq!(from_str::<Value>("[1,2]").unwrap(), json!([1, 2]));
    }

    #[test]
    fn test_native_transfer_is_reentrant_and_one_shot() {
        struct Reentrant;

        impl<'de> Visitor<'de> for Reentrant {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the original DOM")
            }

            fn visit_bytes<E: de::Error>(self, marker: &[u8]) -> Result<Value, E> {
                assert_eq!(
                    from_str::<Value>("[true,null]").unwrap(),
                    json!([true, null])
                );
                let forged = marker.to_vec();
                assert!(Value::deserialize(BytesDeserializer::<ValueError>::new(&forged)).is_err());
                for panic in [false, true] {
                    if panic && !cfg!(panic = "unwind") {
                        continue;
                    }
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        visit_native_value(json!({"nested": "unclaimed"}), RejectVisitor { panic })
                    }));
                }
                let value = ValueVisitor.visit_bytes(marker)?;
                assert!(ValueVisitor.visit_bytes::<E>(marker).is_err());
                Ok(value)
            }
        }

        let expected = json!({"outer": [1, "retained"]});
        assert_eq!(
            visit_native_value(expected.clone(), Reentrant).unwrap(),
            expected
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn test_native_transfer_is_thread_local() {
        struct Concurrent(Arc<Barrier>);

        impl<'de> Visitor<'de> for Concurrent {
            type Value = Value;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("this thread's DOM")
            }

            fn visit_bytes<E: de::Error>(self, marker: &[u8]) -> Result<Value, E> {
                self.0.wait();
                ValueVisitor.visit_bytes(marker)
            }
        }

        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0_u64..2)
            .map(|number| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    let value =
                        visit_native_value(Value::from(number), Concurrent(barrier)).unwrap();
                    assert_eq!(value.as_u64(), Some(number));
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn test_value_deserializes_during_thread_local_cleanup() {
        struct OnExit(Arc<AtomicBool>);

        impl Drop for OnExit {
            fn drop(&mut self) {
                self.0.store(
                    from_value::<Value>(&Value::from(42)).is_ok(),
                    Ordering::SeqCst,
                );
            }
        }

        thread_local! {
            static ON_EXIT: RefCell<Option<OnExit>> = const { RefCell::new(None) };
        }

        let completed = Arc::new(AtomicBool::new(false));
        let observed = completed.clone();
        thread::spawn(move || {
            // Register this destructor before initializing the native handoff.
            ON_EXIT.with(|slot| *slot.borrow_mut() = Some(OnExit(observed)));
            assert_eq!(
                from_value::<Value>(&Value::from(42)).unwrap().as_i64(),
                Some(42)
            );
        })
        .join()
        .unwrap();
        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn test_struct_variant_from_value_sequence() {
        // A struct variant written as a sequence deserializes from text; from a Value it failed
        // with "invalid type: sequence, expected struct variant".
        #[derive(Debug, PartialEq, serde::Deserialize)]
        enum E {
            S { a: u8, b: String },
        }
        use crate::{from_str, from_value, Value};
        let text = r#"{"S":[5,"k"]}"#;
        let expected = E::S {
            a: 5,
            b: "k".to_owned(),
        };
        assert_eq!(from_str::<E>(text).unwrap(), expected);
        let v: Value = from_str(text).unwrap();
        assert_eq!(from_value::<E>(&v).unwrap(), expected);
        for bad in [r#"{"S":[5]}"#, r#"{"S":[5,"k",1]}"#] {
            let v: Value = from_str(bad).unwrap();
            assert!(from_value::<E>(&v).is_err(), "{bad}");
        }
    }
}
