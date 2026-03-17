#![no_std]
use soroban_sdk::{contract, contractimpl, token, vec, Address, Env, MuxedAddress, Vec};

#[contract]
pub struct BatchTransferContract;

#[contractimpl]
impl BatchTransferContract {
    pub fn batch_transfer(
        env: Env,
        sender: Address,
        token: Address,
        receivers: Vec<(MuxedAddress, i128)>,
    ) -> Vec<bool> {
        sender.require_auth();

        let client = token::TokenClient::new(&env, &token);
        let mut results = vec![&env];

        for (to, amount) in receivers.iter() {
            let result = client.try_transfer(&sender, &to, &amount);
            results.push_back(result.is_ok());
        }

        results
    }
}

mod test;
