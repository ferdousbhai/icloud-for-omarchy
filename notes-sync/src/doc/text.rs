//! Compression and the versioned-document wrapper. Ports icloud-md
//! `src/notes/noteText.ts` and `src/notes/versionedDocument.ts`.
//! Owner: workstream B.
#![allow(unused_variables)]

use super::Result;
use super::proto::topotext;
use super::proto::versioned_document;

/// `VersionedDocument`: the decoded wrapper plus `version[0].data`.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionedDocument {
    pub wrapper: versioned_document::Document,
    pub data: Vec<u8>,
}

/// `decompressNoteDocument`: gunzip when the gzip magic is present, else
/// zlib-inflate.
pub fn decompress_note_document(buf: &[u8]) -> Result<Vec<u8>> {
    todo!()
}

/// `compressNoteDocument`: zlib deflate at Node's default level, so the
/// bytes (and the base64 in push requests) match `zlib.deflateSync`.
pub fn compress_note_document(raw: &[u8]) -> Vec<u8> {
    todo!()
}

/// `decodeNoteString`: decompress + unwrap + parse the topotext `String`.
pub fn decode_note_string(compressed: &[u8]) -> Result<topotext::String> {
    todo!()
}

/// `decodeNoteBodyText`.
pub fn decode_note_body_text(compressed: &[u8]) -> Result<String> {
    todo!()
}

/// `parseVersionedDocument`: exactly one version carrying data, or `Err`.
pub fn parse_versioned_document(raw: &[u8]) -> Result<VersionedDocument> {
    todo!()
}

/// `encodeVersionedDocument`: the wrapper with `version[0].data` replaced.
pub fn encode_versioned_document(doc: &VersionedDocument, data: &[u8]) -> Result<Vec<u8>> {
    todo!()
}
