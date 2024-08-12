use ethers_core::abi::{Contract, FunctionExt, Token};
use ic_cdk::api::management_canister::http_request::{
    http_request, CanisterHttpRequestArgument, HttpHeader, HttpMethod, HttpResponse, TransformArgs,
    TransformContext,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

use crate::util::{from_hex, to_hex};

// Constants for HTTP call configuration
const CYCLES_COST: u128 = 100_000_000;
const MAX_BYTES: u64 = 2048;

// Structs to define JSON-RPC requests and responses
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RpcRequest {
    request_id: u64,
    version: String,
    action: String,
    parameters: (EthCallData, String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct EthCallData {
    recipient: String,
    payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RpcResponse {
    outcome: Option<String>,
    rpc_error: Option<RpcErrorDetail>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RpcErrorDetail {
    error_code: isize,
    error_message: String,
}

// Macro to include ABI JSON files
#[macro_export]
macro_rules! load_abi {
    ($file:expr $(,)?) => {{
        match serde_json::from_str::<ethers_core::abi::Contract>(include_str!($file)) {
            Ok(contract) => contract,
            Err(err) => panic!("Error loading ABI from {:?}: {}", $file, err),
        }
    }};
}

// Generate a unique ID for requests
fn generate_request_id() -> u64 {
    thread_local! {
        static REQUEST_ID: RefCell<u64> = RefCell::default();
    }
    REQUEST_ID.with(|id| {
        let mut id = id.borrow_mut();
        let current_id = *id;
        *id = id.wrapping_add(1);
        current_id
    })
}

// Function to get the RPC endpoint URL based on network name
fn determine_rpc_url(network: &str) -> &'static str {
    match network {
        "mainnet" | "ethereum" => "https://cloudflare-eth.com/v1/mainnet",
        "goerli" => "https://ethereum-goerli.publicnode.com",
        "sepolia" => "https://rpc.sepolia.org",
        _ => panic!("Unsupported network: {}", network),
    }
}

/// Perform a call to an Ethereum smart contract
pub async fn execute_contract_call(
    network: &str,
    address: String,
    contract_abi: &Contract,
    method_name: &str,
    arguments: &[Token],
) -> Vec<Token> {
    // Find the function to call from the ABI
    let function = match contract_abi.functions_by_name(method_name).map(|v| &v[..]) {
        Ok([func]) => func,
        Ok(overloads) => panic!(
            "Found {} function overloads. Use one of: {}",
            overloads.len(),
            overloads
                .iter()
                .map(|func| format!("{:?}", func.abi_signature()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(_) => contract_abi
            .functions()
            .find(|func| method_name == func.abi_signature())
            .expect("Function not found"),
    };
    let encoded_data = function
        .encode_input(arguments)
        .expect("Error encoding input arguments");

    // Prepare JSON-RPC payload
    let rpc_payload = serde_json::to_string(&RpcRequest {
        request_id: generate_request_id(),
        version: "2.0".to_string(),
        action: "eth_call".to_string(),
        parameters: (
            EthCallData {
                recipient: address,
                payload: to_hex(&encoded_data),
            },
            "latest".to_string(),
        ),
    })
    .expect("Error encoding JSON-RPC request");

    // Parse service URL and set headers
    let rpc_url = determine_rpc_url(network).to_string();
    let url_parts = url::Url::parse(&rpc_url).expect("Error parsing service URL");
    let host_header = url_parts.host_str().expect("Invalid service URL host");

    let headers = vec![
        HttpHeader {
            name: "Content-Type".to_string(),
            value: "application/json".to_string(),
        },
        HttpHeader {
            name: "Host".to_string(),
            value: host_header.to_string(),
        },
    ];

    // Prepare the HTTP request
    let http_request_data = CanisterHttpRequestArgument {
        url: rpc_url,
        max_response_bytes: Some(MAX_BYTES),
        method: HttpMethod::POST,
        headers,
        body: Some(rpc_payload.as_bytes().to_vec()),
        transform: Some(TransformContext::from_name(
            "handle_transform".to_string(),
            vec![],
        )),
    };

    // Perform the HTTP request
    let response = match http_request(http_request_data, CYCLES_COST).await {
        Ok((res,)) => res,
        Err((res, msg)) => panic!("{:?} {:?}", res, msg),
    };

    // Decode the JSON-RPC response
    let rpc_result: RpcResponse =
        serde_json::from_str(std::str::from_utf8(&response.body).expect("Invalid UTF-8"))
            .expect("Malformed JSON response");
    if let Some(err) = rpc_result.rpc_error {
        panic!(
            "JSON-RPC error code {}: {}",
            err.error_code, err.error_message
        );
    }
    let decoded_result = from_hex(&rpc_result.outcome.expect("Unexpected JSON response")).unwrap();
    function
        .decode_output(&decoded_result)
        .expect("Error decoding output")
}

#[ic_cdk_macros::query(name = "handle_transform")]
pub fn handle_transform(args: TransformArgs) -> HttpResponse {
    HttpResponse {
        status: args.response.status.clone(),
        body: args.response.body,
        // Remove headers that can differ and affect consensus
        headers: Vec::new(),
    }
}
