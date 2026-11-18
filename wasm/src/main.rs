// The crate root is upstream cli/src/main.rs, copied into OUT_DIR by build.rs
// with the edits listed there.
include!(concat!(env!("OUT_DIR"), "/src/main.rs"));
