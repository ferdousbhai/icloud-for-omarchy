//! Apple's Notes protobuf messages (`proto/versioned_document.proto`,
//! `topotext.proto`, `crdt.proto`) with a small hand-written codec that
//! reproduces protobuf-es 2.12 (what icloud-md decodes and encodes with)
//! byte for byte - the round-trip gates depend on it.
//!
//! Why not the rust-protobuf bindings `build.rs` generates: rust-protobuf keeps
//! unknown fields in a `HashMap` keyed by field number (grouped by wire type),
//! so several unknown fields - or interleaved values of one - re-emit in a
//! different order than they arrived; it refuses to parse a message missing a
//! proto2 `required` field; and it rejects invalid UTF-8 in `string` fields.
//! protobuf-es keeps unknown fields as raw `{no, wireType, data}` records in
//! arrival order, never checks `required` on decode (only on encode), and
//! decodes strings with a non-fatal `TextDecoder` (U+FFFD substitution, a
//! leading BOM dropped). This module does exactly what protobuf-es does:
//!
//! - decode: `reader.tag()`; an unknown field number is skipped and kept
//!   verbatim; a known field is read as its declared type whatever the wire
//!   type says (only repeated scalars look at it, for packed encoding); a
//!   singular scalar keeps the last value, a singular message merges.
//! - encode: declared fields in field-number order (only the set ones; a
//!   `required` one that isn't set is an error), then the unknown fields in
//!   arrival order, each as `tag(no, wireType) + data`.
//!
//! Presence is explicit everywhere (`Option`), like proto2 in protobuf-es.

use std::fmt;

/// A protobuf-es decode/encode failure; the message is protobuf-es's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoError(pub String);

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtoError {}

pub type ProtoResult<T> = std::result::Result<T, ProtoError>;

fn err<T>(message: impl Into<String>) -> ProtoResult<T> {
    Err(ProtoError(message.into()))
}

const RECURSION_LIMIT: u32 = 100;

pub const WIRE_VARINT: u8 = 0;
pub const WIRE_BIT64: u8 = 1;
pub const WIRE_LENGTH_DELIMITED: u8 = 2;
pub const WIRE_START_GROUP: u8 = 3;
pub const WIRE_END_GROUP: u8 = 4;
pub const WIRE_BIT32: u8 = 5;

/// One field this schema doesn't declare, kept as protobuf-es keeps it:
/// `data` is the raw value bytes after the tag (a length-delimited value
/// includes its length prefix).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownField {
    pub no: u32,
    pub wire_type: u8,
    pub data: Vec<u8>,
}

// --- reader ------------------------------------------------------------------

/// protobuf-es's `BinaryReader`: reads past the end see zero bytes and then
/// fail the bounds check ("premature EOF").
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn next_byte(&mut self) -> u8 {
        let byte = self.buf.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        byte
    }

    fn assert_bounds(&self) -> ProtoResult<()> {
        if self.pos > self.buf.len() {
            return err("premature EOF");
        }
        Ok(())
    }

    /// `varint32read`: low 32 bits of a varint of up to 10 bytes.
    pub fn uint32(&mut self) -> ProtoResult<u32> {
        let mut result: u32 = 0;
        for shift in [0u32, 7, 14, 21] {
            let b = self.next_byte();
            result |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                self.assert_bounds()?;
                return Ok(result);
            }
        }
        let mut b = self.next_byte();
        result |= u32::from(b & 0x0f) << 28;
        let mut read_bytes = 5;
        while b & 0x80 != 0 && read_bytes < 10 {
            b = self.next_byte();
            read_bytes += 1;
        }
        if b & 0x80 != 0 {
            return err("invalid varint");
        }
        self.assert_bounds()?;
        Ok(result)
    }

    /// `varint64read`.
    pub fn uint64(&mut self) -> ProtoResult<u64> {
        let mut low: u32 = 0;
        for shift in [0u32, 7, 14, 21] {
            let b = self.next_byte();
            low |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                self.assert_bounds()?;
                return Ok(u64::from(low));
            }
        }
        let middle = self.next_byte();
        low |= u32::from(middle & 0x0f) << 28;
        let mut high = u32::from((middle & 0x70) >> 4);
        if middle & 0x80 == 0 {
            self.assert_bounds()?;
            return Ok((u64::from(high) << 32) | u64::from(low));
        }
        let mut shift = 3u32;
        while shift <= 31 {
            let b = self.next_byte();
            high |= u32::from(b & 0x7f).wrapping_shl(shift);
            if b & 0x80 == 0 {
                self.assert_bounds()?;
                return Ok((u64::from(high) << 32) | u64::from(low));
            }
            shift += 7;
        }
        err("invalid varint")
    }

    fn tag(&mut self) -> ProtoResult<(u32, u8)> {
        let start = self.pos;
        let tag = self.uint32()?;
        let bytes_read = self.pos - start;
        if bytes_read > 5 || (bytes_read == 5 && self.buf.get(self.pos - 1).copied().unwrap_or(0) > 0x0f) {
            return err("illegal tag: varint overflows uint32");
        }
        let field_no = tag >> 3;
        let wire_type = (tag & 7) as u8;
        if field_no == 0 || wire_type > 5 {
            return err(format!("illegal tag: field no {field_no} wire type {wire_type}"));
        }
        Ok((field_no, wire_type))
    }

    fn skip(&mut self, wire_type: u8, field_no: u32, recursion_limit: i64) -> ProtoResult<Vec<u8>> {
        let start = self.pos;
        match wire_type {
            WIRE_VARINT => while self.next_byte() & 0x80 != 0 {},
            WIRE_BIT64 => self.pos += 8,
            WIRE_BIT32 => self.pos += 4,
            WIRE_LENGTH_DELIMITED => {
                let len = self.uint32()? as usize;
                self.pos += len;
            }
            WIRE_START_GROUP => {
                if recursion_limit <= 0 {
                    return err("maximum recursion depth reached");
                }
                loop {
                    let (fn_, wt) = self.tag()?;
                    if wt == WIRE_END_GROUP {
                        if fn_ != field_no {
                            return err("invalid end group tag");
                        }
                        break;
                    }
                    self.skip(wt, fn_, recursion_limit - 1)?;
                }
            }
            other => return err(format!("cant skip wire type {other}")),
        }
        self.assert_bounds()?;
        Ok(self.buf[start..self.pos].to_vec())
    }

    fn fixed32(&mut self) -> ProtoResult<u32> {
        if self.pos + 4 > self.buf.len() {
            return err("premature EOF");
        }
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap_or([0; 4]));
        self.pos += 4;
        Ok(v)
    }

    fn fixed64(&mut self) -> ProtoResult<u64> {
        if self.pos + 8 > self.buf.len() {
            return err("premature EOF");
        }
        let v = u64::from_le_bytes(self.buf[self.pos..self.pos + 8].try_into().unwrap_or([0; 8]));
        self.pos += 8;
        Ok(v)
    }

    fn bytes(&mut self) -> ProtoResult<Vec<u8>> {
        let len = self.uint32()? as usize;
        let start = self.pos;
        self.pos += len;
        self.assert_bounds()?;
        Ok(self.buf[start..start + len].to_vec())
    }
}

/// `new TextDecoder().decode(bytes)`: invalid sequences become U+FFFD
/// (maximal-subpart substitution, which `from_utf8_lossy` also implements)
/// and a leading byte-order mark is dropped.
pub fn decode_utf8_like_text_decoder(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

// --- writer --------------------------------------------------------------------

/// protobuf-es's `BinaryWriter`, minus fork/join (nested messages are encoded
/// into their own buffer, then length-prefixed - the same bytes).
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn uint32(&mut self, mut value: u32) {
        while value > 0x7f {
            self.buf.push(((value & 0x7f) | 0x80) as u8);
            value >>= 7;
        }
        self.buf.push(value as u8);
    }

    /// `varint32write` for a signed value: negative numbers take ten bytes.
    pub fn int32(&mut self, value: i32) {
        if value >= 0 {
            self.uint32(value as u32);
        } else {
            self.uint64(value as i64 as u64);
        }
    }

    pub fn uint64(&mut self, mut value: u64) {
        while value > 0x7f {
            self.buf.push(((value & 0x7f) | 0x80) as u8);
            value >>= 7;
        }
        self.buf.push(value as u8);
    }

    pub fn tag(&mut self, field_no: u32, wire_type: u8) {
        self.uint32((field_no << 3) | u32::from(wire_type));
    }

    pub fn bytes(&mut self, value: &[u8]) {
        self.uint32(value.len() as u32);
        self.buf.extend_from_slice(value);
    }
}

// --- scalar kinds ------------------------------------------------------------------

/// One protobuf scalar type: how protobuf-es reads (ignoring the wire type)
/// and writes it.
pub trait Scalar {
    type V: Clone + fmt::Debug + PartialEq + Default;
    const WIRE_TYPE: u8;
    /// Whether a repeated field of this type may arrive packed.
    const PACKABLE: bool = true;
    fn read(r: &mut Reader) -> ProtoResult<Self::V>;
    fn write(w: &mut Writer, v: &Self::V);
}

pub struct U32;
pub struct I32;
pub struct U64;
pub struct I64;
pub struct S64;
/// `float`, kept as its bits (a signalling NaN is quieted on read, as V8's
/// float32 -> double conversion does).
pub struct F32;
/// `double`, kept as its bits.
pub struct F64;
pub struct Str;
pub struct Bytes;

impl Scalar for U32 {
    type V = u32;
    const WIRE_TYPE: u8 = WIRE_VARINT;
    fn read(r: &mut Reader) -> ProtoResult<u32> {
        r.uint32()
    }
    fn write(w: &mut Writer, v: &u32) {
        w.uint32(*v);
    }
}

impl Scalar for I32 {
    type V = i32;
    const WIRE_TYPE: u8 = WIRE_VARINT;
    fn read(r: &mut Reader) -> ProtoResult<i32> {
        Ok(r.uint32()? as i32)
    }
    fn write(w: &mut Writer, v: &i32) {
        w.int32(*v);
    }
}

impl Scalar for U64 {
    type V = u64;
    const WIRE_TYPE: u8 = WIRE_VARINT;
    fn read(r: &mut Reader) -> ProtoResult<u64> {
        r.uint64()
    }
    fn write(w: &mut Writer, v: &u64) {
        w.uint64(*v);
    }
}

impl Scalar for I64 {
    type V = i64;
    const WIRE_TYPE: u8 = WIRE_VARINT;
    fn read(r: &mut Reader) -> ProtoResult<i64> {
        Ok(r.uint64()? as i64)
    }
    fn write(w: &mut Writer, v: &i64) {
        w.uint64(*v as u64);
    }
}

impl Scalar for S64 {
    type V = i64;
    const WIRE_TYPE: u8 = WIRE_VARINT;
    fn read(r: &mut Reader) -> ProtoResult<i64> {
        let z = r.uint64()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }
    fn write(w: &mut Writer, v: &i64) {
        w.uint64(((*v << 1) ^ (*v >> 63)) as u64);
    }
}

impl Scalar for F32 {
    type V = u32;
    const WIRE_TYPE: u8 = WIRE_BIT32;
    fn read(r: &mut Reader) -> ProtoResult<u32> {
        let bits = r.fixed32()?;
        let is_nan = bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0;
        Ok(if is_nan { bits | 0x0040_0000 } else { bits })
    }
    fn write(w: &mut Writer, v: &u32) {
        w.buf.extend_from_slice(&v.to_le_bytes());
    }
}

impl Scalar for F64 {
    type V = u64;
    const WIRE_TYPE: u8 = WIRE_BIT64;
    fn read(r: &mut Reader) -> ProtoResult<u64> {
        r.fixed64()
    }
    fn write(w: &mut Writer, v: &u64) {
        w.buf.extend_from_slice(&v.to_le_bytes());
    }
}

impl Scalar for Str {
    type V = String;
    const WIRE_TYPE: u8 = WIRE_LENGTH_DELIMITED;
    const PACKABLE: bool = false;
    fn read(r: &mut Reader) -> ProtoResult<String> {
        Ok(decode_utf8_like_text_decoder(&r.bytes()?))
    }
    fn write(w: &mut Writer, v: &String) {
        w.bytes(v.as_bytes());
    }
}

impl Scalar for Bytes {
    type V = Vec<u8>;
    const WIRE_TYPE: u8 = WIRE_LENGTH_DELIMITED;
    const PACKABLE: bool = false;
    fn read(r: &mut Reader) -> ProtoResult<Vec<u8>> {
        r.bytes()
    }
    fn write(w: &mut Writer, v: &Vec<u8>) {
        w.bytes(v);
    }
}

// --- messages -------------------------------------------------------------------

/// A message type. `decode`/`encode` are protobuf-es's `fromBinary`/`toBinary`.
pub trait Message: Default + Clone + fmt::Debug + PartialEq {
    /// Fully qualified name, e.g. `topotext.String`.
    const TYPE_NAME: &'static str;

    /// Reads one declared field; `Ok(false)` when `no` isn't declared.
    fn read_field(&mut self, no: u32, wire_type: u8, r: &mut Reader, depth: u32) -> ProtoResult<bool>;
    fn write_fields(&self, w: &mut Writer) -> ProtoResult<()>;
    fn unknown_fields(&self) -> &[UnknownField];
    fn unknown_fields_mut(&mut self) -> &mut Vec<UnknownField>;

    /// `readMessage` (length-delimited).
    fn merge_from(&mut self, r: &mut Reader, len: usize, depth: u32) -> ProtoResult<()> {
        let depth = depth + 1;
        if depth > RECURSION_LIMIT {
            return err(format!(
                "cannot decode message {} from binary: maximum recursion depth of {RECURSION_LIMIT} reached",
                Self::TYPE_NAME
            ));
        }
        let end = r.pos + len;
        while r.pos < end {
            let (no, wire_type) = r.tag()?;
            if !self.read_field(no, wire_type, r, depth)? {
                let data = r.skip(wire_type, no, i64::from(RECURSION_LIMIT) - i64::from(depth))?;
                self.unknown_fields_mut().push(UnknownField { no, wire_type, data });
            }
        }
        Ok(())
    }

    fn decode(bytes: &[u8]) -> ProtoResult<Self> {
        let mut message = Self::default();
        let mut r = Reader::new(bytes);
        message.merge_from(&mut r, bytes.len(), 0)?;
        Ok(message)
    }

    fn encode(&self) -> ProtoResult<Vec<u8>> {
        let mut w = Writer::default();
        self.write_fields(&mut w)?;
        Ok(w.buf)
    }
}

fn write_unknown(w: &mut Writer, unknown: &[UnknownField]) {
    for field in unknown {
        w.tag(field.no, field.wire_type);
        w.buf.extend_from_slice(&field.data);
    }
}

fn required_error(name: &str) -> ProtoError {
    ProtoError(format!("cannot encode field {name} to binary: required field not set"))
}

/// Field-kind helpers the `message!` macro dispatches to (`opt`, `req`, `rep`
/// for scalars; `optm`, `reqm`, `repm` for messages).
#[allow(non_camel_case_types)]
pub mod kind {
    use super::*;

    pub mod opt {
        use super::*;
        pub type Ty<S> = Option<<S as Scalar>::V>;
        pub fn read<S: Scalar>(slot: &mut Ty<S>, r: &mut Reader, _wt: u8, _depth: u32) -> ProtoResult<()> {
            *slot = Some(S::read(r)?);
            Ok(())
        }
        pub fn write<S: Scalar>(slot: &Ty<S>, no: u32, w: &mut Writer, _name: &str) -> ProtoResult<()> {
            if let Some(v) = slot {
                w.tag(no, S::WIRE_TYPE);
                S::write(w, v);
            }
            Ok(())
        }
    }

    pub mod req {
        use super::*;
        pub type Ty<S> = Option<<S as Scalar>::V>;
        pub fn read<S: Scalar>(slot: &mut Ty<S>, r: &mut Reader, wt: u8, depth: u32) -> ProtoResult<()> {
            opt::read::<S>(slot, r, wt, depth)
        }
        pub fn write<S: Scalar>(slot: &Ty<S>, no: u32, w: &mut Writer, name: &str) -> ProtoResult<()> {
            if slot.is_none() {
                return Err(required_error(name));
            }
            opt::write::<S>(slot, no, w, name)
        }
    }

    pub mod rep {
        use super::*;
        pub type Ty<S> = Vec<<S as Scalar>::V>;
        pub fn read<S: Scalar>(slot: &mut Ty<S>, r: &mut Reader, wt: u8, _depth: u32) -> ProtoResult<()> {
            if wt == WIRE_LENGTH_DELIMITED && S::PACKABLE {
                let len = r.uint32()? as usize;
                let end = r.pos + len;
                while r.pos < end {
                    slot.push(S::read(r)?);
                }
            } else {
                slot.push(S::read(r)?);
            }
            Ok(())
        }
        pub fn write<S: Scalar>(slot: &Ty<S>, no: u32, w: &mut Writer, _name: &str) -> ProtoResult<()> {
            for v in slot {
                w.tag(no, S::WIRE_TYPE);
                S::write(w, v);
            }
            Ok(())
        }
    }

    fn read_message<M: Message>(message: &mut M, r: &mut Reader, depth: u32) -> ProtoResult<()> {
        let len = r.uint32()? as usize;
        message.merge_from(r, len, depth)
    }

    fn write_message<M: Message>(message: &M, no: u32, w: &mut Writer) -> ProtoResult<()> {
        let mut inner = Writer::default();
        message.write_fields(&mut inner)?;
        w.tag(no, WIRE_LENGTH_DELIMITED);
        w.bytes(&inner.buf);
        Ok(())
    }

    pub mod optm {
        use super::*;
        pub type Ty<M> = Option<M>;
        pub fn read<M: Message>(slot: &mut Ty<M>, r: &mut Reader, _wt: u8, depth: u32) -> ProtoResult<()> {
            read_message(slot.get_or_insert_with(M::default), r, depth)
        }
        pub fn write<M: Message>(slot: &Ty<M>, no: u32, w: &mut Writer, _name: &str) -> ProtoResult<()> {
            if let Some(m) = slot {
                write_message(m, no, w)?;
            }
            Ok(())
        }
    }

    pub mod reqm {
        use super::*;
        pub type Ty<M> = Option<M>;
        pub fn read<M: Message>(slot: &mut Ty<M>, r: &mut Reader, wt: u8, depth: u32) -> ProtoResult<()> {
            optm::read(slot, r, wt, depth)
        }
        pub fn write<M: Message>(slot: &Ty<M>, no: u32, w: &mut Writer, name: &str) -> ProtoResult<()> {
            if slot.is_none() {
                return Err(required_error(name));
            }
            optm::write(slot, no, w, name)
        }
    }

    pub mod repm {
        use super::*;
        pub type Ty<M> = Vec<M>;
        pub fn read<M: Message>(slot: &mut Ty<M>, r: &mut Reader, _wt: u8, depth: u32) -> ProtoResult<()> {
            let mut m = M::default();
            read_message(&mut m, r, depth)?;
            slot.push(m);
            Ok(())
        }
        pub fn write<M: Message>(slot: &Ty<M>, no: u32, w: &mut Writer, _name: &str) -> ProtoResult<()> {
            for m in slot {
                write_message(m, no, w)?;
            }
            Ok(())
        }
    }
}

/// Declares one message. Fields must be listed in field-number order (that
/// is the encode order).
macro_rules! message {
    (
        $(#[$meta:meta])*
        pub struct $name:ident = $full:literal {
            $( $(#[$fmeta:meta])* $no:literal $field:ident $pname:literal : $kind:ident $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: $crate::doc::proto::kind::$kind::Ty<$ty>, )*
            /// Undeclared fields, in arrival order.
            pub unknown_fields: Vec<$crate::doc::proto::UnknownField>,
        }

        impl $crate::doc::proto::Message for $name {
            const TYPE_NAME: &'static str = $full;

            #[allow(unused_variables)]
            fn read_field(
                &mut self,
                no: u32,
                wire_type: u8,
                r: &mut $crate::doc::proto::Reader,
                depth: u32,
            ) -> $crate::doc::proto::ProtoResult<bool> {
                match no {
                    $( $no => {
                        $crate::doc::proto::kind::$kind::read::<$ty>(&mut self.$field, r, wire_type, depth)?;
                        Ok(true)
                    } )*
                    _ => Ok(false),
                }
            }

            fn write_fields(&self, w: &mut $crate::doc::proto::Writer) -> $crate::doc::proto::ProtoResult<()> {
                $( $crate::doc::proto::kind::$kind::write::<$ty>(&self.$field, $no, w, concat!($full, ".", $pname))?; )*
                $crate::doc::proto::write_unknown_fields(w, &self.unknown_fields);
                Ok(())
            }

            fn unknown_fields(&self) -> &[$crate::doc::proto::UnknownField] {
                &self.unknown_fields
            }

            fn unknown_fields_mut(&mut self) -> &mut Vec<$crate::doc::proto::UnknownField> {
                &mut self.unknown_fields
            }
        }
    };
}

#[doc(hidden)]
pub fn write_unknown_fields(w: &mut Writer, unknown: &[UnknownField]) {
    write_unknown(w, unknown);
}

/// `proto/versioned_document.proto`.
pub mod versioned_document {
    use super::{Bytes, U32};

    message! {
        /// The outermost wrapper of every mergeable payload.
        pub struct Document = "versioned_document.Document" {
            1 serialization_version "serializationVersion": opt U32,
            2 version "version": repm Version,
        }
    }

    message! {
        pub struct Version = "versioned_document.Version" {
            1 serialization_version "serializationVersion": opt U32,
            2 minimum_supported_version "minimumSupportedVersion": opt U32,
            3 data "data": opt Bytes,
        }
    }
}

/// `proto/topotext.proto`.
pub mod topotext {
    use super::{Bytes, F32, I32, Str, U32, U64};

    message! {
        /// A note body / cell text / ordering mirror.
        pub struct String = "topotext.String" {
            2 string "string": req Str,
            3 substring "substring": repm Substring,
            4 timestamp "timestamp": optm VectorTimestamp,
            5 attribute_run "attributeRun": repm AttributeRun,
        }
    }

    message! {
        pub struct CharID = "topotext.CharID" {
            1 replica_id "replicaID": req U32,
            2 clock "clock": req U32,
        }
    }

    message! {
        pub struct Substring = "topotext.Substring" {
            1 char_id "charID": reqm CharID,
            2 length "length": req U32,
            3 timestamp "timestamp": reqm CharID,
            4 tombstone "tombstone": opt U32,
            5 child "child": rep U32,
        }
    }

    message! {
        pub struct VectorTimestamp = "topotext.VectorTimestamp" {
            1 clock "clock": repm vector_timestamp::Clock,
        }
    }

    pub mod vector_timestamp {
        use super::super::Bytes;

        message! {
            pub struct Clock = "topotext.VectorTimestamp.Clock" {
                1 replica_uuid "replicaUUID": req Bytes,
                2 replica_clock "replicaClock": repm clock::ReplicaClock,
            }
        }

        pub mod clock {
            use super::super::super::U32;

            message! {
                pub struct ReplicaClock = "topotext.VectorTimestamp.Clock.ReplicaClock" {
                    1 clock "clock": req U32,
                    2 subclock "subclock": opt U32,
                }
            }
        }
    }

    message! {
        pub struct Color = "topotext.Color" {
            1 red "red": req F32,
            2 green "green": req F32,
            3 blue "blue": req F32,
            4 alpha "alpha": req F32,
        }
    }

    message! {
        pub struct AttachmentInfo = "topotext.AttachmentInfo" {
            1 attachment_identifier "attachmentIdentifier": opt Str,
            2 type_uti "typeUTI": opt Str,
            3 system_attachment_class_name "systemAttachmentClassName": opt Str,
            4 system_attachment_data "systemAttachmentData": opt Bytes,
        }
    }

    message! {
        pub struct Font = "topotext.Font" {
            1 name "name": opt Str,
            2 point_size "pointSize": opt F32,
            3 font_hints "fontHints": opt U32,
        }
    }

    message! {
        pub struct Todo = "topotext.Todo" {
            1 todo_uuid "todoUUID": req Bytes,
            2 done "done": req U32,
        }
    }

    message! {
        pub struct ParagraphStyle = "topotext.ParagraphStyle" {
            1 style "style": opt U32,
            2 alignment "alignment": opt U32,
            3 writing_direction "writingDirection": opt U32,
            4 indent "indent": opt I32,
            5 todo "todo": optm Todo,
            6 paragraph_hints "paragraphHints": opt U32,
            7 starting_list_item_number "startingListItemNumber": opt U32,
            8 block_quote_level "blockQuoteLevel": opt U32,
            9 uuid "uuid": opt Bytes,
        }
    }

    message! {
        pub struct AttributeRun = "topotext.AttributeRun" {
            1 length "length": req U32,
            2 paragraph_style "paragraphStyle": optm ParagraphStyle,
            3 font "font": optm Font,
            5 font_hints "fontHints": opt U32,
            6 underline "underline": opt U32,
            7 strikethrough "strikethrough": opt U32,
            8 superscript "superscript": opt I32,
            9 link "link": opt Str,
            10 color "color": optm Color,
            11 writing_direction "writingDirection": opt U32,
            12 attachment_info "attachmentInfo": optm AttachmentInfo,
            13 timestamp "timestamp": opt U64,
            14 emphasis "emphasis": opt U32,
            15 system_attachment_info "systemAttachmentInfo": optm AttachmentInfo,
        }
    }

    impl AttributeRun {
        /// `run.length` (0 when unset, like protobuf-es's zero default).
        #[allow(clippy::len_without_is_empty)]
        pub fn len(&self) -> u32 {
            self.length.unwrap_or(0)
        }

        /// A run with only `length` set - `create(AttributeRunSchema, {length})`.
        pub fn with_length(length: u32) -> AttributeRun {
            AttributeRun {
                length: Some(length),
                ..AttributeRun::default()
            }
        }
    }
}

/// `proto/crdt.proto` (package `CRDT`).
pub mod crdt {
    use super::{Bytes, F64, I64, S64, Str, U32, U64, topotext};

    message! {
        pub struct ObjectID = "CRDT.ObjectID" {
            1 integer_value "integerValue": opt S64,
            2 unsigned_integer_value "unsignedIntegerValue": opt U64,
            3 double_value "doubleValue": opt F64,
            4 string_value "stringValue": opt Str,
            5 bytes_value "bytesValue": opt Bytes,
            6 object_index "objectIndex": opt U32,
        }
    }

    message! {
        pub struct Timestamp = "CRDT.Timestamp" {
            1 replica_index "replicaIndex": opt U64,
            2 counter "counter": opt I64,
        }
    }

    message! {
        pub struct RegisterLatest = "CRDT.RegisterLatest" {
            1 timestamp "timestamp": optm Timestamp,
            2 contents "contents": optm ObjectID,
        }
    }

    message! {
        pub struct VectorTimestamp = "CRDT.VectorTimestamp" {
            1 element "element": repm vector_timestamp::Element,
        }
    }

    pub mod vector_timestamp {
        use super::super::U64;

        message! {
            pub struct Element = "CRDT.VectorTimestamp.Element" {
                1 replica_index "replicaIndex": opt U64,
                2 clock "clock": opt U64,
                3 subclock "subclock": opt U64,
            }
        }
    }

    message! {
        pub struct Dictionary = "CRDT.Dictionary" {
            1 element "element": repm dictionary::Element,
        }
    }

    pub mod dictionary {
        use super::{ObjectID, RegisterLatest, VectorTimestamp};

        message! {
            pub struct Element = "CRDT.Dictionary.Element" {
                1 key "key": optm ObjectID,
                2 value "value": reqm ObjectID,
                3 timestamp "timestamp": optm VectorTimestamp,
                4 index "index": optm RegisterLatest,
            }
        }
    }

    message! {
        pub struct Index = "CRDT.Index" {
            1 element "element": repm index::Element,
        }
    }

    pub mod index {
        use super::super::{I64, U64};

        message! {
            pub struct Element = "CRDT.Index.Element" {
                1 replica_index "replicaIndex": opt U64,
                2 integer "integer": opt I64,
            }
        }
    }

    message! {
        pub struct OneOf = "CRDT.OneOf" {
            1 element "element": repm one_of::Element,
        }
    }

    pub mod one_of {
        use super::{ObjectID, Timestamp};

        message! {
            pub struct Element = "CRDT.OneOf.Element" {
                1 value "value": optm ObjectID,
                2 timestamp "timestamp": optm Timestamp,
            }
        }
    }

    message! {
        pub struct StringArray = "CRDT.StringArray" {
            1 contents "contents": reqm topotext::String,
            2 attachments "attachments": repm string_array::ArrayAttachment,
        }
    }

    pub mod string_array {
        use super::super::{Bytes, U64};

        message! {
            pub struct ArrayAttachment = "CRDT.StringArray.ArrayAttachment" {
                1 attachment_index "attachmentIndex": req U64,
                2 contents "contents": req Bytes,
            }
        }
    }

    message! {
        pub struct Array = "CRDT.Array" {
            1 array "array": reqm StringArray,
            2 dictionary "dictionary": reqm Dictionary,
        }
    }

    message! {
        pub struct OrderedSet = "CRDT.OrderedSet" {
            1 array "array": reqm Array,
            2 set "set": reqm Dictionary,
        }
    }

    message! {
        pub struct Document = "CRDT.Document" {
            1 version "version": optm VectorTimestamp,
            2 start_version "startVersion": optm VectorTimestamp,
            3 object "object": repm document::DocObject,
            4 key_item "keyItem": rep Str,
            5 type_item "typeItem": rep Str,
            6 uuid_item "uuidItem": rep Bytes,
            7 tt_timestamp "ttTimestamp": optm topotext::VectorTimestamp,
        }
    }

    pub mod document {
        use super::super::{I32, U64};
        use super::{
            Array, Dictionary, Index, ObjectID, OneOf, OrderedSet, RegisterLatest, StringArray, Timestamp,
            VectorTimestamp, topotext,
        };

        message! {
            pub struct CustomObject = "CRDT.Document.CustomObject" {
                1 type_ "type": req I32,
                3 map_entry "mapEntry": repm custom_object::MapEntry,
            }
        }

        pub mod custom_object {
            use super::super::super::I32;
            use super::ObjectID;

            message! {
                pub struct MapEntry = "CRDT.Document.CustomObject.MapEntry" {
                    1 key "key": req I32,
                    2 value "value": reqm ObjectID,
                }
            }
        }

        message! {
            pub struct DocObject = "CRDT.Document.DocObject" {
                1 register_latest "registerLatest": optm RegisterLatest,
                2 register_greatest "registerGreatest": optm RegisterLatest,
                3 register_least "registerLeast": optm RegisterLatest,
                4 set "set": optm Dictionary,
                5 ordered_set "orderedSet": optm Dictionary,
                6 dictionary "dictionary": optm Dictionary,
                7 timestamp "timestamp": optm Timestamp,
                8 vector_timestamp "vectorTimestamp": optm VectorTimestamp,
                9 index "index": optm Index,
                10 string "string": optm topotext::String,
                11 weak_reference "weakReference": opt U64,
                12 oneof "oneof": optm OneOf,
                13 custom "custom": optm CustomObject,
                14 string_array "stringArray": optm StringArray,
                15 array "array": optm Array,
                16 ts_ordered_set "tsOrderedSet": optm OrderedSet,
            }
        }
    }
}
