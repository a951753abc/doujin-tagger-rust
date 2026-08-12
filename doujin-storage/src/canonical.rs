#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Event,
    Circle,
    Author,
    Parody,
}

impl EntityKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Circle => "circle",
            Self::Author => "author",
            Self::Parody => "parody",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "event" => Ok(Self::Event),
            "circle" => Ok(Self::Circle),
            "author" => Ok(Self::Author),
            "parody" => Ok(Self::Parody),
            _ => Err(format!("未知 canonical entity kind：{value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalEntitySnapshot {
    pub id: i64,
    pub kind: EntityKind,
    pub canonical_name: String,
    pub is_official: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMappingEvidence {
    pub source_reference: Option<String>,
    pub reason: String,
}

impl CanonicalMappingEvidence {
    pub(crate) fn encode(&self) -> Result<String, String> {
        if self.reason.trim().is_empty() {
            return Err("canonical mapping 必須包含理由".to_owned());
        }
        if self
            .source_reference
            .as_ref()
            .is_some_and(|reference| reference.trim().is_empty())
        {
            return Err("canonical mapping 的來源參照不得為空白".to_owned());
        }
        Ok(serde_json::json!({
            "source_reference": self.source_reference,
            "reason": self.reason,
        })
        .to_string())
    }
}
