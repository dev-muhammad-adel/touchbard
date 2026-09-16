//! Shared, testable logic for the Control Center showcase.
//!
//! The routed page modules live behind the `app_router!()` expansion (a
//! function-local module), where `#[cfg(test)]` items are never collected by
//! the test harness. The challenge model therefore lives in this small library
//! target so its rules can be tested with plain `cargo test`, while the routed
//! pages import it as a normal dependency.

pub mod challenge;
