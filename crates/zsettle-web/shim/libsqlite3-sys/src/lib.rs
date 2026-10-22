//! The libsqlite3-sys interface over sqlite-wasm-rs, for the browser build.

#![allow(non_snake_case, non_camel_case_types, unused_imports)]

pub use self::error::*;
pub use self::error::{code_to_str, Error, ErrorCode};
pub use sqlite_wasm_rs::*;

use std::mem;

mod error;

#[must_use]
pub fn SQLITE_STATIC() -> sqlite3_destructor_type {
    None
}

#[must_use]
pub fn SQLITE_TRANSIENT() -> sqlite3_destructor_type {
    Some(unsafe { mem::transmute::<isize, unsafe extern "C" fn(*mut std::ffi::c_void)>(-1_isize) })
}
