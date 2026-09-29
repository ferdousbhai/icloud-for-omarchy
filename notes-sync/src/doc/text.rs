//! Compression and the versioned-document wrapper. Ports icloud-md
//! `src/notes/noteText.ts` and `src/notes/versionedDocument.ts`.

use std::io::Read;

use super::proto::{Message, topotext, versioned_document};
use super::{DocError, Result};

/// `VersionedDocument`: the decoded wrapper plus `version[0].data`.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionedDocument {
    pub wrapper: versioned_document::Document,
    pub data: Vec<u8>,
}

/// `decompressNoteDocument`: gunzip when the gzip magic is present, else
/// zlib-inflate.
pub fn decompress_note_document(buf: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if buf.len() >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
        flate2::read::MultiGzDecoder::new(buf).read_to_end(&mut out)?;
    } else {
        flate2::read::ZlibDecoder::new(buf).read_to_end(&mut out)?;
    }
    Ok(out)
}

/// `compressNoteDocument`: `zlib.deflateSync(raw)`, byte for byte as Node
/// (Chromium zlib) produces it - see `deflate.rs`.
pub fn compress_note_document(raw: &[u8]) -> Vec<u8> {
    super::deflate::deflate_sync(raw)
}

/// `decodeNoteString`: decompress + unwrap + parse the topotext `String`.
pub fn decode_note_string(compressed: &[u8]) -> Result<topotext::String> {
    let raw = decompress_note_document(compressed)?;
    let versioned = parse_versioned_document(&raw)?;
    Ok(topotext::String::decode(&versioned.data)?)
}

/// `decodeNoteBodyText`.
pub fn decode_note_body_text(compressed: &[u8]) -> Result<String> {
    Ok(decode_note_string(compressed)?.string.unwrap_or_default())
}

/// `parseVersionedDocument`: exactly one version carrying data, or `Err`.
pub fn parse_versioned_document(raw: &[u8]) -> Result<VersionedDocument> {
    let wrapper = versioned_document::Document::decode(raw)?;
    if wrapper.version.len() != 1 {
        return Err(DocError::Invalid(format!(
            "Versioned document has {} versions - only exactly one is understood",
            wrapper.version.len()
        )));
    }
    let Some(data) = wrapper.version[0].data.clone() else {
        return Err(DocError::Invalid(
            "Versioned document's version carries no data payload".into(),
        ));
    };
    Ok(VersionedDocument { wrapper, data })
}

/// `encodeVersionedDocument`: the wrapper with `version[0].data` replaced
/// (in place, as in TS).
pub fn encode_versioned_document(doc: &mut VersionedDocument, data: &[u8]) -> Result<Vec<u8>> {
    let Some(version) = doc.wrapper.version.first_mut() else {
        return Err(DocError::Invalid(
            "Versioned document lost its version entry - refusing to encode".into(),
        ));
    };
    version.data = Some(data.to_vec());
    doc.data = data.to_vec();
    Ok(doc.wrapper.encode()?)
}
