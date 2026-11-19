use oraclelink_runtime as runtime;
use sc_service::ChainType;
use serde_json::json;
use sp_consensus_aura::sr25519::AuthorityId as AuraId;
use sp_consensus_grandpa::AuthorityId as GrandpaId;
use sp_core::{sr25519, Pair, Public};
use sp_runtime::traits::{IdentifyAccount, Verify};

pub type ChainSpec = sc_service::GenericChainSpec;

type AccountPublic = <runtime::Signature as Verify>::Signer;

fn get_from_seed<TPublic: Public>(seed: &str) -> <TPublic::Pair as Pair>::Public {
    TPublic::Pair::from_string(&format!("//{seed}"), None)
        .expect("static values are valid; qed")
        .public()
}

fn get_account_id_from_seed<TPublic: Public>(seed: &str) -> runtime::AccountId
where
    AccountPublic: From<<TPublic::Pair as Pair>::Public>,
{
    AccountPublic::from(get_from_seed::<TPublic>(seed)).into_account()
}

fn authority_keys_from_seed(s: &str) -> (AuraId, GrandpaId) {
    (get_from_seed::<AuraId>(s), get_from_seed::<GrandpaId>(s))
}

fn genesis_patch(
    initial_authorities: Vec<(AuraId, GrandpaId)>,
    endowed: Vec<runtime::AccountId>,
    council_members: Vec<runtime::AccountId>,
) -> serde_json::Value {
    let ten = endowed
        .iter()
        .cloned()
        .map(|k| (k, 1_000_000u128 * runtime::UNIT))
        .collect::<Vec<_>>();
    let occ = endowed
        .iter()
        .cloned()
        .map(|k| (k, 1_000_000u128 * runtime::UNIT))
        .collect::<Vec<_>>();
    json!({
        "balances": { "balances": ten },
        "occ": { "balances": occ },
        "aura": { "authorities": initial_authorities.iter().map(|x| x.0.clone()).collect::<Vec<_>>() },
        "grandpa": { "authorities": initial_authorities.iter().map(|x| (x.1.clone(), 1u64)).collect::<Vec<_>>() },
        "council": { "members": council_members }
    })
}

pub fn development_chain_spec() -> Result<ChainSpec, String> {
    let wasm = runtime::WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?;
    let endowed = vec![
        get_account_id_from_seed::<sr25519::Public>("Alice"),
        get_account_id_from_seed::<sr25519::Public>("Bob"),
        get_account_id_from_seed::<sr25519::Public>("Alice//stash"),
        get_account_id_from_seed::<sr25519::Public>("Bob//stash"),
    ];
    Ok(ChainSpec::builder(wasm, None)
        .with_name("OracleLink Development")
        .with_id("oraclelink-dev")
        .with_chain_type(ChainType::Development)
        .with_genesis_config_patch(genesis_patch(
            vec![authority_keys_from_seed("Alice")],
            endowed.clone(),
            vec![get_account_id_from_seed::<sr25519::Public>("Alice")],
        ))
        .build())
}

pub fn local_chain_spec() -> Result<ChainSpec, String> {
    let wasm = runtime::WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?;
    let endowed = vec![
        get_account_id_from_seed::<sr25519::Public>("Alice"),
        get_account_id_from_seed::<sr25519::Public>("Bob"),
        get_account_id_from_seed::<sr25519::Public>("Charlie"),
        get_account_id_from_seed::<sr25519::Public>("Dave"),
        get_account_id_from_seed::<sr25519::Public>("Eve"),
        get_account_id_from_seed::<sr25519::Public>("Ferdie"),
    ];
    Ok(ChainSpec::builder(wasm, None)
        .with_name("OracleLink Local Testnet")
        .with_id("oraclelink-local")
        .with_chain_type(ChainType::Local)
        .with_genesis_config_patch(genesis_patch(
            vec![
                authority_keys_from_seed("Alice"),
                authority_keys_from_seed("Bob"),
            ],
            endowed,
            vec![
                get_account_id_from_seed::<sr25519::Public>("Alice"),
                get_account_id_from_seed::<sr25519::Public>("Bob"),
            ],
        ))
        .build())
}
