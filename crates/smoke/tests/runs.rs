use std::process::Command;

#[test]
fn smoke_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_smoke"))
        .output()
        .expect("run smoke");
    assert!(
        output.status.success(),
        "smoke exited with {}",
        output.status
    );
}
