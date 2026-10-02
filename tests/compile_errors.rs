//! Mistakes that must be reported with a clear compiler error.

/// Without `alloc`, `embassy_spawner!` explains what is missing.
#[cfg(not(feature = "alloc"))]
#[test]
fn embassy_spawner_without_alloc() {
    trybuild::TestCases::new().compile_fail("tests/ui/embassy_spawner_without_alloc.rs");
}
