use crate::utils::*;
use ethjson::spec::ForkSpec;
use evm::backend::{ApplyBackend, MemoryAccount, MemoryBackend, MemoryVicinity};
use evm::executor::{
	self, MemoryStackState, PrecompileFailure, PrecompileOutput, StackExecutor,
	StackSubstateMetadata,
};
use evm::{Config, Context, ExitError, ExitSucceed};
use lazy_static::lazy_static;
use parity_crypto::publickey;
use primitive_types::{H160, H256, U256};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::convert::TryInto;

#[derive(Deserialize, Debug)]
pub struct Test(ethjson::test_helpers::state::State);

impl Test {
	pub fn unwrap_to_pre_state(&self) -> BTreeMap<H160, MemoryAccount> {
		unwrap_to_state(&self.0.pre_state)
	}

	pub fn unwrap_caller(&self) -> H160 {
		let secret_key: H256 = self.0.transaction.secret.clone().unwrap().into();
		let secret = publickey::Secret::import_key(&secret_key[..]).unwrap();
		let public = publickey::KeyPair::from_secret(secret)
			.unwrap()
			.public()
			.clone();
		let sender = publickey::public_to_address(&public);

		sender
	}

	pub fn unwrap_to_vicinity(&self) -> MemoryVicinity {
		MemoryVicinity {
			gas_price: self.0.transaction.gas_price.clone().into(),
			origin: self.unwrap_caller(),
			block_hashes: Vec::new(),
			block_number: self.0.env.number.clone().into(),
			block_coinbase: self.0.env.author.clone().into(),
			block_timestamp: self.0.env.timestamp.clone().into(),
			block_difficulty: self.0.env.difficulty.clone().into(),
			block_gas_limit: self.0.env.gas_limit.clone().into(),
			chain_id: U256::one(),
		}
	}
}

lazy_static! {
	static ref ISTANBUL_BUILTINS: BTreeMap<H160, ethcore_builtin::Builtin> =
		JsonPrecompile::builtins("./res/istanbul_builtins.json");
}

lazy_static! {
	static ref BERLIN_BUILTINS: BTreeMap<H160, ethcore_builtin::Builtin> =
		JsonPrecompile::builtins("./res/berlin_builtins.json");
}

macro_rules! precompile_entry {
	($map:expr, $builtins:expr, $index:expr) => {
		let x: fn(
			&[u8],
			Option<u64>,
			&Context,
			bool,
		) -> Result<PrecompileOutput, PrecompileFailure> =
			|input: &[u8], gas_limit: Option<u64>, _context: &Context, _is_static: bool| {
				let builtin = $builtins.get(&H160::from_low_u64_be($index)).unwrap();
				Self::exec_as_precompile(builtin, input, gas_limit)
			};
		$map.insert(H160::from_low_u64_be($index), x);
	};
}

pub struct JsonPrecompile;

impl JsonPrecompile {
	pub fn precompile(spec: &ForkSpec) -> Option<BTreeMap<H160, executor::PrecompileFn>> {
		match spec {
			ForkSpec::Istanbul => {
				let mut map = BTreeMap::new();
				precompile_entry!(map, ISTANBUL_BUILTINS, 1);
				precompile_entry!(map, ISTANBUL_BUILTINS, 2);
				precompile_entry!(map, ISTANBUL_BUILTINS, 3);
				precompile_entry!(map, ISTANBUL_BUILTINS, 4);
				precompile_entry!(map, ISTANBUL_BUILTINS, 5);
				precompile_entry!(map, ISTANBUL_BUILTINS, 6);
				precompile_entry!(map, ISTANBUL_BUILTINS, 7);
				precompile_entry!(map, ISTANBUL_BUILTINS, 8);
				precompile_entry!(map, ISTANBUL_BUILTINS, 9);
				Some(map)
			}
			ForkSpec::Berlin => {
				let mut map = BTreeMap::new();
				precompile_entry!(map, BERLIN_BUILTINS, 1);
				precompile_entry!(map, BERLIN_BUILTINS, 2);
				precompile_entry!(map, BERLIN_BUILTINS, 3);
				precompile_entry!(map, BERLIN_BUILTINS, 4);
				precompile_entry!(map, BERLIN_BUILTINS, 5);
				precompile_entry!(map, BERLIN_BUILTINS, 6);
				precompile_entry!(map, BERLIN_BUILTINS, 7);
				precompile_entry!(map, BERLIN_BUILTINS, 8);
				precompile_entry!(map, BERLIN_BUILTINS, 9);
				Some(map)
			}
			_ => None,
		}
	}

	fn builtins(spec_path: &str) -> BTreeMap<H160, ethcore_builtin::Builtin> {
		let reader = std::fs::File::open(spec_path).unwrap();
		let builtins: BTreeMap<ethjson::hash::Address, ethjson::spec::builtin::BuiltinCompat> =
			serde_json::from_reader(reader).unwrap();
		builtins
			.into_iter()
			.map(|(address, builtin)| {
				(
					address.into(),
					ethjson::spec::Builtin::from(builtin).try_into().unwrap(),
				)
			})
			.collect()
	}

	fn exec_as_precompile(
		builtin: &ethcore_builtin::Builtin,
		input: &[u8],
		gas_limit: Option<u64>,
	) -> Result<PrecompileOutput, PrecompileFailure> {
		let cost = builtin.cost(input, 0);

		if let Some(target_gas) = gas_limit {
			if cost > U256::from(u64::MAX) || target_gas < cost.as_u64() {
				return Err(PrecompileFailure::Error {
					exit_status: ExitError::OutOfGas,
				});
			}
		}

		let mut output = Vec::new();
		match builtin.execute(input, &mut parity_bytes::BytesRef::Flexible(&mut output)) {
			Ok(()) => Ok(PrecompileOutput {
				exit_status: ExitSucceed::Stopped,
				output,
				cost: cost.as_u64(),
				logs: Vec::new(),
			}),
			Err(e) => Err(PrecompileFailure::Error {
				exit_status: ExitError::Other(e.into()),
			}),
		}
	}
}

pub fn test(name: &str, test: Test) {
	use std::thread;

	const STACK_SIZE: usize = 16 * 1024 * 1024;

	let name = name.to_string();
	// Spawn thread with explicit stack size
	let child = thread::Builder::new()
		.stack_size(STACK_SIZE)
		.spawn(move || test_run(&name, test))
		.unwrap();

	// Wait for thread to join
	child.join().unwrap();
}

fn test_run(name: &str, test: Test) {
	for (spec, states) in &test.0.post_states {
		let (gasometer_config, delete_empty) = match spec {
			ethjson::spec::ForkSpec::Istanbul => (Config::istanbul(), true),
			ethjson::spec::ForkSpec::Berlin => (Config::berlin(), true),
			spec => {
				println!("Skip spec {:?}", spec);
				continue;
			}
		};

		let original_state = test.unwrap_to_pre_state();
		let vicinity = test.unwrap_to_vicinity();
		let caller = test.unwrap_caller();

		for (i, state) in states.iter().enumerate() {
			print!("Running {}:{:?}:{} ... ", name, spec, i);
			flush();

			let transaction = test.0.transaction.select(&state.indexes);
			let gas_limit: u64 = transaction.gas_limit.into();
			let data: Vec<u8> = transaction.data.into();

			let mut backend = MemoryBackend::new(&vicinity, original_state.clone());
			let metadata =
				StackSubstateMetadata::new(transaction.gas_limit.into(), &gasometer_config);
			let executor_state = MemoryStackState::new(metadata, &backend);
			let precompile = JsonPrecompile::precompile(spec).unwrap();
			let mut executor =
				StackExecutor::new_with_precompiles(executor_state, &gasometer_config, &precompile);
			let total_fee = vicinity.gas_price * gas_limit;

			executor.state_mut().withdraw(caller, total_fee).unwrap();

			let access_list = transaction
				.access_list
				.into_iter()
				.map(|(address, keys)| (address.0, keys.into_iter().map(|k| k.0).collect()))
				.collect();

			match transaction.to {
				ethjson::maybe::MaybeEmpty::Some(to) => {
					let data = data;
					let value = transaction.value.into();

					let _reason = executor.transact_call(
						caller,
						to.into(),
						value,
						data,
						gas_limit,
						access_list,
					);
				}
				ethjson::maybe::MaybeEmpty::None => {
					let code = data;
					let value = transaction.value.into();

					let _reason =
						executor.transact_create(caller, value, code, gas_limit, access_list);
				}
			}

			let actual_fee = executor.fee(vicinity.gas_price);
			executor
				.state_mut()
				.deposit(vicinity.block_coinbase, actual_fee);
			executor.state_mut().deposit(caller, total_fee - actual_fee);
			let (values, logs) = executor.into_state().deconstruct();
			backend.apply(values, logs, delete_empty);
			assert_valid_hash(&state.hash.0, backend.state());

			println!("passed");
		}
	}
}
