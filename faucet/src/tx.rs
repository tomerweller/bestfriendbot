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

fn g_address_to_muxed(addr: &str) -> Result<MuxedAccount, TxError> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(addr)
        .map_err(|e| format!("Invalid G address {addr}: {e}"))?;
    Ok(MuxedAccount::Ed25519(Uint256(pk.0)))
}

fn c_address_to_sc_address(addr: &str) -> Result<ScAddress, TxError> {
    let contract = stellar_strkey::Contract::from_string(addr)
        .map_err(|e| format!("Invalid C address {addr}: {e}"))?;
    Ok(ScAddress::Contract(ContractId(Hash(contract.0))))
}

fn g_address_to_sc_address(addr: &str) -> Result<ScAddress, TxError> {
    let pk = stellar_strkey::ed25519::PublicKey::from_string(addr)
        .map_err(|e| format!("Invalid G address {addr}: {e}"))?;
    Ok(ScAddress::Account(AccountId(
        PublicKey::PublicKeyTypeEd25519(Uint256(pk.0)),
    )))
}

fn i128_to_sc_val(v: i128) -> ScVal {
    ScVal::I128(xdr::Int128Parts {
        hi: (v >> 64) as i64,
        lo: v as u64,
    })
}

fn build_receiver_sc_val(addr: &str, amount: i128) -> Result<ScVal, TxError> {
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
) -> Result<(String, Vec<bool>), TxError> {
    let sender_sc = ScVal::Address(g_address_to_sc_address(source_account)?);
    let token_sc = ScVal::Address(c_address_to_sc_address(token_address)?);

    let receiver_vals: Vec<ScVal> = receivers
        .iter()
        .map(|(addr, amount)| build_receiver_sc_val(addr, *amount))
        .collect::<Result<Vec<_>, _>>()?;
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

    Ok((tx_hash, results))
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

fn sign_envelope(
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

fn parse_results_from_meta(meta: &xdr::TransactionMeta, receiver_count: usize) -> Vec<bool> {
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
