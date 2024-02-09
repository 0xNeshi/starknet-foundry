use crate::runtime_extensions::call_to_blockifier_runtime_extension::rpc::{
    call_l1_handler, CallResult,
};
use crate::runtime_extensions::call_to_blockifier_runtime_extension::RuntimeState;
use blockifier::abi::abi_utils::starknet_keccak;
use blockifier::execution::syscalls::hint_processor::SyscallHintProcessor;
use cairo_felt::Felt252;
use starknet_api::core::ContractAddress;

pub fn l1_handler_execute(
    syscall_handler: &mut SyscallHintProcessor,
    runtime_state: &mut RuntimeState,
    contract_address: ContractAddress,
    function_name: &Felt252,
    from_address: &Felt252,
    payload: &[Felt252],
) -> CallResult {
    let selector = starknet_keccak(&function_name.to_bytes_be());

    let mut calldata = vec![from_address.clone()];
    calldata.extend_from_slice(payload);

    call_l1_handler(
        syscall_handler,
        runtime_state,
        &contract_address,
        &selector,
        calldata.as_slice(),
    )
}
