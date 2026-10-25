#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

mod elf;
mod ffi;
mod hook;
mod nvdec;

#[link(name = "cuda")]
unsafe extern "C" {}

#[link(name = "nvcuvid")]
unsafe extern "C" {}
