use ed25519_dalek::SigningKey;
use stellar_strkey::ed25519::PublicKey;

pub struct Config {
    pub funding_secret: SigningKey,
    pub funding_public: String,
    pub token_address: String,
    pub contract_address: String,
    pub amount: i128,
    pub max_batch_size: usize,
    pub rpc_url: String,
    pub network_passphrase: String,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Self {
        let secret_str =
            std::env::var("FUNDING_SECRET_KEY").expect("FUNDING_SECRET_KEY must be set");
        let secret_bytes = stellar_strkey::ed25519::PrivateKey::from_string(&secret_str)
            .expect("Invalid FUNDING_SECRET_KEY")
            .0;
        let signing_key = SigningKey::from_bytes(&secret_bytes);
        let verifying_key = signing_key.verifying_key();
        let funding_public = PublicKey(verifying_key.to_bytes()).to_string();

        let token_address =
            std::env::var("TOKEN_ADDRESS").expect("TOKEN_ADDRESS must be set");
        let contract_address =
            std::env::var("CONTRACT_ADDRESS").expect("CONTRACT_ADDRESS must be set");
        let amount: i128 = std::env::var("AMOUNT")
            .expect("AMOUNT must be set")
            .parse()
            .expect("AMOUNT must be a valid i128");
        let max_batch_size: usize = std::env::var("MAX_BATCH_SIZE")
            .unwrap_or_else(|_| "65".into())
            .parse()
            .expect("MAX_BATCH_SIZE must be a valid usize");
        let rpc_url = std::env::var("RPC_URL").expect("RPC_URL must be set");
        let network_passphrase =
            std::env::var("NETWORK_PASSPHRASE").expect("NETWORK_PASSPHRASE must be set");
        let port: u16 = std::env::var("PORT")
            .unwrap_or_else(|_| "8000".into())
            .parse()
            .expect("PORT must be a valid u16");

        // Validate addresses
        stellar_strkey::Contract::from_string(&token_address)
            .expect("TOKEN_ADDRESS must be a valid C... address");
        stellar_strkey::Contract::from_string(&contract_address)
            .expect("CONTRACT_ADDRESS must be a valid C... address");

        Config {
            funding_secret: signing_key,
            funding_public,
            token_address,
            contract_address,
            amount,
            max_batch_size,
            rpc_url,
            network_passphrase,
            port,
        }
    }
}
