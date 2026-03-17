#![cfg(test)]
extern crate std;

use super::*;
use soroban_env_host::InvocationResourceLimits;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{vec, Env};
use std::eprintln;

fn create_token<'a>(
    env: &'a Env,
    admin: &'a Address,
) -> (TokenClient<'a>, StellarAssetClient<'a>) {
    let contract_id = env.register_stellar_asset_contract_v2(admin.clone()).address();
    (
        TokenClient::new(env, &contract_id),
        StellarAssetClient::new(env, &contract_id),
    )
}

#[test]
fn test_happy_path() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver1 = Address::generate(&env);
    let receiver2 = Address::generate(&env);
    let receiver3 = Address::generate(&env);

    let (token, sac) = create_token(&env, &admin);
    sac.mint(&sender, &1000);

    let contract_id = env.register(BatchTransferContract, ());
    let client = BatchTransferContractClient::new(&env, &contract_id);

    let receivers = vec![
        &env,
        Receiver {
            address: receiver1.clone(),
            amount: 100,
        },
        Receiver {
            address: receiver2.clone(),
            amount: 200,
        },
        Receiver {
            address: receiver3.clone(),
            amount: 300,
        },
    ];

    let results = client.batch_transfer(&sender, &token.address, &receivers);

    assert_eq!(results, vec![&env, true, true, true]);
    assert_eq!(token.balance(&sender), 400);
    assert_eq!(token.balance(&receiver1), 100);
    assert_eq!(token.balance(&receiver2), 200);
    assert_eq!(token.balance(&receiver3), 300);
}

#[test]
fn test_partial_failure() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver1 = Address::generate(&env);
    let receiver2 = Address::generate(&env);

    let (token, sac) = create_token(&env, &admin);
    sac.mint(&sender, &150);

    let contract_id = env.register(BatchTransferContract, ());
    let client = BatchTransferContractClient::new(&env, &contract_id);

    let receivers = vec![
        &env,
        Receiver {
            address: receiver1.clone(),
            amount: 100,
        },
        Receiver {
            address: receiver2.clone(),
            amount: 200,
        },
    ];

    let results = client.batch_transfer(&sender, &token.address, &receivers);

    assert_eq!(results, vec![&env, true, false]);
    assert_eq!(token.balance(&sender), 50);
    assert_eq!(token.balance(&receiver1), 100);
    assert_eq!(token.balance(&receiver2), 0);
}

#[test]
fn test_empty_batch() {
    let env = Env::default();
    env.mock_all_auths();

    let sender = Address::generate(&env);
    let token_addr = Address::generate(&env);

    let contract_id = env.register(BatchTransferContract, ());
    let client = BatchTransferContractClient::new(&env, &contract_id);

    let receivers: Vec<Receiver> = vec![&env];
    let results = client.batch_transfer(&sender, &token_addr, &receivers);

    assert_eq!(results, vec![&env]);
}

// ---------------------------------------------------------------------------
// Resource limit evaluation
// ---------------------------------------------------------------------------

/// Live testnet/mainnet per-transaction limits (post-SLP-0004, all SLPs voted).
/// The SDK's built-in `InvocationResourceLimits::mainnet()` is stale (pre-SLP-0004),
/// so we define the actual live limits here.
fn live_network_limits() -> InvocationResourceLimits {
    InvocationResourceLimits {
        instructions: 400_000_000,
        mem_bytes: 41_943_040,
        disk_read_entries: 200,
        write_entries: 200,
        ledger_entries: 400,
        disk_read_bytes: 200_000,
        write_bytes: 132_096,
        contract_events_size_bytes: 16_384,
        max_contract_data_key_size_bytes: 250,
        max_contract_data_entry_size_bytes: 65_536,
        max_contract_code_entry_size_bytes: 131_072,
    }
}

/// Measure resource consumption for a batch of `n` SAC transfers.
fn measure(n: u32) -> soroban_env_host::InvocationResources {
    let env = Env::default();
    env.mock_all_auths();
    // Disable SDK resource enforcement — we measure and check manually.
    env.cost_estimate().disable_resource_limits();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let (token, sac) = create_token(&env, &admin);
    sac.mint(&sender, &(n as i128 * 1_000));

    let contract_id = env.register(BatchTransferContract, ());
    let client = BatchTransferContractClient::new(&env, &contract_id);

    let mut receivers = vec![&env];
    for _ in 0..n {
        receivers.push_back(Receiver {
            address: Address::generate(&env),
            amount: 100,
        });
    }

    let _ = client.batch_transfer(&sender, &token.address, &receivers);
    env.cost_estimate().resources()
}

fn pct(used: i64, limit: i64) -> f64 {
    used as f64 / limit as f64 * 100.0
}

fn fits_limits(res: &soroban_env_host::InvocationResources, lim: &InvocationResourceLimits) -> bool {
    res.instructions <= lim.instructions
        && res.write_entries <= lim.write_entries
        && (res.disk_read_entries + res.memory_read_entries + res.write_entries) <= lim.ledger_entries
        && res.contract_events_size_bytes <= lim.contract_events_size_bytes
        && res.write_bytes <= lim.write_bytes
        && res.disk_read_bytes <= lim.disk_read_bytes
}

fn binding_name(res: &soroban_env_host::InvocationResources, lim: &InvocationResourceLimits) -> (&'static str, f64) {
    let footprint = res.disk_read_entries + res.memory_read_entries + res.write_entries;
    let candidates: [(&str, f64); 6] = [
        ("cpu", pct(res.instructions, lim.instructions)),
        ("write_ent", pct(res.write_entries as i64, lim.write_entries as i64)),
        ("footprint", pct(footprint as i64, lim.ledger_entries as i64)),
        ("evt_bytes", pct(res.contract_events_size_bytes as i64, lim.contract_events_size_bytes as i64)),
        ("wr_bytes", pct(res.write_bytes as i64, lim.write_bytes as i64)),
        ("rd_bytes", pct(res.disk_read_bytes as i64, lim.disk_read_bytes as i64)),
    ];
    candidates.into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).unwrap()
}

#[test]
fn find_max_batch_size() {
    let lim = live_network_limits();

    eprintln!();
    eprintln!("{:=<120}", "");
    eprintln!("  BATCH TRANSFER — RESOURCE CONSUMPTION vs LIVE NETWORK LIMITS (post-SLP-0004)");
    eprintln!("{:=<120}", "");

    // ── Phase 1: Profile at various N ──────────────────────────────────
    let test_sizes: &[u32] = &[1, 5, 10, 25, 50, 75, 80, 85, 90, 100, 125, 150, 199];

    eprintln!(
        "\n {:>4} | {:>12} {:>6} | {:>5} {:>6} | {:>5} {:>6} | {:>7} {:>6} | {:>7} {:>6} | {:>7} {:>6} | binding",
        "N", "cpu", "%", "wrEn", "%", "fp", "%", "evtB", "%", "wrB", "%", "rdB", "%"
    );
    eprintln!("{:-<120}", "");

    for &n in test_sizes {
        let res = measure(n);
        let footprint = res.disk_read_entries + res.memory_read_entries + res.write_entries;
        let (bname, bpct) = binding_name(&res, &lim);
        let tag = if !fits_limits(&res, &lim) { " OVER" } else { "" };
        eprintln!(
            " {:>4} | {:>12} {:>5.1}% | {:>5} {:>5.1}% | {:>5} {:>5.1}% | {:>7} {:>5.1}% | {:>7} {:>5.1}% | {:>7} {:>5.1}% | {}{}",
            n,
            res.instructions, pct(res.instructions, lim.instructions),
            res.write_entries, pct(res.write_entries as i64, lim.write_entries as i64),
            footprint, pct(footprint as i64, lim.ledger_entries as i64),
            res.contract_events_size_bytes, pct(res.contract_events_size_bytes as i64, lim.contract_events_size_bytes as i64),
            res.write_bytes, pct(res.write_bytes as i64, lim.write_bytes as i64),
            res.disk_read_bytes, pct(res.disk_read_bytes as i64, lim.disk_read_bytes as i64),
            bname, tag,
        );
    }

    // ── Phase 2: Binary search for exact max N ─────────────────────────
    eprintln!("\n{:=<120}", "");
    eprintln!("  BINARY SEARCH: finding exact max N where all resources fit within live limits");
    eprintln!("{:=<120}", "");

    let mut lo: u32 = 1;
    let mut hi: u32 = 199;

    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let res = measure(mid);
        let ok = fits_limits(&res, &lim);
        let (_, bpct) = binding_name(&res, &lim);
        eprintln!(
            "  N={:>3}: binding={:.1}% → {}",
            mid,
            bpct,
            if ok { "OK" } else { "OVER" }
        );
        if ok {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }

    let max_n = lo;
    eprintln!("\n  ╔═══════════════════════════════════════════════════════╗");
    eprintln!(  "  ║  MAX SAC TRANSFERS PER INVOCATION: {:>3}                ║", max_n);
    eprintln!(  "  ╚═══════════════════════════════════════════════════════╝");

    // ── Final: show resource breakdown at max N ────────────────────────
    let res = measure(max_n);
    let footprint = res.disk_read_entries + res.memory_read_entries + res.write_entries;
    let (bname, _) = binding_name(&res, &lim);
    eprintln!("\n  Resources at N={}:", max_n);
    eprintln!("    CPU instructions:     {:>12} / {:>12} ({:.1}%)", res.instructions, lim.instructions, pct(res.instructions, lim.instructions));
    eprintln!("    Memory bytes:         {:>12} / {:>12} ({:.1}%)", res.mem_bytes, lim.mem_bytes as i64, pct(res.mem_bytes, lim.mem_bytes as i64));
    eprintln!("    Write entries:        {:>12} / {:>12} ({:.1}%)", res.write_entries, lim.write_entries, pct(res.write_entries as i64, lim.write_entries as i64));
    eprintln!("    Footprint entries:    {:>12} / {:>12} ({:.1}%)", footprint, lim.ledger_entries, pct(footprint as i64, lim.ledger_entries as i64));
    eprintln!("    Events bytes:         {:>12} / {:>12} ({:.1}%)", res.contract_events_size_bytes, lim.contract_events_size_bytes, pct(res.contract_events_size_bytes as i64, lim.contract_events_size_bytes as i64));
    eprintln!("    Write bytes:          {:>12} / {:>12} ({:.1}%)", res.write_bytes, lim.write_bytes, pct(res.write_bytes as i64, lim.write_bytes as i64));
    eprintln!("    Read bytes (disk):    {:>12} / {:>12} ({:.1}%)", res.disk_read_bytes, lim.disk_read_bytes, pct(res.disk_read_bytes as i64, lim.disk_read_bytes as i64));
    eprintln!("    Binding constraint: {}", bname);
    eprintln!();
    eprintln!("  NOTE: SDK test env may differ from stellar-core enforcement.");
    eprintln!("  Validate on actual testnet with: stellar contract invoke ...");
    eprintln!();

    // Sanity
    assert!(max_n >= 50, "Expected at least 50 transfers to fit, got {}", max_n);
}
