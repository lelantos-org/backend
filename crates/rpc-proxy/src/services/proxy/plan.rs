//! Deciding how each call in a request is served, and putting the answers back
//! together.
//!
//! Nothing here touches the network: a plan says where a value will come from —
//! config, a refusal, a cache key, or the calls inside a client's multicall —
//! and [`ChainService::assemble`] turns plans and fetched values into responses
//! in request order.

use super::{ChainService, SERVER_ERROR};
use crate::domain::allowlist::{Method, Policy as Allow};
use crate::domain::cache_key::{CacheKey, key as cache_key};
use crate::domain::jsonrpc::{CachedResult, Reply, Request, Response, RpcError, VERSION};
use crate::domain::multicall;
use crate::domain::policy::{Class, Policy, is_revert, policy};
use serde_json::Value;
use std::collections::HashMap;

impl ChainService {
    /// Decide how one request is served, without touching the network.
    pub(super) fn plan(&self, req: &Request) -> Plan {
        let Some(method) = Method::parse(&req.method) else {
            self.count_unknown_method();
            return Plan::Refused(RpcError::method_not_found(&req.method));
        };
        if method == Method::EthCall
            && let Some(requested) = multicall::requested(req.params())
        {
            return self.plan_multicall(req, requested);
        }

        let allow = self.allowlist();
        if let Err(e) = allow.validate(method, req) {
            self.count(method, "rejected_params");
            self.count_target_rejection(method, req);
            return Plan::Refused(e);
        }

        let params = req.params();
        let policy = policy(method, params, self.tip.get(), self.limits.reorg_depth);
        if policy == Policy::Local {
            self.count(method, "local");
            return Plan::Local(self.chain_id_result.clone());
        }
        Plan::Fetch {
            method,
            key: cache_key(self.chain_id, method, params),
            class: policy.class(),
        }
    }

    /// Plan a client's `aggregate3` as the calls inside it.
    ///
    /// The multicall itself is held to everything an `eth_call` is except its
    /// target; each inner call is then validated, keyed and classified exactly
    /// as if it had arrived on its own. One refused inner call refuses the
    /// whole multicall, before anything is fetched, and names which.
    fn plan_multicall(
        &self,
        req: &Request,
        requested: Result<Vec<multicall::Requested>, RpcError>,
    ) -> Plan {
        let allow = self.allowlist();
        let planned = allow
            .validate_call_envelope(req.params())
            .and(requested)
            .and_then(|calls| {
                calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, call)| self.plan_inner(&allow, i, call))
                    .collect::<Result<Vec<_>, _>>()
            });
        match planned {
            Ok(calls) => Plan::Multicall(calls),
            Err(e) => {
                self.count(Method::EthCall, "rejected_params");
                Plan::Refused(e)
            }
        }
    }

    /// One call inside a multicall, planned as a standalone `eth_call`.
    fn plan_inner(
        &self,
        allow: &Allow<'_>,
        index: usize,
        call: multicall::Requested,
    ) -> Result<Inner, RpcError> {
        let req = Request {
            jsonrpc: VERSION.into(),
            method: Method::EthCall.label().into(),
            params: Some(call.params),
            id: None,
        };
        if let Err(e) = allow.validate(Method::EthCall, &req) {
            // The same WARN a lone call gets: a token missing from the
            // allowlist is as broken inside a multicall as outside one.
            self.count_target_rejection(Method::EthCall, &req);
            return Err(RpcError::invalid_params(format!(
                "aggregate3 call {index}: {}",
                e.message
            )));
        }

        let params = req.params();
        Ok(Inner {
            key: cache_key(self.chain_id, Method::EthCall, params),
            class: policy(
                Method::EthCall,
                params,
                self.tip.get(),
                self.limits.reorg_depth,
            )
            .class(),
            allow_failure: call.allow_failure,
            params: req.params.unwrap_or_default(),
        })
    }

    /// Turn plans and fetched values into responses, in request order.
    pub(super) fn assemble(
        &self,
        reqs: &[Request],
        plans: Vec<Plan>,
        fetched: HashMap<CacheKey, Reply>,
    ) -> Vec<Response> {
        reqs.iter()
            .zip(plans)
            .map(|(req, plan)| {
                let id = req.id.clone().unwrap_or(Value::Null);
                match plan {
                    Plan::Local(v) => Response::result(id, v),
                    Plan::Refused(e) => Response::error(id, e),
                    Plan::Fetch { key, .. } => match fetched.get(&key) {
                        Some(Reply::Result(v)) => Response::result(id, v.clone()),
                        // The chain's own verdict. Reported per request, so one
                        // reverting call does not fail the nineteen beside it.
                        Some(Reply::Error(e)) => Response::error(id, e.clone()),
                        None => Response::error(id, no_answer()),
                    },
                    Plan::Multicall(calls) => match multicall_reply(&calls, &fetched) {
                        Ok(v) => Response::result(id, v),
                        Err(e) => Response::error(id, e),
                    },
                }
            })
            .collect()
    }
}

/// How one request will be served.
pub(super) enum Plan {
    /// Answered from config.
    Local(CachedResult),
    /// Refused before any upstream call.
    Refused(RpcError),
    /// Needs a value. `class` is `None` for a result-classified method.
    Fetch {
        method: Method,
        key: CacheKey,
        class: Option<Class>,
    },
    /// A client's `aggregate3`, served as the calls inside it.
    Multicall(Vec<Inner>),
}

/// One call inside a client's multicall, planned as a standalone `eth_call`.
pub(super) struct Inner {
    pub(super) key: CacheKey,
    pub(super) class: Option<Class>,
    allow_failure: bool,
    /// Owned: nothing in the client's request holds these params on their own.
    pub(super) params: Value,
}

/// A client's `aggregate3`, answered from its calls' own replies as Multicall3
/// would have answered it.
fn multicall_reply(
    calls: &[Inner],
    fetched: &HashMap<CacheKey, Reply>,
) -> Result<CachedResult, RpcError> {
    let mut outcomes = Vec::with_capacity(calls.len());
    for call in calls {
        match fetched.get(&call.key) {
            Some(Reply::Result(raw)) => {
                outcomes.push((true, multicall::result_bytes(raw).ok_or_else(no_answer)?));
            }
            // A revert is a failed call. `aggregate3` reports one in place when
            // the caller allowed it, and reverts as a whole when not.
            Some(Reply::Error(e)) if is_revert(e) => {
                if !call.allow_failure {
                    return Err(multicall::call_failed());
                }
                let data = e.data.as_deref().and_then(|d| d.parse().ok());
                outcomes.push((false, data.unwrap_or_default()));
            }
            // Anything else is the node failing, which would have failed the
            // whole `eth_call`.
            Some(Reply::Error(e)) => return Err(e.clone()),
            None => return Err(no_answer()),
        }
    }
    Ok(multicall::respond(outcomes).into())
}

/// For a call that came back with neither a value nor a verdict.
fn no_answer() -> RpcError {
    RpcError::new(SERVER_ERROR, "no answer for this call")
}
