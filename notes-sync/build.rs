//! Generates the rust-protobuf (3.x, pure parser) bindings for Apple's Notes
//! protos under `proto/` (copied verbatim from icloud-md). proto2 field
//! presence and unknown-field preservation are why this is rust-protobuf and
//! not prost: a decode→encode round trip must reproduce the bytes exactly.

fn main() {
    let protos = [
        "proto/versioned_document.proto",
        "proto/topotext.proto",
        "proto/crdt.proto",
    ];
    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }
    protobuf_codegen::Codegen::new()
        .pure()
        .include("proto")
        .inputs(protos)
        .cargo_out_dir("proto")
        .run_from_script();
}
