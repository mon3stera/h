//! The ratatui front end.
//!
//! Built alongside the iocraft one so the tree keeps compiling while it grows;
//! `main_loop` switches over once this draws everything the old one could.
//!
//! Until that switch nothing outside this module calls in, so the parts that
//! have landed so far look unused.
#![allow(dead_code)]

pub mod text;
