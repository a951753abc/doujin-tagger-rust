use std::io::Write;
use std::process::{Command, Output, Stdio};

use doujin_parser::domain::ParseResult;

fn run_cli(stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_doujin-parser"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start doujin-parser CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(stdin.as_bytes())
        .expect("write CLI stdin");
    child.wait_with_output().expect("wait for CLI")
}

#[test]
fn cli_parses_json_from_stdin_and_writes_json_to_stdout() {
    let output = run_cli(
        r#"{
          "filename": "[社團] 作品名稱 (ポケモン).zip",
          "parody_evidence": [
            {
              "raw": "ポケモン",
              "kind": "confirmed_alias",
              "canonical": "ポケットモンスター"
            }
          ]
        }"#,
    );

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result: ParseResult = serde_json::from_slice(&output.stdout).expect("parse CLI output");
    assert_eq!("作品名稱", result.title);
    assert_eq!(
        "ポケットモンスター",
        result.parody.expect("parody").canonical
    );
}

#[test]
fn cli_rejects_invalid_json() {
    let output = run_cli("not JSON");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid input JSON"));
}

#[test]
fn cli_accepts_a_batch_of_parse_inputs() {
    let output = run_cli(
        r#"[
          {"filename": "(C100) [社團] 第一冊.zip", "parody_evidence": []},
          {"filename": "(C101) [社團] 第二冊.zip", "parody_evidence": []}
        ]"#,
    );

    assert!(output.status.success());
    let results: Vec<ParseResult> =
        serde_json::from_slice(&output.stdout).expect("parse batch CLI output");
    assert_eq!(2, results.len());
    assert_eq!(Some("C100"), results[0].event.as_deref());
    assert_eq!(Some("C101"), results[1].event.as_deref());
}
