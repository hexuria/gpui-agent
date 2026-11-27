//! CLI version output.

#[test]
fn version_prints_the_cli_package_name() {
    for flag in ["--version", "-V"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gpui-agent"))
            .arg(flag)
            .output()
            .expect("run version");
        assert!(output.status.success(), "{flag}: {output:?}");
        let stdout = String::from_utf8(output.stdout).expect("utf8");
        assert!(stdout.starts_with("gpui-agent-cli "), "{flag}: {stdout:?}");
    }
}
