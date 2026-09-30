//! The Apple Notes document codec.
//!
//! Ports icloud-md's noteDocument, noteFormat, formatReconcile,
//! decode/encodeNoteRecord, encodeFolderRecord, noteText, versionedDocument,
//! mergeableDataPool, decodeTableRecord, tableEdit, tableCellEdit,
//! tablePushEdit, embedPushEdit, noteAttachments and unknownContent.
//!
//! Offsets and lengths in this model are UTF-16 code units, as on the wire
//! (Apple's topotext and icloud-md's JS strings both count UTF-16), while
//! text is held as `String`. See `format.rs`.

pub mod decode;
pub mod deflate;
pub mod document;
pub mod embeds;
pub mod encode;
pub mod format;
pub mod proto;
pub mod reconcile;
pub mod table_edit;
pub mod tables;
pub mod text;

/// Failures icloud-md surfaces as a thrown `Error` (its message is what a
/// refusal or `CreateBuildError` reason shows, so keep TS wording).
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("{0}")]
    Protobuf(#[from] proto::ProtoError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, DocError>;
