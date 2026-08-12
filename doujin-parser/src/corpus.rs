use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::{ParseInput, ParseResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusStatus {
    Draft,
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaseOrigin {
    BddExample { reference: String },
    CollectionDb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserCase {
    pub id: String,
    pub review_status: ReviewStatus,
    pub origin: CaseOrigin,
    pub decisions: Vec<String>,
    pub tags: Vec<String>,
    pub input: ParseInput,
    pub expected: ParseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserCorpus {
    pub schema_version: u32,
    pub corpus_status: CorpusStatus,
    pub accepted_at: Option<String>,
    pub description: String,
    pub cases: Vec<ParserCase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusValidationError {
    UnsupportedSchema(u32),
    DuplicateCaseId(String),
    UnreviewedCaseInAcceptedCorpus { id: String, status: ReviewStatus },
}

impl fmt::Display for CorpusValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema(version) => {
                write!(
                    formatter,
                    "unsupported parser corpus schema version: {version}"
                )
            }
            Self::DuplicateCaseId(id) => write!(formatter, "duplicate parser case id: {id}"),
            Self::UnreviewedCaseInAcceptedCorpus { id, status } => write!(
                formatter,
                "accepted corpus contains non-accepted case {id}: {status:?}"
            ),
        }
    }
}

impl Error for CorpusValidationError {}

impl ParserCorpus {
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    pub fn validate(&self) -> Result<(), CorpusValidationError> {
        if self.schema_version != 1 {
            return Err(CorpusValidationError::UnsupportedSchema(
                self.schema_version,
            ));
        }

        let mut case_ids = HashSet::with_capacity(self.cases.len());
        for case in &self.cases {
            if !case_ids.insert(case.id.as_str()) {
                return Err(CorpusValidationError::DuplicateCaseId(case.id.clone()));
            }
            if self.corpus_status == CorpusStatus::Accepted
                && case.review_status != ReviewStatus::Accepted
            {
                return Err(CorpusValidationError::UnreviewedCaseInAcceptedCorpus {
                    id: case.id.clone(),
                    status: case.review_status,
                });
            }
        }

        Ok(())
    }
}
