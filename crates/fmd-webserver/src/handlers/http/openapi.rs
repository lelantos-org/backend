//! The OpenAPI document, assembled from the per-handler `utoipa::path`
//! annotations. Mounted by [`super::router::build`].

use crate::domain::dto::CreateSubscription;
use crate::domain::responses::{
    CommitmentChunkOut, HeadOut, MatchOut, MatchesPage, NoteOut, NullifierChunkOut,
    SubscriptionOut, TreeStateOut,
};
use crate::handlers::http as handlers;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "fmd-webserver", description = "FMD webserver API"),
    paths(
        handlers::head::get_head,
        handlers::notes::list_notes,
        handlers::matches::list_matches,
        handlers::subscriptions::create_subscription,
        handlers::subscriptions::delete_subscription,
        handlers::tree::get_tree_state,
        handlers::commitments::get_commitment_chunk,
        handlers::nullifiers::get_nullifier_chunk,
    ),
    components(schemas(HeadOut, NoteOut, MatchOut, MatchesPage, SubscriptionOut, CreateSubscription, TreeStateOut, CommitmentChunkOut, NullifierChunkOut)),
    tags(
        (name = "head", description = "Sync watermarks"),
        (name = "notes", description = "Notes"),
        (name = "matches", description = "Matches"),
        (name = "subscriptions", description = "Subscriptions"),
        (name = "tree", description = "Merkle tree state"),
        (name = "commitments", description = "Note-commitment chunk feed"),
        (name = "nullifiers", description = "Spent-nullifier chunk feed"),
    )
)]
pub struct ApiDoc;
