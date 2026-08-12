use std::io::{self, Read, Write};
use std::process::ExitCode;

use doujin_parser::domain::ParseInput;
use doujin_parser::parser::parse_filename;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
enum CliInput {
    Single(ParseInput),
    Batch(Vec<ParseInput>),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("doujin-parser: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut json = String::new();
    io::stdin()
        .read_to_string(&mut json)
        .map_err(|error| format!("failed to read stdin: {error}"))?;
    let input: CliInput =
        serde_json::from_str(&json).map_err(|error| format!("invalid input JSON: {error}"))?;
    let mut output = match input {
        CliInput::Single(input) => serde_json::to_vec_pretty(&parse_filename(&input)),
        CliInput::Batch(inputs) => {
            serde_json::to_vec_pretty(&inputs.iter().map(parse_filename).collect::<Vec<_>>())
        }
    }
    .map_err(|error| format!("failed to serialize result: {error}"))?;
    output.push(b'\n');
    io::stdout()
        .lock()
        .write_all(&output)
        .map_err(|error| format!("failed to write stdout: {error}"))
}
