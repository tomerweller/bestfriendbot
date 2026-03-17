# BestFriendBot

A token faucet for Soroban/Stellar that batches funding requests into a single transaction using a `batch_transfer` smart contract, served through an HTTP API.

## Architecture

```
HTTP Request /?addr=G...
  → Validate address
  → Enqueue (with dedup)
  → Wait for batch
  → batch_transfer contract invocation
  → Per-recipient success/failure response
```

Incoming funding requests are queued and drained every 5 seconds. The batch processor builds a single Soroban transaction that calls the `batch_transfer` contract, which transfers tokens from a funding account to up to 65 recipients atomically. Individual transfers can fail without affecting the rest of the batch.

## Project Structure

```
├── contracts/
│   └── batch_transfer/     # Soroban contract — transfers tokens to multiple recipients
│       └── src/
│           ├── lib.rs       # Contract: batch_transfer(sender, token, receivers) -> Vec<bool>
│           └── test.rs      # Unit tests + resource profiling (max batch size)
├── faucet/                  # HTTP service
│   └── src/
│       ├── main.rs          # Server startup, health checks, route setup
│       ├── config.rs        # Environment variable configuration
│       ├── api.rs           # GET/POST / (fund) and GET /health endpoints
│       ├── queue.rs         # Thread-safe request queue with dedup
│       ├── batch.rs         # Background batch processing loop
│       ├── tx.rs            # Transaction building, signing, submission
│       └── error.rs         # RFC 7807 problem detail error responses
├── test_faucet.sh           # End-to-end integration test on testnet
└── submit_every_ledger.sh   # Ledger timing measurement utility
```

## Configuration

| Variable | Required | Description |
|---|---|---|
| `FUNDING_SECRET_KEY` | Yes | Funder's Stellar secret key (`S...`) |
| `TOKEN_ADDRESS` | Yes | SAC token contract address (`C...`) |
| `CONTRACT_ADDRESS` | Yes | Deployed `batch_transfer` contract address (`C...`) |
| `AMOUNT` | Yes | Token amount per recipient (in stroops) |
| `RPC_URL` | Yes | Soroban RPC endpoint |
| `NETWORK_PASSPHRASE` | Yes | e.g. `"Test SDF Network ; September 2015"` |
| `PORT` | No | HTTP port (default: `8000`) |
| `MAX_BATCH_SIZE` | No | Max recipients per batch (default: `65`) |
| `RUST_LOG` | No | Log filter (default: `faucet=info`) |

## Build

```bash
# Build the contract
stellar contract build

# Build the faucet service
cargo build -p faucet --release
```

## Deploy

Deploy the contract and set up the token (SAC) using the Stellar CLI, then start the faucet:

```bash
export FUNDING_SECRET_KEY="S..."
export TOKEN_ADDRESS="C..."
export CONTRACT_ADDRESS="C..."
export AMOUNT=10000000
export RPC_URL="https://soroban-testnet.stellar.org"
export NETWORK_PASSPHRASE="Test SDF Network ; September 2015"

./target/release/faucet
```

On startup the faucet verifies the contract is deployed and the funding account exists before accepting requests.

## API

### `GET/POST /?addr=G...`

Request tokens for a Stellar address. The request blocks until the next batch is processed and returns:

```json
{ "tx_hash": "abc123..." }
```

Errors use [RFC 7807](https://datatracker.ietf.org/doc/html/rfc7807) problem detail format:
- **400** — Invalid address or transfer failed
- **409** — Address already pending in queue
- **503** — Queue full

### `GET /health`

Returns service status, queue size, contract/token addresses, and batch configuration.

## Testing

Run the full integration test on testnet (requires `stellar`, `cargo`, `curl`, `jq`):

```bash
./test_faucet.sh
```

This creates accounts, deploys the contract, sets up a test token, starts the faucet, sends concurrent funding requests, and verifies recipient balances.
