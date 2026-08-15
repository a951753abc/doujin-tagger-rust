//! Statistics and facet endpoints.

use axum::Json;
use axum::extract::{RawQuery, State};
use doujin_files::RecycleBin;
use doujin_storage::statistics::{CollectionStatistics, NamedCount};
use serde::Serialize;

use crate::error::ApiError;
use crate::params::parse_facet_query;
use crate::{HttpState, lock_interactive_application};

#[derive(Debug, Serialize)]
pub(crate) struct NamedCountResponse {
    name: String,
    count: i64,
}

impl From<NamedCount> for NamedCountResponse {
    fn from(value: NamedCount) -> Self {
        Self {
            name: value.name,
            count: value.count,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct StatisticsResponse {
    total: i64,
    tagged: i64,
    missing_metadata: i64,
    categories: Vec<NamedCountResponse>,
    top_parody: Vec<NamedCountResponse>,
    top_author: Vec<NamedCountResponse>,
    top_circle: Vec<NamedCountResponse>,
    top_event: Vec<NamedCountResponse>,
    top_tags: Vec<NamedCountResponse>,
}

impl From<CollectionStatistics> for StatisticsResponse {
    fn from(value: CollectionStatistics) -> Self {
        Self {
            total: value.total,
            tagged: value.tagged,
            missing_metadata: value.missing_metadata,
            categories: value.classifications.into_iter().map(Into::into).collect(),
            top_parody: value.top_parodies.into_iter().map(Into::into).collect(),
            top_author: value.top_authors.into_iter().map(Into::into).collect(),
            top_circle: value.top_circles.into_iter().map(Into::into).collect(),
            top_event: value.top_events.into_iter().map(Into::into).collect(),
            top_tags: value.top_tags.into_iter().map(Into::into).collect(),
        }
    }
}

pub(crate) async fn get_statistics<R>(
    State(state): State<HttpState<R>>,
) -> Result<Json<StatisticsResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let statistics = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .collection_statistics()
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(statistics.into()))
}

#[derive(Debug, Serialize)]
pub(crate) struct FacetResponse {
    items: Vec<NamedCountResponse>,
}

pub(crate) async fn get_facets<R>(
    State(state): State<HttpState<R>>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<FacetResponse>, ApiError>
where
    R: RecycleBin + Send + 'static,
{
    let (facet, search, limit) = parse_facet_query(raw_query.as_deref())?;
    let items = tokio::task::spawn_blocking(move || {
        let application = lock_interactive_application(&state.application)?;
        application
            .collection_facets(facet, &search, limit)
            .map_err(ApiError::from_application)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(FacetResponse {
        items: items.into_iter().map(Into::into).collect(),
    }))
}
