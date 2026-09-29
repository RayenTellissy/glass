//! Headless check binary: opens the reference project, plays a short range and exports
//! a short range without a window, then exits 0. It starts as a stub and grows with
//! each phase.

const STEPS: &[&str] = &[
    "open tests/fixtures/reference.proj",
    "play 2 s headless",
    "export 1 s",
];

fn main() {
    println!("smoke: stub, no checks run yet. Once implemented it will:");
    for step in STEPS {
        println!("  - {step}");
    }
    println!("smoke: ok (stub)");
}
