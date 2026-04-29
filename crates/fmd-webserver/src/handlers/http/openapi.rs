use crate::domain::dto::{CreateSubscription, SpentRequest};
use crate::domain::responses::{
    MatchOut, MerkleProofOut, NoteOut, SpentResponse, SubscriptionOut, TreeStateOut,
};
use crate::handlers::http as handlers;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "fmd-webserver", description = "FMD webserver API"),
    paths(
        handlers::notes::list_notes,
        handlers::matches::list_matches,
        handlers::subscriptions::list_subscriptions,
        handlers::subscriptions::create_subscription,
        handlers::subscriptions::delete_subscription,
        handlers::tree::get_path,
        handlers::tree::get_tree_state,
        handlers::spent::check_spent,
    ),
    components(schemas(NoteOut, MatchOut, SubscriptionOut, CreateSubscription, MerkleProofOut, TreeStateOut, SpentRequest, SpentResponse)),
    tags(
        (name = "notes", description = "Notes"),
        (name = "matches", description = "Matches"),
        (name = "subscriptions", description = "Subscriptions"),
        (name = "tree", description = "Merkle tree paths + state"),
        (name = "spent", description = "Spent-nullifier set"),
    )
)]
pub struct ApiDoc;
