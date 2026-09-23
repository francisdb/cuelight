//! What the tests that need a GPU adapter do when they cannot get one.

#![allow(dead_code)]

/// The variable that turns the GPU tests off.
pub const SKIP: &str = "CUELIGHT_SKIP_GPU_TESTS";

/// Called when no adapter answered. Skips only if this run asked to skip;
/// otherwise it fails.
///
/// A test that passes without doing anything is worse than one that
/// fails, because nobody looks at it again. That is how the render tests
/// spent months reporting thirteen passes on Windows CI while drawing
/// nothing, and why a missing adapter is not quietly tolerated here.
/// Skipping is a decision someone makes by setting [`SKIP`], not
/// something that happens on its own.
pub fn no_adapter(what: &str) {
    assert!(
        std::env::var_os(SKIP).is_some(),
        "no GPU adapter for {what}. These tests need one; set {SKIP}=1 to skip them on purpose."
    );
    eprintln!("{SKIP} is set, skipping {what}");
}
