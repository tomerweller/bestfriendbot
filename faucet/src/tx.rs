use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};
use stellar_rpc_client::Client;
use stellar_xdr::curr::{
    self as xdr, AccountId, ContractDataDurability, ContractExecutable, ContractId,
    DecoratedSignature, Hash, HostFunction, InvokeContractArgs, InvokeHostFunctionOp,
    LedgerEntryData, LedgerKey, LedgerKeyContractData, Limits, Memo, MuxedAccount, Operation,
    OperationBody, Preconditions, PublicKey, ReadXdr, ScAddress, ScMap,
    ScMapEntry, ScSymbol, ScVal, ScVec, SequenceNumber, Signature, SignatureHint, Transaction,
    TransactionEnvelope, TransactionExt, TransactionSignaturePayload,
    TransactionSignaturePayloadTaggedTransaction, TransactionV1Envelope, Uint256, WriteXdr,
};

#[derive(Debug)]
pub struct TxError {
    pub message: String,
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<String> for TxError {
    fn from(s: String) -> Self {
        Self { message: s }
    }
}

impl From<&str> for TxError {
    fn from(s: &str) -> Self {
        Self {
            message: s.to_string(),
        }
    }
}

pub(crate) fn g_address_to_muxed(addr: &str) -> Result<MuxedAccount, TxError> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(addr)
        .map_err(|e| format!("Invalid G address {addr}: {e}"))?;
    Ok(MuxedAccount::Ed25519(Uint256(pk.0)))
}

pub(crate) fn c_address_to_sc_address(addr: &str) -> Result<ScAddress, TxError> {
    let contract = stellar_strkey::Contract::from_string(addr)
        .map_err(|e| format!("Invalid C address {addr}: {e}"))?;
    Ok(ScAddress::Contract(ContractId(Hash(contract.0))))
}

pub(crate) fn g_address_to_sc_address(addr: &str) -> Result<ScAddress, TxError> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(addr)
        .map_err(|e| format!("Invalid G address {addr}: {e}"))?;
    Ok(ScAddress::Account(AccountId(
        PublicKey::PublicKeyTypeEd25519(Uint256(pk.0)),
    )))
}

pub(crate) fn i128_to_sc_val(v: i128) -> ScVal {
    ScVal::I128(xdr::Int128Parts {
        hi: (v >> 64) as i64,
        lo: v as u64,
    })
}

pub(crate) fn build_receiver_sc_val(addr: &str, amount: i128) -> Result<ScVal, TxError> {
    let sc_addr = g_address_to_sc_address(addr)?;
    let map = ScMap(
        vec![
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("address".try_into().unwrap())),
                val: ScVal::Address(sc_addr),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("amount".try_into().unwrap())),
                val: i128_to_sc_val(amount),
            },
        ]
        .try_into()
        .unwrap(),
    );
    Ok(ScVal::Map(Some(map)))
}

pub async fn verify_contract_deployed(
    rpc: &Client,
    contract_address: &str,
) -> Result<(), TxError> {
    let contract_hash = stellar_strkey::Contract::from_string(contract_address)
        .map_err(|e| format!("Invalid contract address: {e}"))?;

    let key = LedgerKey::ContractData(LedgerKeyContractData {
        contract: ScAddress::Contract(ContractId(Hash(contract_hash.0))),
        key: ScVal::LedgerKeyContractInstance,
        durability: ContractDataDurability::Persistent,
    });

    let keys = vec![key];
    let result = rpc
        .get_ledger_entries(&keys)
        .await
        .map_err(|e| format!("Failed to query contract data: {e}"))?;

    if result.entries.is_none() || result.entries.as_ref().unwrap().is_empty() {
        return Err("Contract not found on-chain".into());
    }

    let entry = &result.entries.as_ref().unwrap()[0];
    let data = LedgerEntryData::from_xdr_base64(&entry.xdr, Limits::none())
        .map_err(|e| format!("Failed to decode ledger entry: {e}"))?;
    if let LedgerEntryData::ContractData(cd) = &data {
        if let ScVal::ContractInstance(instance) = &cd.val {
            match &instance.executable {
                ContractExecutable::Wasm(_) => return Ok(()),
                ContractExecutable::StellarAsset => {
                    return Err("Address is a SAC, not the batch_transfer contract".into())
                }
            }
        }
    }

    Err("Unexpected ledger entry format".into())
}

pub async fn verify_account_exists(rpc: &Client, account: &str) -> Result<(), TxError> {
    rpc.get_account(account)
        .await
        .map_err(|e| format!("Failed to fetch funding account {account}: {e}"))?;
    Ok(())
}

pub async fn invoke_batch_transfer(
    rpc: &Client,
    network_passphrase: &str,
    signing_key: &ed25519_dalek::SigningKey,
    source_account: &str,
    contract_address: &str,
    token_address: &str,
    receivers: &[(String, i128)],
) -> Result<(String, String, Vec<bool>), TxError> {
    let sender_sc = ScVal::Address(g_address_to_sc_address(source_account)?);
    let token_sc = ScVal::Address(c_address_to_sc_address(token_address)?);

    // Build Vec<(MuxedAddress, i128)> — tuples are encoded as ScVec pairs
    let receiver_vals: Vec<ScVal> = receivers
        .iter()
        .map(|(addr, amount)| {
            let addr_sc = ScVal::Address(g_address_to_sc_address(addr)?);
            let amount_sc = i128_to_sc_val(*amount);
            let tuple = ScVal::Vec(Some(ScVec(
                vec![addr_sc, amount_sc].try_into().unwrap(),
            )));
            Ok(tuple)
        })
        .collect::<Result<Vec<_>, TxError>>()?;
    let receivers_sc = ScVal::Vec(Some(ScVec(receiver_vals.try_into().unwrap())));

    let contract_sc_address = c_address_to_sc_address(contract_address)?;
    let invoke_args = InvokeContractArgs {
        contract_address: contract_sc_address,
        function_name: ScSymbol("batch_transfer".try_into().unwrap()),
        args: vec![sender_sc, token_sc, receivers_sc].try_into().unwrap(),
    };

    // Fetch account sequence number
    let account = rpc
        .get_account(source_account)
        .await
        .map_err(|e| format!("Failed to fetch account: {e}"))?;
    let seq_num = account.seq_num;

    // Build transaction
    let source_muxed = g_address_to_muxed(source_account)?;
    let op = Operation {
        source_account: None,
        body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
            host_function: HostFunction::InvokeContract(invoke_args),
            auth: Default::default(),
        }),
    };

    let tx = Transaction {
        source_account: source_muxed,
        fee: 100,
        seq_num: SequenceNumber(seq_num.0 + 1),
        cond: Preconditions::None,
        memo: Memo::None,
        operations: vec![op].try_into().unwrap(),
        ext: TransactionExt::V0,
    };

    let envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: Default::default(),
    });

    // Simulate
    let sim = rpc
        .simulate_transaction_envelope(&envelope, None)
        .await
        .map_err(|e| format!("Simulation failed: {e}"))?;

    // Check for simulation error
    if let Some(ref error) = sim.error {
        return Err(format!("Simulation returned error: {error}").into());
    }

    // Assemble the transaction with simulation results
    let assembled_envelope = assemble_transaction(&envelope, &sim)?;

    // Sign the assembled envelope
    let signed_envelope = sign_envelope(&assembled_envelope, signing_key, network_passphrase)?;

    // Compute tx hash for the response
    let network_id = {
        let mut hasher = Sha256::new();
        hasher.update(network_passphrase.as_bytes());
        let result: [u8; 32] = hasher.finalize().into();
        result
    };
    let tx_hash_bytes = signed_envelope
        .hash(network_id)
        .map_err(|e| format!("Failed to compute tx hash: {e}"))?;
    let tx_hash = hex::encode(tx_hash_bytes);

    // Submit and poll
    let response = rpc
        .send_transaction_polling(&signed_envelope)
        .await
        .map_err(|e| format!("Transaction submission failed: {e}"))?;

    // Parse results from transaction meta
    let results = if let Some(ref meta) = response.result_meta {
        parse_results_from_meta(meta, receivers.len())
    } else {
        vec![true; receivers.len()]
    };

    // Serialize the signed envelope to base64 XDR
    let envelope_xdr = signed_envelope
        .to_xdr_base64(Limits::none())
        .map_err(|e| format!("Failed to encode envelope XDR: {e}"))?;

    Ok((tx_hash, envelope_xdr, results))
}

fn assemble_transaction(
    envelope: &TransactionEnvelope,
    sim: &stellar_rpc_client::SimulateTransactionResponse,
) -> Result<TransactionEnvelope, TxError> {
    let tx = match envelope {
        TransactionEnvelope::Tx(v1) => &v1.tx,
        _ => return Err("Expected TransactionEnvelope::Tx".into()),
    };

    // Parse the soroban transaction data from simulation, then add safety buffers
    let mut transaction_data =
        xdr::SorobanTransactionData::from_xdr_base64(&sim.transaction_data, Limits::none())
            .map_err(|e| format!("Failed to parse transaction data: {e}"))?;

    // Add 15% buffer to resource limits to handle ledger state drift between sim and submit
    let res = &mut transaction_data.resources;
    res.instructions = res.instructions.saturating_add(res.instructions / 7);
    res.disk_read_bytes = res.disk_read_bytes.saturating_add(res.disk_read_bytes / 7);
    res.write_bytes = res.write_bytes.saturating_add(res.write_bytes / 7);
    transaction_data.resource_fee =
        transaction_data.resource_fee.saturating_add(transaction_data.resource_fee / 7);

    // Get auth from simulation results
    let auth = if let Some(first_result) = sim.results.first() {
        first_result
            .auth
            .iter()
            .map(|a| {
                xdr::SorobanAuthorizationEntry::from_xdr_base64(a, Limits::none())
                    .map_err(|e| format!("Failed to parse auth entry: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![]
    };

    // Rebuild the operation with auth entries
    let op = &tx.operations[0];
    let new_op = match &op.body {
        OperationBody::InvokeHostFunction(invoke) => Operation {
            source_account: op.source_account.clone(),
            body: OperationBody::InvokeHostFunction(InvokeHostFunctionOp {
                host_function: invoke.host_function.clone(),
                auth: auth.try_into().unwrap(),
            }),
        },
        _ => return Err("Expected InvokeHostFunction operation".into()),
    };

    // Calculate fee: embedded resource_fee (already buffered) + base inclusion fee
    let total_fee = (transaction_data.resource_fee as u64).saturating_add(100);

    let new_tx = Transaction {
        source_account: tx.source_account.clone(),
        fee: total_fee as u32,
        seq_num: tx.seq_num.clone(),
        cond: tx.cond.clone(),
        memo: tx.memo.clone(),
        operations: vec![new_op].try_into().unwrap(),
        ext: TransactionExt::V1(transaction_data),
    };

    Ok(TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: new_tx,
        signatures: Default::default(),
    }))
}

pub(crate) fn sign_envelope(
    envelope: &TransactionEnvelope,
    signing_key: &ed25519_dalek::SigningKey,
    network_passphrase: &str,
) -> Result<TransactionEnvelope, TxError> {
    let tx = match envelope {
        TransactionEnvelope::Tx(v1) => &v1.tx,
        _ => return Err("Expected TransactionEnvelope::Tx".into()),
    };

    let network_hash = {
        let mut hasher = Sha256::new();
        hasher.update(network_passphrase.as_bytes());
        let result = hasher.finalize();
        Hash(result.into())
    };

    let payload = TransactionSignaturePayload {
        network_id: network_hash,
        tagged_transaction: TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
    };

    let payload_bytes = payload
        .to_xdr(Limits::none())
        .map_err(|e| format!("Failed to encode signature payload: {e}"))?;

    let tx_hash = {
        let mut hasher = Sha256::new();
        hasher.update(&payload_bytes);
        hasher.finalize()
    };

    let signature = signing_key.sign(&tx_hash);
    let verifying_key = signing_key.verifying_key();
    let hint_bytes = verifying_key.as_bytes();
    let hint = SignatureHint([
        hint_bytes[28],
        hint_bytes[29],
        hint_bytes[30],
        hint_bytes[31],
    ]);

    let decorated = DecoratedSignature {
        hint,
        signature: Signature(signature.to_bytes().to_vec().try_into().unwrap()),
    };

    let signed = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx: tx.clone(),
        signatures: vec![decorated].try_into().unwrap(),
    });

    Ok(signed)
}

pub(crate) fn parse_results_from_meta(meta: &xdr::TransactionMeta, receiver_count: usize) -> Vec<bool> {
    if let xdr::TransactionMeta::V3(v3) = meta {
        if let Some(ref soroban_meta) = v3.soroban_meta {
            if let ScVal::Vec(Some(vec)) = &soroban_meta.return_value {
                return vec
                    .0
                    .iter()
                    .map(|v| matches!(v, ScVal::Bool(true)))
                    .collect();
            }
        }
    }
    // If we can't parse, assume all succeeded (tx was successful)
    vec![true; receiver_count]
}

#[cfg(test)]
mod tests {
    use super::*;

    // A valid testnet G address (well-formed, 56 chars)
    const VALID_G: &str = "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7";
    // A valid testnet C address
    const VALID_C: &str = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC";

    #[test]
    fn test_g_address_to_sc_address_valid() {
        let result = g_address_to_sc_address(VALID_G);
        assert!(result.is_ok());
        match result.unwrap() {
            ScAddress::Account(_) => {} // expected
            other => panic!("Expected ScAddress::Account, got {other:?}"),
        }
    }

    #[test]
    fn test_g_address_to_sc_address_invalid() {
        let result = g_address_to_sc_address("GINVALID");
        assert!(result.is_err());
    }

    #[test]
    fn test_c_address_to_sc_address_valid() {
        let result = c_address_to_sc_address(VALID_C);
        assert!(result.is_ok());
        match result.unwrap() {
            ScAddress::Contract(_) => {} // expected
            other => panic!("Expected ScAddress::Contract, got {other:?}"),
        }
    }

    #[test]
    fn test_c_address_to_sc_address_invalid() {
        let result = c_address_to_sc_address("CINVALID");
        assert!(result.is_err());
    }

    #[test]
    fn test_g_address_to_muxed_valid() {
        let result = g_address_to_muxed(VALID_G);
        assert!(result.is_ok());
        match result.unwrap() {
            MuxedAccount::Ed25519(_) => {}
            other => panic!("Expected MuxedAccount::Ed25519, got {other:?}"),
        }
    }

    #[test]
    fn test_g_address_to_muxed_invalid() {
        let result = g_address_to_muxed("not-a-valid-address");
        assert!(result.is_err());
    }

    #[test]
    fn test_i128_zero() {
        let val = i128_to_sc_val(0);
        match val {
            ScVal::I128(parts) => {
                assert_eq!(parts.hi, 0);
                assert_eq!(parts.lo, 0);
            }
            other => panic!("Expected ScVal::I128, got {other:?}"),
        }
    }

    #[test]
    fn test_i128_large() {
        let large: i128 = 1_000_000_000_000_000_000; // 1e18
        let val = i128_to_sc_val(large);
        match val {
            ScVal::I128(parts) => {
                let reconstructed = ((parts.hi as i128) << 64) | (parts.lo as i128);
                assert_eq!(reconstructed, large);
            }
            other => panic!("Expected ScVal::I128, got {other:?}"),
        }
    }

    #[test]
    fn test_build_receiver_sc_val() {
        let result = build_receiver_sc_val(VALID_G, 1000);
        assert!(result.is_ok());
        match result.unwrap() {
            ScVal::Map(Some(map)) => {
                assert_eq!(map.0.len(), 2);
                // First entry should be "address"
                match &map.0[0].key {
                    ScVal::Symbol(s) => assert_eq!(s.0.to_string(), "address"),
                    other => panic!("Expected Symbol key, got {other:?}"),
                }
                // Second entry should be "amount"
                match &map.0[1].key {
                    ScVal::Symbol(s) => assert_eq!(s.0.to_string(), "amount"),
                    other => panic!("Expected Symbol key, got {other:?}"),
                }
            }
            other => panic!("Expected ScVal::Map, got {other:?}"),
        }
    }

    #[test]
    fn test_build_receiver_sc_val_invalid_address() {
        let result = build_receiver_sc_val("GINVALID", 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_sign_envelope_produces_signature() {
        // Build a minimal unsigned envelope
        let source = g_address_to_muxed(VALID_G).unwrap();
        let tx = Transaction {
            source_account: source,
            fee: 100,
            seq_num: SequenceNumber(1),
            cond: Preconditions::None,
            memo: Memo::None,
            operations: vec![Operation {
                source_account: None,
                body: OperationBody::Inflation,
            }]
            .try_into()
            .unwrap(),
            ext: TransactionExt::V0,
        };
        let envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
            tx,
            signatures: Default::default(),
        });

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let result = sign_envelope(&envelope, &signing_key, "Test SDF Network ; September 2015");
        assert!(result.is_ok());

        match result.unwrap() {
            TransactionEnvelope::Tx(v1) => {
                assert_eq!(v1.signatures.len(), 1);
            }
            other => panic!("Expected Tx envelope, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_results_from_v3_meta() {
        // Build V3 meta with soroban return value of [true, false, true]
        let return_value = ScVal::Vec(Some(ScVec(
            vec![ScVal::Bool(true), ScVal::Bool(false), ScVal::Bool(true)]
                .try_into()
                .unwrap(),
        )));
        let soroban_meta = xdr::SorobanTransactionMeta {
            ext: xdr::SorobanTransactionMetaExt::V0,
            events: Default::default(),
            return_value,
            diagnostic_events: Default::default(),
        };
        let meta = xdr::TransactionMeta::V3(xdr::TransactionMetaV3 {
            ext: xdr::ExtensionPoint::V0,
            tx_changes_before: Default::default(),
            operations: Default::default(),
            tx_changes_after: Default::default(),
            soroban_meta: Some(soroban_meta),
        });

        let results = parse_results_from_meta(&meta, 3);
        assert_eq!(results, vec![true, false, true]);
    }

    #[test]
    fn test_parse_results_fallback_non_v3() {
        // V0 meta should fall back to all-true
        let meta = xdr::TransactionMeta::V0(Default::default());
        let results = parse_results_from_meta(&meta, 3);
        assert_eq!(results, vec![true, true, true]);
    }

    #[test]
    fn test_parse_results_fallback_no_soroban_meta() {
        // V3 meta without soroban_meta should fall back
        let meta = xdr::TransactionMeta::V3(xdr::TransactionMetaV3 {
            ext: xdr::ExtensionPoint::V0,
            tx_changes_before: Default::default(),
            operations: Default::default(),
            tx_changes_after: Default::default(),
            soroban_meta: None,
        });
        let results = parse_results_from_meta(&meta, 2);
        assert_eq!(results, vec![true, true]);
    }
}
