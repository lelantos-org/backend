//! Getting the values a plan needs: the cache, the in-flight map, and the
//! upstream round trip.
//!
//! A request probes the cache for every distinct key it wants, claims the misses
//! it leads, forwards those as one round trip and waits on the rest — so
//! coalescing and batching apply to the same call. The round trip packs the
//! `eth_call`s that share a block into one Multicall3 call where the chain has
//! it, and what comes back is cached only where policy allows.

use super::plan::Plan;
use super::{ChainService, Notice, SERVER_ERROR};
use crate::adapters::upstream::{Answer, OutboundCall};
use crate::app::inflight::{self, Claim, Follow};
use crate::domain::allowlist::Method;
use crate::domain::cache_key::CacheKey;
use crate::domain::error::{AppError, AppResult};
use crate::domain::jsonrpc::{Reply, Request, RpcError, positional};
use crate::domain::multicall;
use crate::domain::policy::{Class, caches_error, classify_result};
use alloy::primitives::Bytes;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use shared::metrics::name;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

impl ChainService {
    /// Resolve every call a plan needs a value for, from cache or upstream.
    pub(super) async fn fetch(
        &self,
        reqs: &[Request],
        plans: &[Plan],
    ) -> AppResult<HashMap<CacheKey, Reply>> {
        let mut pending: Vec<Pending> = Vec::new();
        for (plan, req) in plans.iter().zip(reqs) {
            match plan {
                Plan::Fetch { method, key, class } => pending.push(Pending {
                    method: *method,
                    key: *key,
                    class: *class,
                    params: req.params.as_ref(),
                }),
                // A multicall's calls join everything else outstanding: they
                // share the cache, coalesce with other callers, and are packed
                // again on the way up.
                Plan::Multicall(calls) => pending.extend(calls.iter().map(|call| Pending {
                    method: Method::EthCall,
                    key: call.key,
                    class: call.class,
                    params: Some(&call.params),
                })),
                Plan::Local(_) | Plan::Refused(_) => {}
            }
        }
        if pending.is_empty() {
            return Ok(HashMap::new());
        }

        // A client asking the same question twice in one batch is answered once.
        let mut distinct: Vec<&Pending> = Vec::new();
        for p in &pending {
            if !distinct.iter().any(|d| d.key == p.key) {
                distinct.push(p);
            }
        }

        // Probe once per distinct key, then attribute the result to every
        // request that asked for it. Counting in one place is what keeps the
        // outcome counters summing to the number of requests received.
        let mut out = HashMap::new();
        let mut misses: Vec<&Pending> = Vec::new();
        for p in distinct {
            match self.cached(p).await {
                Some(v) => {
                    out.insert(p.key, v);
                }
                None => misses.push(p),
            }
        }
        for p in &pending {
            self.count(
                p.method,
                if out.contains_key(&p.key) {
                    "hit"
                } else {
                    "miss"
                },
            );
        }

        self.resolve(misses, &mut out).await?;
        Ok(out)
    }

    /// Fetch every miss, sharing upstream calls with any concurrent caller.
    ///
    /// Each key is claimed first. Keys nobody else has in flight are led here
    /// and forwarded together as one round trip; keys someone else is already
    /// fetching are waited on rather than asked again. A herd of wallets whose
    /// batches overlap — each polling `eth_blockNumber` alongside its own reads
    /// — therefore costs one upstream call per distinct question.
    ///
    /// Leading never waits on following, so two requests each leading a key the
    /// other follows cannot deadlock: both forward, both publish, then both
    /// collect.
    ///
    /// A leader that vanishes without an answer — its client disconnected and
    /// the request was dropped — leaves its keys unanswered. They are claimed
    /// again on the next pass, where one of the callers that waited leads.
    pub(super) async fn resolve<'p, 'r>(
        &self,
        mut misses: Vec<&'p Pending<'r>>,
        out: &mut HashMap<CacheKey, Reply>,
    ) -> AppResult<()> {
        while !misses.is_empty() {
            let mut leads = Vec::new();
            let mut follows = Vec::new();
            for p in misses {
                match self.inflight.claim(p.key) {
                    Claim::Lead(lead) => leads.push((p, lead)),
                    Claim::Follow(follow) => follows.push((p, follow)),
                }
            }
            self.lead(leads, out).await?;
            misses = self.follow(follows, out).await?;
        }
        Ok(())
    }

    /// Fetch the keys this caller leads, as one round trip, and publish each
    /// answer to whoever is waiting on it.
    async fn lead(
        &self,
        leads: Vec<(&Pending<'_>, Lead<'_>)>,
        out: &mut HashMap<CacheKey, Reply>,
    ) -> AppResult<()> {
        // A value can land between the caller's probe and the claim: its leader
        // stores it, then releases the key. Probing again here turns that window
        // into a cache read instead of a second upstream call.
        let mut forward = Vec::new();
        for (p, lead) in leads {
            match self.cached(p).await {
                Some(v) => {
                    lead.finish(Ok(v.clone()));
                    out.insert(p.key, v);
                }
                None => forward.push((p, lead)),
            }
        }
        if forward.is_empty() {
            return Ok(());
        }

        let batch: Vec<&Pending> = forward.iter().map(|(p, _)| *p).collect();
        match self.forward_batch(&batch).await {
            Ok(answers) => {
                for ((p, lead), answer) in forward.into_iter().zip(answers) {
                    // Stored before finishing, so a caller arriving after the
                    // key is released finds it cached.
                    let reply = self.store(p, answer).await;
                    lead.finish(Ok(reply.clone()));
                    out.insert(p.key, reply);
                }
                Ok(())
            }
            Err(e) => {
                let shared = Arc::new(e.restated());
                for (_, lead) in forward {
                    lead.finish(Err(Arc::clone(&shared)));
                }
                Err(e)
            }
        }
    }

    /// Collect the answers other callers fetched, returning the keys whose
    /// leader released them without one.
    async fn follow<'p, 'r>(
        &self,
        follows: Vec<(&'p Pending<'r>, Follow<Shared>)>,
        out: &mut HashMap<CacheKey, Reply>,
    ) -> AppResult<Vec<&'p Pending<'r>>> {
        let mut orphaned = Vec::new();
        for (p, follow) in follows {
            match follow.wait().await {
                Some(Ok(reply)) => {
                    out.insert(p.key, reply);
                }
                Some(Err(e)) => return Err(e.restated()),
                None => {
                    debug!(
                        method = p.method.label(),
                        "in-flight leader went away; claiming its key"
                    );
                    orphaned.push(p);
                }
            }
        }
        Ok(orphaned)
    }

    /// The cached value for a pending call, if any.
    ///
    /// A result-classified method has to try every class it could have landed
    /// in, since the class was chosen from the response rather than the request.
    async fn cached(&self, p: &Pending<'_>) -> Option<Reply> {
        let Some(class) = p.class else {
            for c in Class::FROM_RESULT {
                if let Some(v) = self.caches.get(c).get(&p.key).await {
                    return Some(v);
                }
            }
            return None;
        };
        self.caches.get(class).get(&p.key).await
    }

    /// Forward outstanding calls as one upstream round trip, one answer per
    /// call in order. A single call goes out as a bare object; see the upstream
    /// adapter.
    ///
    /// `eth_call`s sharing a block are packed into one Multicall3 call where the
    /// chain has it; see [`multicall`]. A pack member that failed, or a pack
    /// that did not unpack, costs a second round trip asking those calls
    /// unpacked — so the verdict a caller sees is always the node's own.
    async fn forward_batch(&self, misses: &[&Pending<'_>]) -> AppResult<Vec<Answer>> {
        // If any call in the batch needs historical state, the whole batch goes
        // to an archive endpoint. Splitting into two batches would save nothing
        // in the normal case, where the primary is archive and serves both.
        let needs_archive = misses.iter().any(|p| self.needs_archive(p));
        let layout = self.layout(misses).await;

        // Calls going alone first, then one call per pack; answers come back in
        // the same order.
        let calls: Vec<OutboundCall<'_>> = layout
            .alone
            .iter()
            .map(|&i| outbound(misses[i]))
            .chain(layout.packs.iter().map(|pack| OutboundCall {
                method: Method::EthCall.label(),
                params: Some(&pack.params),
            }))
            .collect();
        let mut answers = self.forward(&calls, needs_archive).await?.into_iter();

        let mut slots: Vec<Option<Answer>> = misses.iter().map(|_| None).collect();
        for &i in &layout.alone {
            slots[i] = answers.next();
        }
        let mut again = Vec::new();
        for pack in &layout.packs {
            let Some(results) = self.pack_results(answers.next(), pack.members.len()) else {
                again.extend(&pack.members);
                continue;
            };
            for (&i, result) in pack.members.iter().zip(results) {
                match result {
                    Some(raw) => slots[i] = Some(Answer::Result(raw)),
                    None => again.push(i),
                }
            }
        }

        debug!(
            chain_id = self.chain_id,
            alone = layout.alone.len(),
            packs = layout.packs.len(),
            asked_again = again.len(),
            "upstream round trip"
        );
        if !again.is_empty() {
            let calls: Vec<OutboundCall<'_>> = again.iter().map(|&i| outbound(misses[i])).collect();
            for (&i, answer) in again.iter().zip(self.forward(&calls, needs_archive).await?) {
                slots[i] = Some(answer);
            }
        }

        slots
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| AppError::Upstream("upstream returned no answer".into()))
    }

    /// What a pack's answer says about each of its `calls`, or `None` when the
    /// answer says nothing usable and every call must be asked again alone.
    fn pack_results(
        &self,
        answer: Option<Answer>,
        calls: usize,
    ) -> Option<Vec<Option<Box<RawValue>>>> {
        match answer? {
            Answer::Result(raw) => {
                let results = multicall::unpack(&raw, calls);
                // The probe found code, so an answer that is not an `aggregate3`
                // return means the chain changed under this process — a reset
                // dev chain — and every pack from now on costs two round trips
                // until a restart re-probes.
                if results.is_none() && self.notices.admit(Notice::UnreadablePack) {
                    warn!(
                        chain_id = self.chain_id,
                        calls,
                        "a Multicall3 pack did not unpack; asking its calls one by one \
                         (repeats suppressed for 10m)"
                    );
                }
                results
            }
            // An error for the whole pack — its shared gas, typically. The calls
            // are asked alone, and each gets its own verdict.
            Answer::Error(error) => {
                debug!(chain_id = self.chain_id, calls, %error, "a Multicall3 pack failed; asking its calls one by one");
                None
            }
        }
    }

    /// How `misses` go upstream: packed where they may be and the chain has
    /// Multicall3, each alone otherwise. The chain is only probed once there is
    /// something to pack.
    async fn layout(&self, misses: &[&Pending<'_>]) -> multicall::Layout {
        let layout = multicall::layout(misses.iter().map(|p| (p.method, positional(p.params))));
        if layout.packs.is_empty() || self.has_multicall().await {
            layout
        } else {
            multicall::Layout::unpacked(misses.len())
        }
    }

    /// Whether this chain has code at [`multicall::MULTICALL3`], asked once.
    ///
    /// Probed rather than configured. Every production chain has it at the
    /// same address and a bare anvil has none, and a wrong setting would not
    /// error: every pack would come back `0x` and be asked again call by call,
    /// doubling the round trips it exists to save.
    ///
    /// A probe that fails is not remembered, so a transport blip costs one
    /// batch its packing rather than the process; the price is one extra call
    /// per packable batch while the upstream is down. Code that disappears
    /// later — a dev chain reset under a running proxy — is caught by the pack
    /// failing to unpack.
    async fn has_multicall(&self) -> bool {
        let probe = self.multicall.get_or_try_init(|| async {
            let params = json!([multicall::MULTICALL3.to_string(), "latest"]);
            let calls = [OutboundCall {
                method: "eth_getCode",
                params: Some(&params),
            }];
            let code = match self.forward(&calls, false).await?.into_iter().next() {
                Some(Answer::Result(raw)) => serde_json::from_str::<String>(raw.get())
                    .map_err(|e| AppError::Upstream(format!("eth_getCode result: {e}")))?,
                _ => return Err(AppError::Upstream("eth_getCode answered no code".into())),
            };
            let present = code.len() > "0x".len();
            // Either outcome, once: this explains the chain's whole `eth_call`
            // bill, and nobody should have to infer it from a provider dashboard.
            info!(
                chain_id = self.chain_id,
                multicall3 = present,
                "probed for Multicall3; eth_call misses pack through it only when present"
            );
            Ok(present)
        });
        probe.await.copied().unwrap_or(false)
    }

    /// Cache an answer if policy allows, and return it either way.
    async fn store(&self, p: &Pending<'_>, answer: Answer) -> Reply {
        let (reply, class) = match answer {
            Answer::Result(raw) => {
                // The one parse of the body, reused below rather than repeated.
                let height = self.observe(p.method, &raw);
                // A class the request did not fix is chosen from the result: an
                // unmined receipt is never cached, and a shallow one is held
                // briefly rather than for an hour.
                let class = p
                    .class
                    .or_else(|| classify_result(height, self.tip.get(), self.limits.reorg_depth));
                (Reply::Result(raw.into()), class)
            }
            // Returned to the caller either way. Cached only when it is the
            // chain's answer rather than the node's trouble — a revert — and
            // then for exactly as long as a result in the same class.
            Answer::Error(v) => {
                let e = verdict(&v);
                let class = p
                    .class
                    .filter(|_| caches_error(p.method, positional(p.params), &e));
                (Reply::Error(e), class)
            }
        };
        if let Some(c) = class {
            self.caches.get(c).insert(p.key, reply.clone()).await;
        }
        reply
    }

    /// Forward to the upstream, recording what it cost.
    async fn forward(
        &self,
        calls: &[OutboundCall<'_>],
        needs_archive: bool,
    ) -> AppResult<Vec<Answer>> {
        let started = Instant::now();
        let res = self.upstream.call(calls, needs_archive).await;

        // A batch spans several methods, so it is labelled by its first rather
        // than split across them: the histogram measures round trips, and the
        // round trip is what a batch has one of.
        let label = calls.first().map_or("batch", |c| c.method);
        metrics::histogram!(
            name::RPC_PROXY_UPSTREAM_DURATION,
            "chain" => self.chain_id.to_string(),
            "method" => label.to_string(),
        )
        .record(started.elapsed().as_secs_f64());

        if res.is_ok() {
            // Counted on what actually went upstream, so the metric tracks the
            // bill rather than the traffic.
            metrics::counter!(
                name::RPC_PROXY_UPSTREAM_UNITS,
                "chain" => self.chain_id.to_string(),
            )
            .increment(calls.len() as u64);
        }
        res
    }

    /// Whether this call reads historical *state*, which a pruning node cannot
    /// answer.
    ///
    /// Block bodies and receipts are retained by ordinary full nodes; only
    /// state reads at an old height need an archive. So this is narrower than
    /// "finalized": it is `eth_call` and `eth_getBalance` at a finalized height,
    /// which is exactly the yield index's access pattern.
    fn needs_archive(&self, p: &Pending) -> bool {
        matches!(p.method, Method::EthCall | Method::EthGetBalance)
            && p.class == Some(Class::Finalized)
    }

    /// Feed the head tracker from a response that happens to carry a height,
    /// and report that height.
    ///
    /// Returned so [`Self::store`] can classify a result-classified answer
    /// without parsing the body again: a receipt carries every log the
    /// transaction emitted, and it is polled once a second per pending deposit.
    fn observe(&self, method: Method, raw: &RawValue) -> Option<u64> {
        match method {
            Method::EthBlockNumber => {
                self.tip.observe_block_number(raw);
                None
            }
            Method::EthGetBlockByNumber | Method::EthGetBlockByHash => self.tip.observe_block(raw),
            Method::EthGetTransactionReceipt | Method::EthGetTransactionByHash => {
                self.tip.observe_inclusion(raw)
            }
            _ => None,
        }
    }
}

/// What a leader publishes to the callers waiting on its key. A failure is
/// shared as well as a value, so the waiters on a failing upstream fail with
/// that one call rather than each retrying it.
pub(super) type Shared = Result<Reply, Arc<AppError>>;

/// This service's claim on one in-flight key.
type Lead<'a> = inflight::Lead<'a, CacheKey, Shared>;

/// A pending call as it goes upstream on its own.
fn outbound<'a>(p: &Pending<'a>) -> OutboundCall<'a> {
    OutboundCall {
        method: p.method.label(),
        params: p.params,
    }
}

/// One request that needs a value, and how to get it.
pub(super) struct Pending<'r> {
    pub(super) method: Method,
    pub(super) key: CacheKey,
    /// `None` for a method whose cache class is chosen from the result.
    pub(super) class: Option<Class>,
    /// The caller's params, forwarded as sent.
    pub(super) params: Option<&'r Value>,
}

/// Restate an upstream error object as this endpoint's error.
///
/// The code, message and revert data are the chain's own — a caller
/// distinguishing `execution reverted` from a gas failure needs them intact, and
/// decodes a custom error from the data — but the shape is rebuilt rather than
/// forwarded, so an upstream cannot inject arbitrary fields into a response this
/// service signs its name to. Data that is not hex bytes is dropped.
fn verdict(v: &Value) -> RpcError {
    RpcError {
        code: v
            .get("code")
            .and_then(Value::as_i64)
            .unwrap_or(SERVER_ERROR),
        message: v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("upstream returned an error")
            .to_string(),
        data: v
            .get("data")
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<Bytes>().ok())
            .map(|d| d.to_string()),
    }
}
