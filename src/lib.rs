//! Backend library for `rust-transfer-gui`.
//!
//! The protocol modules ([`ftp`], [`sftp`], [`tftp`]) are completely independent
//! of the GUI and can be used (and tested) on their own. The [`worker`] module
//! runs them on background threads and reports results over channels, and
//! [`app`] contains the egui front-end.

pub mod app;
pub mod common;
pub mod config;
pub mod ftp;
pub mod sftp;
pub mod tftp;
pub mod worker;
