use std::process::Command;

#[test]
fn generated_markdown_is_current() {
    let output = Command::new(env!("CARGO_BIN_EXE_render-docs"))
        .arg("--check")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "generated documentation check failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
