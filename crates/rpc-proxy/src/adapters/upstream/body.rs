//! The outbound JSON-RPC body, and matching its response back to the calls.

use super::{Answer, OutboundCall};
use crate::domain::jsonrpc::{UpstreamResponse, VERSION};
use serde::Serialize;
use serde_json::Value;

/// The outbound request, and the shape its response must have.
///
/// A one-call batch is sent as a bare object rather than a single-element
/// array: it is what every client sends for a lone call, and some providers
/// meter or handle the two paths differently.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Body<'a> {
    Single(Envelope<'a>),
    Batch(Vec<Envelope<'a>>),
}

#[derive(Serialize)]
pub(super) struct Envelope<'a> {
    jsonrpc: &'static str,
    id: usize,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a Value>,
}

impl<'a> Body<'a> {
    pub(super) fn new(calls: &'a [OutboundCall<'a>]) -> Self {
        let mut envelopes: Vec<Envelope<'a>> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| Envelope {
                jsonrpc: VERSION,
                id: i,
                method: c.method,
                params: c.params,
            })
            .collect();

        if envelopes.len() == 1 {
            Body::Single(envelopes.pop().expect("length checked"))
        } else {
            Body::Batch(envelopes)
        }
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Body::Single(_) => 1,
            Body::Batch(v) => v.len(),
        }
    }

    /// Parse a response and put the answers back in request order.
    ///
    /// Reordering is required, not defensive: JSON-RPC explicitly permits a
    /// server to return batch responses in any order. Zipping by position would
    /// hand one caller another caller's answer — the worst failure this service
    /// could have, since both are valid JSON and nothing downstream would
    /// notice.
    pub(super) fn decode(&self, text: &str) -> Result<Vec<Answer>, String> {
        let n = self.len();
        let raw: Vec<UpstreamResponse> = match self {
            Body::Single(_) => vec![
                serde_json::from_str(text).map_err(|e| format!("decode single response: {e}"))?,
            ],
            Body::Batch(_) => {
                serde_json::from_str(text).map_err(|e| format!("decode batch response: {e}"))?
            }
        };
        if raw.len() != n {
            return Err(format!("expected {n} responses, got {}", raw.len()));
        }

        let mut slots: Vec<Option<Answer>> = (0..n).map(|_| None).collect();
        for r in raw {
            let idx = match self {
                // A lone call has one slot; a server that echoed a different id
                // is still answering the only question asked.
                Body::Single(_) => 0,
                Body::Batch(_) => {
                    r.id.as_u64()
                        .and_then(|i| usize::try_from(i).ok())
                        .filter(|i| *i < n)
                        .ok_or_else(|| "batch response carried an unknown id".to_string())?
                }
            };
            let answer = match (r.result, r.error) {
                // `result` wins if a server sends both, which is malformed
                // anyway; taking the error would turn a good answer into a
                // failure.
                (Some(res), _) => Answer::Result(res),
                (None, Some(err)) => Answer::Error(err),
                (None, None) => return Err("response carried neither result nor error".into()),
            };
            if slots[idx].replace(answer).is_some() {
                return Err("batch response repeated an id".into());
            }
        }

        slots
            .into_iter()
            .map(|s| s.ok_or_else(|| "batch response omitted an id".to_string()))
            .collect()
    }
}
