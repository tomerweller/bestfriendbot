#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, vec, Address, Env, IntoVal, InvokeError, Symbol, Val,
    Vec,
};

#[contracttype]
#[derive(Clone, Debug)]
pub struct Receiver {
    pub address: Address,
    pub amount: i128,
}

#[contract]
pub struct BatchTransferContract;

#[contractimpl]
impl BatchTransferContract {
    pub fn batch_transfer(
        env: Env,
        sender: Address,
        token: Address,
        receivers: Vec<Receiver>,
    ) -> Vec<bool> {
        sender.require_auth();

        let transfer_fn = Symbol::new(&env, "transfer");
        let mut results = vec![&env];

        for receiver in receivers.iter() {
            let args: Vec<Val> = vec![
                &env,
                sender.to_val(),
                receiver.address.to_val(),
                receiver.amount.into_val(&env),
            ];
            let result =
                env.try_invoke_contract::<(), InvokeError>(&token, &transfer_fn, args);
            results.push_back(result.is_ok());
        }

        results
    }
}

mod test;
