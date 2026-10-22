use std::process::Command;

use oxicale::auth::PasswordFile;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_oxicale")
}

#[test]
fn check_config_prints_effective_values_and_rejects_unknown_keys() {
    let dir = TempDir::new().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
[server]
bind = ["127.0.0.1:7777"]
external_url = "https://example.test"

[feeds]
rate_limit_per_minute = 7
"#,
    )
    .unwrap();
    let output = Command::new(binary())
        .args(["check-config", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("127.0.0.1:7777"));
    assert!(stdout.contains("rate_limit_per_minute = 7"));

    std::fs::write(&config, "[server]\nnot_a_real_option = true\n").unwrap();
    let output = Command::new(binary())
        .args(["check-config", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));
}

#[test]
fn user_add_and_passwd_write_argon2_file() {
    let dir = TempDir::new().unwrap();
    let users = dir.path().join("users");
    let output = Command::new(binary())
        .args(["user", "add", "alice", "--password-file"])
        .arg(&users)
        .args(["--password", "first-password"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = PasswordFile::load(&users).unwrap();
    assert!(file.verify("alice", "first-password"));

    let output = Command::new(binary())
        .args(["user", "passwd", "alice", "--password-file"])
        .arg(&users)
        .args(["--password", "second-password"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let file = PasswordFile::load(&users).unwrap();
    assert!(!file.verify("alice", "first-password"));
    assert!(file.verify("alice", "second-password"));
    let text = std::fs::read_to_string(&users).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("$argon2id$"));
}

#[test]
fn migrate_radicale_dry_run_and_rebuild_index() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("collection-root");
    let alice = source.join("alice");
    let calendar = alice.join("calendar");
    std::fs::create_dir_all(&calendar).unwrap();
    std::fs::write(
        calendar.join(".Radicale.props"),
        r#"{"tag":"VCALENDAR","D:displayname":"Imported"}"#,
    )
    .unwrap();
    std::fs::write(
        calendar.join("event.ics"),
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261022T100000Z\r\nDTEND:20261022T110000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )
    .unwrap();
    let target = dir.path().join("target");

    let output = Command::new(binary())
        .args(["migrate", "radicale", "--from"])
        .arg(&source)
        .arg("--to")
        .arg(&target)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("dry run"));
    assert!(!target.exists());

    let output = Command::new(binary())
        .args(["migrate", "radicale", "--from"])
        .arg(&source)
        .arg("--to")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Items: 1"));

    let output = Command::new(binary())
        .args(["verify-storage", "--data-dir"])
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("storage verified"));

    let output = Command::new(binary())
        .args(["rebuild-index", "--data-dir"])
        .arg(&target)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("rebuilt index"));
}

#[test]
fn unsafe_configuration_is_refused() {
    let dir = TempDir::new().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
[server]
bind = ["0.0.0.0:5232"]
[auth]
backend = "none"
"#,
    )
    .unwrap();
    let output = Command::new(binary())
        .args(["check-config", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("allow_insecure_remote") || stderr.contains("loopback"));
}
