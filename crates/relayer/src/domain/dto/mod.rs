pub mod estimate;
pub mod swap;
pub mod transact;

pub use estimate::{EstimateDepositRequest, EstimateSpendRequest, EstimateSwapRequest};
pub use swap::{DepositRequestDto, SubmitSwapPayload, SwapBlob};
pub use transact::{
    OutputAuxDto, PointDto, ProofDto, PubInputsDto, SpendKind, SubmitSpendPayload, TRANSACT_IN,
    TRANSACT_OUT,
};

#[cfg(test)]
mod wire_contract {
    //! Golden payloads in the exact shape the SDK's
    //! `services/relayer/codec.ts` emits.
    //!
    //! Every field here is named on one side and consumed on the other, so a
    //! rename that lands in only one repo is invisible until a wallet gets a
    //! 422 in production. Deserializing the literal JSON is what makes that a
    //! compile-time-adjacent failure instead.

    use super::*;

    fn point() -> serde_json::Value {
        serde_json::json!({ "x": "1", "y": "2" })
    }

    fn aux() -> serde_json::Value {
        serde_json::json!({
            "clueR": point(),
            "ephPub": point(),
            "ciphertext": "0xdead",
        })
    }

    fn pub_inputs() -> serde_json::Value {
        serde_json::json!({
            "merkleRoot": "1",
            "nullifier": ["1", "2", "3"],
            "outCm": ["4", "5", "6"],
            "publicAssetId": 1,
            "publicIn": 0,
            "publicOut": 500,
            "inCv": [point(), point(), point()],
            "outCv": [point(), point(), point()],
            "outCvDep": [point(), point(), point()],
            "recipient": "0x000000000000000000000000000000000000beef",
            "chainId": 31337,
            "payer": "0x0000000000000000000000000000000000000001",
            "relayer": "0x0000000000000000000000000000000000000002",
        })
    }

    fn proof() -> serde_json::Value {
        serde_json::json!({
            "piA": ["0", "0", "1"],
            "piB": [["0", "0"], ["0", "0"], ["1", "0"]],
            "piC": ["0", "0", "1"],
        })
    }

    fn deposit_request() -> serde_json::Value {
        serde_json::json!({
            "chainId": 31337,
            "publicAssetId": 2,
            "publicIn": 990,
            "payer": "0x0000000000000000000000000000000000000077",
            "recipient": "0x000000000000000000000000000000000000beef",
            "outCm": "0x05",
            "cvDep": ["23", "24"],
            "rcv": "27",
            // The deposit's second leaf. `feeIn` is a JSON number here and a
            // decimal string on `/v1/deposit` — the two DTOs declare it
            // differently, and `codec.ts` encodes each accordingly.
            "feeIn": 5,
            "feeCm": "0x06",
            "feeCvDep": ["25", "26"],
            "feeRcv": "28",
        })
    }

    #[test]
    fn spend_payload_matches_the_sdk_encoding() {
        let raw = serde_json::json!({
            "chainId": 31337,
            "kind": "withdraw",
            "proof": proof(),
            "pubInputs": pub_inputs(),
            "aux": [aux(), aux(), aux()],
        });
        let p: SubmitSpendPayload = serde_json::from_value(raw).expect("spend payload");
        assert_eq!(p.kind, SpendKind::Withdraw);
        assert_eq!(p.pub_inputs.nullifier.len(), TRANSACT_IN);
        assert_eq!(p.aux.len(), TRANSACT_OUT);
    }

    /// The three arity-bearing arrays are fixed-size, so a 2x2 wallet's
    /// payload must be rejected rather than silently truncated.
    #[test]
    fn a_two_output_spend_payload_is_refused() {
        let mut pi = pub_inputs();
        pi["outCm"] = serde_json::json!(["4", "5"]);
        let raw = serde_json::json!({
            "chainId": 31337,
            "kind": "withdraw",
            "proof": proof(),
            "pubInputs": pi,
            "aux": [aux(), aux()],
        });
        assert!(serde_json::from_value::<SubmitSpendPayload>(raw).is_err());
    }

    #[test]
    fn swap_payload_matches_the_sdk_encoding() {
        let raw = serde_json::json!({
            "chainId": 31337,
            "proof": proof(),
            "pubInputs": pub_inputs(),
            "aux": [aux(), aux(), aux()],
            "swap": {
                "adapter": "0x0000000000000000000000000000000000000001",
                "route": "0x00",
                "depositD": deposit_request(),
                "auxD": aux(),
                "feeAuxD": aux(),
                "tokenIn": "0x0000000000000000000000000000000000000111",
                "tokenOut": "0x0000000000000000000000000000000000000222",
                "amountIn": "1000",
                "minOut": "990",
                "deadline": null,
            },
        });
        let p: SubmitSwapPayload = serde_json::from_value(raw).expect("swap payload");
        assert_eq!(p.swap.deposit_d.public_in, 990);
        assert_eq!(p.swap.deposit_d.rcv, "27");
        assert_eq!(p.swap.deposit_d.fee_in, 5);
        assert_eq!(p.swap.deposit_d.fee_rcv, "28");
        assert_eq!(p.swap.deadline, None);
    }
}
