use std::fs;
use std::path::PathBuf;

use doujin_parser::corpus::{CorpusStatus, ParserCorpus, ReviewStatus};
use doujin_parser::domain::{NextAction, ParseStatus};
use doujin_parser::parser::{parse_creator_prefix, parse_filename, parse_prefix};

fn load_accepted_corpus() -> ParserCorpus {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("parser-corpus-v1.json");
    let json = fs::read_to_string(path).expect("read accepted parser corpus");
    ParserCorpus::from_json(&json).expect("deserialize accepted parser corpus")
}

#[test]
fn accepted_corpus_fits_the_rust_domain_model() {
    let corpus = load_accepted_corpus();

    corpus.validate().expect("validate accepted parser corpus");
    assert_eq!(CorpusStatus::Accepted, corpus.corpus_status);
    assert_eq!(31, corpus.cases.len());
    assert!(
        corpus
            .cases
            .iter()
            .all(|case| case.review_status == ReviewStatus::Accepted)
    );
}

#[test]
fn nested_author_case_preserves_the_inner_parentheses() {
    let corpus = load_accepted_corpus();
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "parser-v2-case-014")
        .expect("nested author case");

    assert_eq!(Some("macdoll"), case.expected.circle.as_deref());
    assert_eq!(
        Some("士嬢マコ(・c_・ )"),
        case.expected.authors.raw.as_deref()
    );
    assert_eq!(
        ["士嬢マコ(・c_・ )"],
        case.expected.authors.values.as_slice()
    );
}

#[test]
fn uncertain_trailing_parentheses_are_other_information() {
    let corpus = load_accepted_corpus();
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "parser-v2-case-017")
        .expect("uncertain trailing parentheses case");

    assert!(case.expected.parody.is_none());
    assert_eq!("角色名稱", case.expected.other_info[0].raw);
    assert_eq!(ParseStatus::Complete, case.expected.parse_status);
    assert_eq!(NextAction::None, case.expected.next_action);
}

#[test]
fn rj_identifier_is_structured_metadata() {
    let corpus = load_accepted_corpus();
    let case = corpus
        .cases
        .iter()
        .find(|case| case.id == "parser-v2-case-023")
        .expect("RJ identifier case");

    assert_eq!("RJ", case.expected.identifiers[0].scheme);
    assert_eq!("RJ407766", case.expected.identifiers[0].value);
}

#[test]
fn prefix_parser_matches_every_accepted_classification_and_event() {
    let corpus = load_accepted_corpus();

    for case in corpus.cases {
        let actual = parse_prefix(&case.input.filename);
        assert_eq!(
            case.expected.classification, actual.classification,
            "classification mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.event, actual.event,
            "event mismatch for {}",
            case.id
        );
    }
}

#[test]
fn creator_parser_matches_every_accepted_leading_bracket() {
    let corpus = load_accepted_corpus();

    for case in corpus.cases {
        let prefix = parse_prefix(&case.input.filename);
        let actual = parse_creator_prefix(&prefix.remaining, &prefix.classification);
        let expected_prefix_markers: Vec<_> = case
            .expected
            .ignored_segments
            .iter()
            .filter(|segment| segment.kind == "date_marker")
            .cloned()
            .collect();
        let expected_creator_other_info: Vec<_> = case
            .expected
            .other_info
            .iter()
            .filter(|item| {
                matches!(
                    item.reason.as_str(),
                    "malformed_circle_author" | "author_parenthesis_not_at_tail"
                )
            })
            .cloned()
            .collect();

        assert_eq!(
            case.expected.leading_bracket_raw, actual.leading_bracket_raw,
            "leading bracket mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.circle, actual.circle,
            "circle mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.authors, actual.authors,
            "authors mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.identifiers, actual.identifiers,
            "identifiers mismatch for {}",
            case.id
        );
        assert_eq!(
            expected_creator_other_info, actual.other_info,
            "other info mismatch for {}",
            case.id
        );
        assert_eq!(
            expected_prefix_markers, actual.ignored_segments,
            "prefix markers mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.parse_status, actual.parse_status,
            "parse status mismatch for {}",
            case.id
        );
        assert_eq!(
            case.expected.next_action, actual.next_action,
            "next action mismatch for {}",
            case.id
        );
    }
}

#[test]
fn complete_parser_matches_every_accepted_case() {
    let corpus = load_accepted_corpus();

    for case in corpus.cases {
        let actual = parse_filename(&case.input);
        assert_eq!(case.expected, actual, "full parse mismatch for {}", case.id);
    }
}
