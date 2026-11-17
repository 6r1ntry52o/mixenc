//! mixenc: lets clangd and AI agents read a C/C++ repository whose files mix Shift_JIS (CP932),
//! UTF-8 and other encodings, through a UTF-8 mirror. See README.md.

#[cfg(not(windows))]
compile_error!("mixenc runs on Windows: it reads CP932 with Windows' own table");

pub mod cmdline;
pub mod config;
pub mod convert;
pub mod cp932;
pub mod db;
pub mod doctor;
pub mod fsx;
pub mod git;
pub mod index;
pub mod judge;
pub mod mirror;
pub mod paths;
pub mod sync;
pub mod text;
pub mod toolchain;
mod win;
