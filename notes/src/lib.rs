//! iCloud Notes without a window, a part at a time: the pure rules behind
//! safe sync ([`syncmodel`]) and the sidebar tree ([`notetree`]).
//!
//! Nothing here links a toolkit. The Qt app (the C++ beside these files) is
//! still the app, and carries its own copy of all this.

pub mod notetree;
pub mod syncmodel;
mod text;
